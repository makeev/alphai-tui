# alphai-tui apt repository

Generated content only. This repository is the Debian and Ubuntu package feed
for [alphai-tui](https://github.com/makeev/alphai-tui), a terminal stock
dashboard with charts, AI scored news and SEC Form 4 insider activity.

Every release force pushes a single commit here from CI, so do not send pull
requests against it. The scripts that build it live in the main repository under
`packaging/deb/`.

## Install

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://makeev.github.io/alphai-tui-apt/alphai-tui.gpg \
  -o /etc/apt/keyrings/alphai-tui.gpg
echo "deb [signed-by=/etc/apt/keyrings/alphai-tui.gpg] https://makeev.github.io/alphai-tui-apt stable main" \
  | sudo tee /etc/apt/sources.list.d/alphai-tui.list
sudo apt update && sudo apt install alphai-tui
```

Packages are built for amd64 and arm64 and need glibc 2.35 or newer, which
means Ubuntu 22.04 and later or Debian 12 and later.

The signing key is `F2E1 9930 D21E B459 AF91  A85B 72DE 82F5 F074 E30D`. Its
public half is also committed to the main repository at
`packaging/deb/alphai-tui.gpg`, so what apt trusts can be compared against what
is in source control.
