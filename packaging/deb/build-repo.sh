#!/usr/bin/env bash
# Turns released linux tarballs into .deb packages and folds them into a signed
# apt repository tree. Pure local work: no network, no git, no gh. The tree it
# produces is what GitHub Pages serves; publish.sh does the moving of files.
#
# Needs a Debian-ish environment (dpkg-deb, apt-ftparchive, gpg). On a host
# without them, publish.sh runs this script inside a container instead.
#
#   build-repo.sh --version 0.10.2 --repo ./tree \
#                 --amd64 alphai-tui-x86_64-unknown-linux-gnu.tar.xz \
#                 --arm64 alphai-tui-aarch64-unknown-linux-gnu.tar.xz \
#                 --pubkey alphai-tui.gpg [--debs-out DIR] [--keep 3]
#
# The signing key comes from $APT_GPG_PRIVATE_KEY (armored private key). Without
# it the script signs with whatever key the ambient gpg agent offers, which is
# only useful for experiments: the final gpgv check still has to pass against
# --pubkey, so a mismatched key fails here rather than in users' apt.
set -euo pipefail

PACKAGE=alphai-tui
MAINTAINER='Mikhail Makeev <mihail.makeev@gmail.com>'
HOMEPAGE=https://github.com/makeev/alphai-tui
SUITE=stable
COMPONENT=main
# Binaries are built on ubuntu-22.04 runners with no container, so glibc 2.35 is
# the floor. Saying so lets apt refuse the install on older releases instead of
# letting the binary die at exec time.
DEPENDS='libc6 (>= 2.35)'
KEEP=3

version= repo= amd64= arm64= pubkey= debs_out=
while [ $# -gt 0 ]; do
  case $1 in
    --version)  version=$2;  shift 2 ;;
    --repo)     repo=$2;     shift 2 ;;
    --amd64)    amd64=$2;    shift 2 ;;
    --arm64)    arm64=$2;    shift 2 ;;
    --pubkey)   pubkey=$2;   shift 2 ;;
    --debs-out) debs_out=$2; shift 2 ;;
    --keep)     KEEP=$2;     shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

for required in version repo amd64 arm64 pubkey; do
  if [ -z "${!required}" ]; then
    echo "missing --$required" >&2
    exit 2
  fi
done
case $version in
  [0-9]*) ;;
  *) echo "version must start with a digit, got '$version'" >&2; exit 2 ;;
esac

for tool in dpkg-deb apt-ftparchive gpg gpgv; do
  command -v "$tool" > /dev/null || { echo "$tool is not installed" >&2; exit 1; }
done

# Everything below runs from inside the repository tree, so relative arguments
# have to be pinned down while the original working directory still applies.
abspath() { printf '%s/%s\n' "$(cd "$(dirname "$1")" && pwd)" "$(basename "$1")"; }
mkdir -p "$repo"
repo=$(cd "$repo" && pwd)
amd64=$(abspath "$amd64")
arm64=$(abspath "$arm64")
pubkey=$(abspath "$pubkey")
[ -z "$debs_out" ] || { mkdir -p "$debs_out"; debs_out=$(cd "$debs_out" && pwd); }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Build one .deb out of one release tarball.
build_deb() {
  arch=$1 tarball=$2
  tree=$work/$arch
  mkdir -p "$tree/DEBIAN" "$tree/usr/bin" "$tree/usr/share/doc/$PACKAGE"

  # Unpack per architecture: both tarballs hold a binary under the same name,
  # and a shared scratch directory would let one arch pick up the other's.
  ext=$work/unpacked-$arch
  mkdir -p "$ext"
  tar -xf "$tarball" -C "$ext"
  src=$(find "$ext" -maxdepth 2 -type f -name "$PACKAGE" -perm -u+x | head -1)
  [ -n "$src" ] || { echo "no $PACKAGE binary inside $tarball" >&2; exit 1; }
  install -m 0755 "$src" "$tree/usr/bin/$PACKAGE"

  srcdir=$(dirname "$src")
  # DEP-5 copyright, with the upstream licence text pulled straight from the
  # tarball so the package can never disagree with what shipped.
  {
    echo 'Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/'
    echo "Upstream-Name: $PACKAGE"
    echo "Source: $HOMEPAGE"
    echo
    echo 'Files: *'
    echo 'Copyright: Mikhail Makeev'
    echo 'License: MIT'
    sed 's/^$/./; s/^/ /' "$srcdir/LICENSE"
  } > "$tree/usr/share/doc/$PACKAGE/copyright"
  gzip -9nc "$srcdir/README.md" > "$tree/usr/share/doc/$PACKAGE/README.md.gz"

  {
    echo "$PACKAGE ($version) $SUITE; urgency=medium"
    echo
    echo "  * Release $version. See $HOMEPAGE/releases/tag/v$version"
    echo
    echo " -- $MAINTAINER  $(date -R -u)"
  } | gzip -9nc > "$tree/usr/share/doc/$PACKAGE/changelog.Debian.gz"

  installed_size=$(du -sk "$tree/usr" | cut -f1)
  cat > "$tree/DEBIAN/control" <<EOF
Package: $PACKAGE
Version: $version
Architecture: $arch
Maintainer: $MAINTAINER
Installed-Size: $installed_size
Depends: $DEPENDS
Section: utils
Priority: optional
Homepage: $HOMEPAGE
Description: terminal stock dashboard with news and insider activity
 Live quotes and interactive charts in the terminal, next to AI scored
 financial news and SEC Form 4 insider transactions from the AlphaAI API.
 .
 Quotes and charts need no account at all (Yahoo by default, Finnhub and
 Alpaca optional). The news, sentiment and insider views read a free
 AlphaAI key from the config file or the environment.
EOF

  ( cd "$tree" && find usr -type f -exec md5sum {} + > DEBIAN/md5sums )

  out=$work/${PACKAGE}_${version}_${arch}.deb
  dpkg-deb --build --root-owner-group "$tree" "$out" > /dev/null
  echo "$out"
}

