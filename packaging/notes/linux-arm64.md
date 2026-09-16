# arm64 / aarch64: not built on this machine

Nothing here yet, and this file says why rather than leaving a hole.

A `.deb` or a tarball has to be built **on** the architecture it runs on: the
binary is linked against that architecture's GTK, libadwaita and SQLite, and a
cross-build would need all of those libraries for aarch64 plus a Rust standard
library for the target. This machine has none of them, and no way to get them
(there is no rustup toolchain, no `gcc-aarch64-linux-gnu`, and no working
network route to the Ubuntu archive from here).

Two ways to get the real thing:

**1. The release workflow (no machine of your own needed).** Pushing a tag runs
`.github/workflows/release.yml`, which builds this directory on GitHub's native
arm64 runner (`ubuntu-24.04-arm`, free for public repositories) and attaches
`clipnest_@VERSION@-1_arm64.deb` and `clipnest-@VERSION@-aarch64-linux-gnu.tar.gz`
to the release.

**2. On any arm64 Linux machine with Rust and the development libraries:**

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libsqlite3-dev
git clone <repository> && cd clipnest
make dist                 # dist/<distribution>-<version>-arm64/ and dist/tarball/
```

or, on Fedora/RHEL/openSUSE, `make dist-tarball` plus the spec in
`../fedora-aarch64/`.

The result installs on any arm64 system that satisfies the same floor as the
x86-64 package: glibc 2.39 or newer, GTK 4.8 or newer, and GNOME 45 or newer
(Ubuntu 24.04 arm64 and up, Debian 13 arm64, Fedora 40 arm64 and up).
