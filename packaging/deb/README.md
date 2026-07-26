# Debian and Ubuntu packaging

An apt repository at <https://makeev.github.io/alphai-tui-apt>, served by GitHub
Pages out of [makeev/alphai-tui-apt](https://github.com/makeev/alphai-tui-apt).
Users get `apt install` and, unlike a one-off `.deb`, `apt upgrade` as well.

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://makeev.github.io/alphai-tui-apt/alphai-tui.gpg \
  -o /etc/apt/keyrings/alphai-tui.gpg
echo "deb [signed-by=/etc/apt/keyrings/alphai-tui.gpg] https://makeev.github.io/alphai-tui-apt stable main" \
  | sudo tee /etc/apt/sources.list.d/alphai-tui.list
sudo apt update && sudo apt install alphai-tui
```

Packages are not compiled here: they repackage the binary from the matching
GitHub release, exactly like the AUR recipe does.

## Files

| File | What it is |
|---|---|
| `build-repo.sh` | Release tarballs to `.deb` to signed repository tree. No network, no git: it only writes files. |
| `publish.sh` | Everything around that: download the release, run the build (in `debian:12-slim` when the host has no `dpkg-deb`), force push the tree, optionally attach the `.deb` files to the release. |
| `test-local.sh` | Adds the repository the way the README tells users to, then installs and runs the binary in throwaway containers. |
| `alphai-tui.gpg` | Public half of the signing key, published next to the repository. Committed so that what apt trusts can be diffed against source control. |
| `known_hosts` | Pinned github.com host keys for the CI push. |
| `site/` | Landing page and README copied into the published repository. `@VERSION@` is substituted at publish time. |

## Layout of the published repository

One suite, `stable`, one component, `main`, architectures `amd64` and `arm64`.
A single suite for every distribution is honest here because every distribution
gets the same binary.

```
alphai-tui.gpg
index.html
pool/main/a/alphai-tui/alphai-tui_<version>_<arch>.deb
dists/stable/{Release,Release.gpg,InRelease}
dists/stable/main/binary-<arch>/Packages{,.gz}
```

The pool keeps the last three versions and every publish force pushes a single
orphan commit, so old `.deb` blobs leave the history instead of piling up in a
repository that Pages re-serves on every release.

## Compatibility

`Depends: libc6 (>= 2.35)`, because release binaries are built on `ubuntu-22.04`
runners with no container. That covers Ubuntu 22.04 and later and Debian 12 and
later. On Ubuntu 20.04 apt refuses the install with an unmet dependency, which is
the point: better a clear refusal than a binary that dies at exec time.

## Releasing

Nothing to do by hand. `publish-jobs` in `dist-workspace.toml` runs
`.github/workflows/publish-deb.yml` after the release artifacts are uploaded, and
that job is a thin wrapper around `publish.sh`. Two secrets:
`APT_GPG_PRIVATE_KEY` (signing key, public half committed here) and
`APT_SSH_KEY` (deploy key with write access to the apt repository).

`publish.sh` does not attach the `.deb` files to the GitHub release in CI: dist
hands custom publish jobs a `GITHUB_TOKEN` without `contents: write`, so the
standalone download lives in the pool that the landing page links to. Passing
`--upload` from a shell with a wider token does attach them.

To republish an existing tag, run the job manually with `workflow_dispatch` and a
tag, or run the script locally:

```sh
export GH_TOKEN=$(gh auth token -u makeev)
export APT_GPG_PRIVATE_KEY="$(cat ~/.ssh/alphai-tui-apt-signing.asc)"
export APT_SSH_KEY="$(cat ~/.ssh/id_ed25519_alphai_apt)"
./packaging/deb/publish.sh --version 0.10.2
```

## Checking a change by hand

```sh
./packaging/deb/publish.sh --version 0.10.2 --no-push    # prints where the tree went
./packaging/deb/test-local.sh --repo <that tree> --version 0.10.2
./packaging/deb/test-local.sh --url https://makeev.github.io/alphai-tui-apt
```

Needs docker. The test installs on `ubuntu:22.04`, `ubuntu:24.04` and
`debian:12`, half of them under emulation, which is the only way to see both
architectures' packages install.

`build-repo.sh` ends by verifying its own signatures with `gpgv` against
`alphai-tui.gpg`. A CI secret that has drifted from the committed public key
therefore fails the release instead of breaking `apt update` for everyone.

## The signing key

`F2E1 9930 D21E B459 AF91  A85B 72DE 82F5 F074 E30D`, rsa4096, no passphrase, no
expiry. An expiring repository key breaks apt for every user on the day it
lapses, and rotating one means asking everyone to re-add it, so this one is meant
to last. The private half is the `APT_GPG_PRIVATE_KEY` secret, with a local
backup at `~/.ssh/alphai-tui-apt-signing.asc`.