pool=$repo/pool/$COMPONENT/${PACKAGE:0:1}/$PACKAGE
mkdir -p "$pool"

for pair in "amd64:$amd64" "arm64:$arm64"; do
  arch=${pair%%:*} tarball=${pair#*:}
  deb=$(build_deb "$arch" "$tarball")
  install -m 0644 "$deb" "$pool/"
  [ -z "$debs_out" ] || { mkdir -p "$debs_out"; install -m 0644 "$deb" "$debs_out/"; }
  echo "built $(basename "$deb")"
done

# Keep the pool bounded: apt only ever installs the newest, older versions are
# there for pinning and for the odd downgrade.
mapfile -t keep < <(
  ls "$pool" | sed -n "s/^${PACKAGE}_\(.*\)_[^_]*\.deb$/\1/p" | sort -uV | tail -n "$KEEP"
)
for deb in "$pool"/*.deb; do
  have=$(basename "$deb" | sed -n "s/^${PACKAGE}_\(.*\)_[^_]*\.deb$/\1/p")
  printf '%s\n' "${keep[@]}" | grep -qxF "$have" || { rm "$deb"; echo "pruned $(basename "$deb")"; }
done

# Indices. apt-ftparchive writes Filename: paths relative to the working
# directory, so it has to run from the repository root.
cd "$repo"
rm -rf dists
for arch in amd64 arm64; do
  dir=dists/$SUITE/$COMPONENT/binary-$arch
  mkdir -p "$dir"
  apt-ftparchive --arch "$arch" packages pool > "$dir/Packages"
  gzip -9nkf "$dir/Packages"
done

cat > "$work/release.conf" <<EOF
APT::FTPArchive::Release::Origin "$PACKAGE";
APT::FTPArchive::Release::Label "$PACKAGE";
APT::FTPArchive::Release::Suite "$SUITE";
APT::FTPArchive::Release::Codename "$SUITE";
APT::FTPArchive::Release::Architectures "amd64 arm64";
APT::FTPArchive::Release::Components "$COMPONENT";
APT::FTPArchive::Release::Description "alphai-tui, terminal stock dashboard";
EOF
apt-ftparchive -c "$work/release.conf" release "dists/$SUITE" > "$work/Release"
mv "$work/Release" "dists/$SUITE/Release"

export GNUPGHOME=$work/gnupg
mkdir -m 700 "$GNUPGHOME"
sign_as=()
if [ -n "${APT_GPG_PRIVATE_KEY:-}" ]; then
  printf '%s\n' "$APT_GPG_PRIVATE_KEY" | gpg --batch --quiet --import
  sign_as=(--local-user "$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr:/ {print $10; exit}')")
else
  unset GNUPGHOME
  echo "APT_GPG_PRIVATE_KEY is unset, signing with the ambient gpg key" >&2
fi

gpg --batch --yes "${sign_as[@]}" --clearsign -o "dists/$SUITE/InRelease" "dists/$SUITE/Release"
gpg --batch --yes "${sign_as[@]}" --detach-sign --armor -o "dists/$SUITE/Release.gpg" "dists/$SUITE/Release"

# Publish the public half next to the repository and, more to the point, prove
# the signatures verify against exactly that key. A CI secret that has drifted
# from the committed key dies here instead of breaking apt update for everyone.
install -m 0644 "$pubkey" "$repo/$PACKAGE.gpg"
gpgv --keyring "$pubkey" "dists/$SUITE/Release.gpg" "dists/$SUITE/Release"
gpgv --keyring "$pubkey" "dists/$SUITE/InRelease"

echo "repository written to $repo"
