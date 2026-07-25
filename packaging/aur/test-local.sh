#!/usr/bin/env bash
# Builds the PKGBUILD next to this script in a throwaway Arch container, lints it
# with namcap, installs the package and smoke-tests the binary. Run it after every
# bump before pushing to the AUR. Needs docker; re-execs itself inside the
# container when run from a non-Arch host.
set -euo pipefail

if [[ ! -f /etc/arch-release ]]; then
	here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
	exec docker run --rm --platform linux/amd64 -v "$here":/work \
		archlinux:base-devel bash /work/test-local.sh
fi

echo "### 1. deps"
# pacman's sandbox cannot init inside docker on a non-native host: seccomp(2)
# returns EINVAL under qemu-emulated amd64 and Landlock is blocked on arm64.
# Has to land in [options]; appending to the end of the file puts it inside the
# last repo section, where it is silently ignored.
sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf
pacman -Syu --noconfirm --needed base-devel namcap sudo >/dev/null 2>&1

useradd -m builder
mkdir -p /build
cp /work/PKGBUILD /build/
chown -R builder:builder /build

echo "### 2. build"
su builder -c 'cd /build && makepkg -f --noconfirm'

echo "### 3. .SRCINFO matches the committed one"
su builder -c 'cd /build && makepkg --printsrcinfo > .SRCINFO'
diff /work/.SRCINFO /build/.SRCINFO && echo "ok, .SRCINFO is up to date"

echo "### 4. namcap"
namcap /build/PKGBUILD || true
namcap /build/*.pkg.tar.[xz]* || true

echo "### 5. install and smoke test"
pacman -U --noconfirm /build/*.pkg.tar.[xz]*
pacman -Ql alphai-tui-bin
alphai-tui --version
alphai-tui --once AAPL

echo "### DONE"
