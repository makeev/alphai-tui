#!/usr/bin/env bash
# End to end check of the apt repository: add the source the way the README
# tells users to, then apt update, apt install and run the binary. Runs against
# a locally built tree (--repo, served over file:) or against the published one
# (--url), on several images and on both architectures.
#
#   packaging/deb/test-local.sh --repo ./tree [--version 0.10.2]
#   packaging/deb/test-local.sh --url https://makeev.github.io/alphai-tui-apt
#
# amd64 images run under emulation on an arm64 host and vice versa, which is the
# point: it is the only way to see the other architecture's .deb install.
set -euo pipefail

url= repo= version=
targets=("linux/amd64 ubuntu:22.04" "linux/arm64 ubuntu:24.04" "linux/amd64 debian:12")
while [ $# -gt 0 ]; do
  case $1 in
    --url)     url=$2;     shift 2 ;;
    --repo)    repo=$2;    shift 2 ;;
    --version) version=$2; shift 2 ;;
    --target)  targets=("$2"); shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -n "$url" ] && [ -n "$repo" ]; then
  echo "pass either --url or --repo, not both" >&2
  exit 2
fi
if [ -z "$url" ] && [ -z "$repo" ]; then
  echo "pass --repo DIR (local tree) or --url URL (published repository)" >&2
  exit 2
fi

mounts=()
if [ -n "$repo" ]; then
  repo=$(cd "$repo" && pwd)
  [ -f "$repo/alphai-tui.gpg" ] || { echo "$repo does not look like an apt repository" >&2; exit 1; }
  mounts=(-v "$repo:/repo:ro")
  source_url=file:/repo
  fetch_key='cp /repo/alphai-tui.gpg /etc/apt/keyrings/alphai-tui.gpg'
  extra_packages=
else
  source_url=$url
  fetch_key="curl -fsSL $url/alphai-tui.gpg -o /etc/apt/keyrings/alphai-tui.gpg"
  extra_packages='ca-certificates curl'
fi

failed=0
for target in "${targets[@]}"; do
  platform=${target%% *} image=${target#* }
  echo
  echo "=== $image on $platform ==="
  if docker run --rm --platform "$platform" "${mounts[@]}" \
      -e DEBIAN_FRONTEND=noninteractive -e EXPECT_VERSION="$version" "$image" bash -c "
set -e
apt-get -qq update > /dev/null
[ -z '$extra_packages' ] || apt-get -qq install -y $extra_packages > /dev/null
install -d -m 0755 /etc/apt/keyrings
$fetch_key
echo 'deb [signed-by=/etc/apt/keyrings/alphai-tui.gpg] $source_url stable main' \
  > /etc/apt/sources.list.d/alphai-tui.list
apt-get -qq update > /dev/null
apt-get -qq install -y alphai-tui > /dev/null
dpkg -s alphai-tui | grep -E '^(Version|Architecture|Depends):'
out=\$(alphai-tui --version)
echo \"\$out\"
if [ -n \"\$EXPECT_VERSION\" ] && [ \"\$out\" != \"alphai-tui \$EXPECT_VERSION\" ]; then
  echo \"expected alphai-tui \$EXPECT_VERSION\" >&2
  exit 1
fi
test -f /usr/share/doc/alphai-tui/copyright
test -f /usr/share/doc/alphai-tui/changelog.Debian.gz
"; then
    echo "ok: $image on $platform"
  else
    echo "FAILED: $image on $platform" >&2
    failed=1
  fi
done

echo
if [ "$failed" -ne 0 ]; then
  echo "some targets failed" >&2
  exit 1
fi
echo "all targets installed and ran alphai-tui"
