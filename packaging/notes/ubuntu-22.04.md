# Ubuntu 22.04 LTS: no package, and not a packaging problem

This directory is here to answer the question instead of leaving a hole. There is
no `.deb` for jammy because none can exist, and the reason is worth writing down
so nobody spends an evening trying.

Two hard floors, and 22.04 is below both of them:

| What | 22.04 (jammy) has | This needs |
| --- | --- | --- |
| GTK | 4.6.1 | **4.8** — the crate asks for it (`features = ["v4_8"]`) because decoding a clipboard image from memory uses `gdk::Texture::from_bytes`, which did not exist before 4.8 |
| GNOME Shell | 42 | **45** — `extension.js` is written against the GJS/Shell APIs of 45 and newer |

And the floor of the package in `../ubuntu-24.04-amd64/` is `libc6 (>= 2.39)`,
while jammy has glibc 2.35. So even a `.deb` built on jammy would be a `.deb`
built for a GTK and a Shell that cannot run this program.

## What to do instead

**Nothing on 22.04.** The desktop is two years past what this project needs, and
the honest answer is that it is not supported:

```console
$ clipnest doctor       # on 22.04: gnome-shell 42, GTK 4.6
```

Building from `../source/` will not help either: `cargo build` stops with

```
error: gtk4 4.8 is required, but 4.6.1 was found
```

## If you maintain a distribution and want it on an older base

The two things to change are both in the source, not in the packaging:

1. Drop the `v4_8` feature and replace the image decode path
   (`gdk::Texture::from_bytes` → `gdk_pixbuf::Pixbuf::from_stream` plus
   `gdk::Texture::for_pixbuf`, which exists in GTK 4.0).
2. Port `extension.js` back to GNOME 42's Shell APIs.

That is a fork with its own maintenance, which is why it is not shipped here.

## The next release up

Ubuntu 24.04 (noble, GNOME 46, GTK 4.14, glibc 2.39) is the oldest supported
system, and the `.deb` in the sibling directory installs on it and on everything
newer. It was not built there — see the top-level `README.md` in this directory
for which artifact was built on which machine and which are built by the release
workflow.
