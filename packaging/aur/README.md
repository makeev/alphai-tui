# AUR packaging

`alphai-tui-bin` on the [AUR](https://aur.archlinux.org/packages/alphai-tui-bin):
a binary package that installs the prebuilt `x86_64` or `aarch64` Linux binary
from the matching GitHub release, so Arch users do not have to compile the crate.

```sh
paru -S alphai-tui-bin      # or yay, or makepkg -si
```

## Files

| File | What it is |
|---|---|
| `PKGBUILD` | The recipe. `pkgver` and both `sha256sums_*` are rewritten by CI on every release, so treat those four lines as generated. |
| `.SRCINFO` | Metadata the AUR itself reads. Never edit it by hand: regenerate with `makepkg --printsrcinfo`. |
| `known_hosts` | Pinned AUR host keys, so CI does not accept whatever key answers on first connect. |
| `test-local.sh` | Builds, lints and installs the package in a throwaway Arch container. |

## Releasing

Nothing to do by hand. `publish-jobs` in `dist-workspace.toml` runs
`.github/workflows/publish-aur.yml` after the release artifacts are uploaded; it
bumps the version and both checksums, regenerates `.SRCINFO`, verifies the
sources against the release, and pushes to the AUR. The repo needs the
`AUR_SSH_KEY` secret (private half of a key registered on the AUR account).

The copy of `PKGBUILD` in this directory is the template CI starts from, so its
`pkgver` trails the newest release between releases. That is expected: the AUR
repo is the published artifact, this is the source.

## Checking a change by hand

```sh
./packaging/aur/test-local.sh
```

Needs docker. It builds the package, diffs the generated `.SRCINFO` against the
committed one, runs `namcap` over both the recipe and the built package, then
installs it and fetches a live quote.

Three `namcap` warnings are expected and harmless: the literal `x86_64` in the
source URL (unavoidable, the two architectures need different checksums, which
forces per-architecture `source_*` arrays), the unused `ld-linux` entry, and
`gcc-libs` being reported as maybe-redundant even though the binary does link
`libgcc_s.so.1`.
