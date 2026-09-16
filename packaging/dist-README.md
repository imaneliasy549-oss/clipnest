# What is in this directory

Everything here comes from one commit, one version, built by `make dist`. One
directory per target, and each directory either holds the real artifact or a
README saying exactly what is missing and what produces it.

A `.deb` directory is named after the **oldest Ubuntu release the package can be
installed on**, which is read out of the package's own dependency list (`libc6 (>=
2.39)` is Ubuntu 24.04). That is why the name can differ from the machine that
built it: a package built on 26.04 has a 24.04 floor, and the directory says the
floor.

| Directory | For | In this build |
| --- | --- | --- |
| `ubuntu-24.04-amd64/` | Debian and Ubuntu, 64-bit x86, 24.04 and newer | ✅ the real `.deb` |
| `ubuntu-22.04-amd64/` | — | ⛔ a README saying why no package can exist for it |
| `tarball/` | **any** distribution of the same architecture, no root | ✅ the real tarball |
| `source/` | anyone: `make && make install` on their own machine | ✅ always |
| `linux-arm64/` | 64-bit ARM | ⛔ see the README in it — built by the release workflow or on an arm machine |
| `fedora-<arch>/` | Fedora / RHEL / openSUSE (RPM) | ⛔ recipe only; needs `rpmbuild` |
| `arch-<arch>/` | Arch Linux (PKGBUILD) | ⛔ recipe only; built by `makepkg` on Arch |
| `SHA256SUMS` | verifying a download: `sha256sum -c SHA256SUMS` | — |
| `README.md` | this file | — |

Every directory whose artifact is missing contains a README that names the one
command that produces it, and the release workflow (`.github/workflows/release.yml`)
runs those commands on the platforms that have the tooling — so a release built
from CI has real `arm64` and RPM files where a workstation has recipes.

Install and finish:

```bash
# Debian / Ubuntu 24.04 and newer
sudo apt install ./ubuntu-24.04-amd64/clipnest_*.deb

# anything else of the same architecture, no root
tar xzf tarball/clipnest-*-linux-*.tar.gz && cd clipnest-*-linux-* && ./install.sh
```

Then **every user** of the machine does the parts that belong to their own
session, and logs out and back in once:

```bash
clipnest setup        # the shell extension, the shortcut, the service
clipnest doctor       # says what is still wrong, if anything
```

## Which systems can install this?

The answer is in the package, not in a promise: `dpkg-shlibdeps` reads the
dependencies out of the binary itself.

```console
$ dpkg-deb -f ubuntu-*/clipnest_*.deb Depends
libadwaita-1-0 (>= 1.0.1), libc6 (>= 2.39), libglib2.0-0t64 (>= 2.54.0),
libgraphene-1.0-0 (>= 1.5.4), libgtk-4-1 (>= 4.7.2), libsqlite3-0 (>= 3.5.9)
```

That means glibc 2.39, GTK 4.8 and libadwaita 1.0.1 as the floor, plus GNOME 45
for the shell extension. In practice:

| System | Works? |
| --- | --- |
| Ubuntu 24.04 (GNOME 46) and newer | ✅ the same `.deb` — the glibc symbol floor is 2.39, which is what 24.04 has, not what the build host has |
| Debian 13 trixie (GNOME 48) | ✅ the same `.deb` |
| Fedora 40+ / openSUSE Tumbleweed | ✅ the RPM, or the tarball |
| Arch / CachyOS | ✅ the PKGBUILD, or the tarball |
| Debian 12 bookworm (GNOME 43) | ❌ GNOME too old for the extension |
| **Ubuntu 22.04 LTS (GNOME 42, GTK 4.6)** | ❌ **not supported, and not a packaging problem**: GNOME 42 ships GTK 4.6 while this needs 4.8, and the extension is written for GNOME 45+. No build of this project can run there — see `ubuntu-22.04-amd64/README.md`. |

A package built on a *newer* distribution than the one you run can refuse to
install even when nothing in the list above is wrong, because the build host's
compiler may emit newer symbols. If that happens, use `source/`: building on your
own machine is never too new for it.

Two claims in that table are worth keeping apart, because only one of them can be
checked here: the **floor** is measured from the artifact (`dpkg-deb -f`, above),
and it is what the directory name is derived from. Whether the binary actually
*runs* on a given release is verified on the machine that built the package and on
the CI legs named in `RELEASING.md`; the table is the claim, the package is the
evidence, and where they have not both been checked it is said so.

---

فارسی: هر پوشه یک هدف است. `.deb` برای دبیان/اوبونتو، `.rpm` برای فدورا/اپن‌سوزه
(توسط rpmbuild ساخته می‌شود)، `PKGBUILD` برای آرچ، و پوشهٔ `tarball/` برای هر
توزیع دیگری — بدون root و بدون بسته‌بند (`./install.sh`). اگر معماری یا توزیع
دیگری می‌خواهی، پوشه‌اش یک `README.md` دارد که دقیقاً می‌گوید با چه دستوری ساخته
می‌شود. بعد از نصب، هر کاربر یک‌بار `clipnest setup` می‌زند و یک‌بار logout/login
می‌کند.

نام پوشهٔ `.deb` را از خود بسته می‌خوانیم (`libc6 (>= 2.39)` یعنی اوبونتو ۲۴.۰۴)،
نه از توزیعی که رویش ساخته شده؛ پس بسته‌ای که روی ۲۶.۰۴ ساخته شده در پوشهٔ
`ubuntu-24.04-amd64/` می‌نشیند — یعنی جایی که واقعاً نصب می‌شود. پوشهٔ
`ubuntu-22.04-amd64/` هم وجود دارد، ولی فقط برای این‌که بگوید چرا چنین بسته‌ای
ممکن نیست (GTK 4.6 در برابر ۴.۸ و Shell 42 در برابر 45).
