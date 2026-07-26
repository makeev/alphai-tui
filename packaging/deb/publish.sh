#!/usr/bin/env bash
# Publishes a released version to the apt repository. Called by CI from
# .github/workflows/publish-deb.yml, and by hand for the initial bootstrap or
# to repair a release: both paths run this same script, so the CI-only surface
# is a few lines of YAML.
#
#   packaging/deb/publish.sh --version 0.10.2
#   packaging/deb/publish.sh --version 0.10.2 --no-push   # build, do not publish
#
# Environment:
#   GH_TOKEN               read the release tarballs
#   APT_GPG_PRIVATE_KEY    armored private key that signs the repository
#   APT_SSH_KEY            deploy key with write access to the apt repository
#
# --upload also attaches the .deb files to the GitHub release. CI does not use
# it: dist hands custom publish jobs a GITHUB_TOKEN without contents write, so
# the standalone download lives in the pool the landing page links to instead.
#
# The apt repository keeps a single commit: every publish force pushes an orphan
# commit, so old .deb blobs fall out of the history instead of accumulating in a
# repository that GitHub Pages re-serves on every release.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SRC_REPO=${SRC_REPO:-makeev/alphai-tui}
APT_REPO=${APT_REPO:-makeev/alphai-tui-apt}

version= tag= push=1 upload=0
while [ $# -gt 0 ]; do
  case $1 in
    --version) version=$2; shift 2 ;;
    --tag)     tag=$2;     shift 2 ;;
    --no-push) push=0;     shift ;;
    --upload)  upload=1;   shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$version" ] || { echo "missing --version" >&2; exit 2; }
tag=${tag:-v$version}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "==> downloading $tag tarballs from $SRC_REPO"
gh release download "$tag" --repo "$SRC_REPO" \
  -p '*unknown-linux-gnu.tar.xz' -D "$work/tarballs" --clobber
amd64=$work/tarballs/alphai-tui-x86_64-unknown-linux-gnu.tar.xz
arm64=$work/tarballs/alphai-tui-aarch64-unknown-linux-gnu.tar.xz
for tarball in "$amd64" "$arm64"; do
  [ -f "$tarball" ] || { echo "release $tag has no $(basename "$tarball")" >&2; exit 1; }
done

# The deploy key doubles as the clone credential, so a run without it can still
# build the tree (--no-push) but never touches the published repository.
if [ -n "${APT_SSH_KEY:-}" ]; then
  install -d -m 700 "$work/ssh"
  printf '%s\n' "$APT_SSH_KEY" > "$work/ssh/key"
  chmod 600 "$work/ssh/key"
  export GIT_SSH_COMMAND="ssh -i $work/ssh/key -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$here/known_hosts"
  remote=ssh://git@github.com/$APT_REPO.git
else
  [ "$push" -eq 0 ] || { echo "APT_SSH_KEY is unset, pass --no-push to build only" >&2; exit 2; }
  remote=https://github.com/$APT_REPO.git
fi

echo "==> cloning $APT_REPO"
tree=$work/tree
git clone --depth 1 "$remote" "$tree" 2>&1 | sed 's/^/    /'

echo "==> building packages and indices"
build_args=(
  --version "$version" --repo "$tree" --debs-out "$work/debs"
  --amd64 "$amd64" --arm64 "$arm64" --pubkey "$here/alphai-tui.gpg"
)
if command -v dpkg-deb > /dev/null && command -v apt-ftparchive > /dev/null; then
  bash "$here/build-repo.sh" "${build_args[@]}"
else
  # No Debian tooling on this host (macOS), so borrow a container for the part
  # that needs it. CI runs on ubuntu and takes the branch above.
  echo "    dpkg-deb not found, running the build in debian:12-slim"
  docker run --rm \
    -e APT_GPG_PRIVATE_KEY -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -v "$here:/pkg:ro" -v "$work:/work" -w /work debian:12-slim bash -c '
      set -e
      apt-get -qq update > /dev/null
      apt-get -qq install -y apt-utils gnupg xz-utils > /dev/null 2>&1
      bash /pkg/build-repo.sh \
        --version "$0" --repo /work/tree --debs-out /work/debs \
        --amd64 /work/tarballs/alphai-tui-x86_64-unknown-linux-gnu.tar.xz \
        --arm64 /work/tarballs/alphai-tui-aarch64-unknown-linux-gnu.tar.xz \
        --pubkey /pkg/alphai-tui.gpg
      chown -R "$HOST_UID:$HOST_GID" /work/tree /work/debs
    ' "$version"
fi

# Landing page for anyone who opens the repository URL in a browser. The
# .nojekyll marker keeps Pages from running the tree through Jekyll, which has
# opinions about which files are worth publishing.
sed "s/@VERSION@/$version/g" "$here/site/index.html" > "$tree/index.html"
cp "$here/site/README.md" "$tree/README.md"
touch "$tree/.nojekyll"

echo "==> committing"
rm -rf "$tree/.git"
git -C "$tree" init -q -b main
git -C "$tree" add -A
git -C "$tree" -c user.name="Mikhail Makeev" -c user.email="mihail.makeev@gmail.com" \
  commit -qm "alphai-tui $version"
git -C "$tree" remote add origin "$remote"

if [ "$push" -eq 1 ]; then
  echo "==> force pushing to $APT_REPO"
  git -C "$tree" push --force origin main
else
  echo "==> --no-push: tree left in $work (not deleted)"
  trap - EXIT
fi

if [ "$upload" -eq 1 ]; then
  echo "==> attaching .deb files to release $tag"
  gh release upload "$tag" "$work"/debs/*.deb --repo "$SRC_REPO" --clobber
fi

echo "done"
