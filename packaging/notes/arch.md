# Arch Linux: a recipe, built on Arch itself

`PKGBUILD` is here with the version and the checksum of the source tarball filled
in; the package itself (`*.pkg.tar.zst`) is not, because building it needs
`makepkg`, which only exists on Arch.

That is also the right way round for a rolling distribution: the PKGBUILD builds
from source with the machine's own current toolchain, so there is no stale glibc
to inherit and the binary links against the gtk4 and libadwaita that are actually
installed.

```bash
mkdir -p ~/build/clipnest && cd ~/build/clipnest
cp <this directory>/PKGBUILD .
cp <../source/clipnest-@VERSION@.tar.gz> .
makepkg -si
```

`makepkg` runs `cargo fetch --locked` first, so the build itself is offline and
reproducible once the crates are cached.

The package installs to the usual Arch paths (`/usr/bin/clipnest`,
`/usr/lib/systemd/user/`, `/usr/share/gnome-shell/extensions/`), and each user
finishes with `clipnest setup` plus one log out and back in.
