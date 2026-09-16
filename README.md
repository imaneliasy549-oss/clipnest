# ClipNest

[![CI](https://github.com/imaneliasy549-oss/clipnest/actions/workflows/ci.yml/badge.svg)](https://github.com/imaneliasy549-oss/clipnest/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/imaneliasy549-oss/clipnest)](https://github.com/imaneliasy549-oss/clipnest/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![GNOME 45+](https://img.shields.io/badge/GNOME-45%2B-blue)](https://extensions.gnome.org)

> **Clipboard history for GNOME on Wayland.** Press `Super+Shift+V`, type, click an entry — it's pasted into the window you came from. Text, images, pinning, search, size limits.
>
> **فارسی:** تاریخچهٔ کلیپ‌بورد برای گنوم روی Wayland. با `Super+Shift+V` باز می‌شود، روی آیتم می‌زنی و همان‌جا پیست می‌شود. متن، تصویر، پین، جستجو.

<!-- Screenshot here: save a screenshot of the panel as docs/screenshot.png and uncomment:
![ClipNest panel](docs/screenshot.png)
-->

---

## Install

### Ubuntu 24.04+ / Debian 13+

```bash
# amd64
wget https://github.com/imaneliasy549-oss/clipnest/releases/latest/download/clipnest_0.7.0-1_amd64.deb
sudo apt install ./clipnest_0.7.0-1_amd64.deb

# arm64
wget https://github.com/imaneliasy549-oss/clipnest/releases/latest/download/clipnest_0.7.0-1_arm64.deb
sudo apt install ./clipnest_0.7.0-1_arm64.deb
```

Then, **once per user**:

```bash
clipnest setup
clipnest doctor        # should be all ✅
```

**Log out and back in** — GNOME Shell only loads the extension at session start.

### Fedora / RHEL / openSUSE

```bash
sudo dnf install ./clipnest-0.7.0-1.fc44.x86_64.rpm
```

### Arch / CachyOS

Use the `PKGBUILD` from the [latest release](https://github.com/imaneliasy549-oss/clipnest/releases/latest).

### Portable tarball (any distro)

Download `clipnest-0.7.0-x86_64-linux-gnu.tar.gz` from the release, then `./install.sh`.

### From source

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libsqlite3-dev
make install
```

---

## Usage

| Action | Key |
| --- | --- |
| Open / close the panel | `Super+Shift+V` (same as Win+Shift+V) |
| Paste an entry | `Enter` or click |
| Navigate | `↑` `↓` `PageUp` `PageDown` `Home` `End` |
| Pin / unpin | `Ctrl+P` |
| Delete | `Delete` |
| Close | `Esc` |
| Search | type in the box at the top |

Focus stays in the search box — arrow keys move the selection without leaving it. `Backspace` clears the search, `Delete` removes the selected entry.

### Auto-paste

Clicking an entry both copies it and sends `Ctrl+V` to the window you came from. The first time, GNOME asks for keyboard-control permission:

```bash
clipnest paste-access      # open the dialog now, wait for the answer
clipnest doctor            # confirms it's ready
```

`clipnest setup` triggers this at the end too. Once granted, the token is remembered — no repeated prompts. If the screen is locked, GNOME refuses and ClipNest says so. If the portal isn't available, auto-paste silently falls back to copy-only; nothing breaks.

### CLI

```bash
clipnest list                    # history (* = pinned, newest first)
clipnest search docker           # substring search
clipnest get 12 --out a.txt      # dump one entry to a file
clipnest pick 1                  # copy entry #1 (no paste)
clipnest paste 1                 # copy + paste entry #1
clipnest pin 12 / unpin 12
clipnest delete 12 / clear / vacuum / stats / status
clipnest config                  # effective settings + path read
clipnest doctor                  # full install health check
```

If the daemon isn't running, `list`, `get` and `stats` read the database read-only and warn.

### Configuration

```bash
clipnest config
$EDITOR ~/.config/clipnest/config.ini
systemctl --user restart clipnest      # daemon reads config only at startup
```

| Key | Default | What it does |
| --- | --- | --- |
| `max_items` | 300 | Non-pinned entries kept |
| `image_budget_mb` | 128 | Total image budget (oldest evicted first) |
| `max_text_mb` | 1 | Single text limit (0 = off) |
| `max_image_mb` | 8 | Single image limit (0 = off) |
| `panel_limit` | 300 | Panel rows read at once |
| `poll_interval_ms` | 400 | How often the extension checks the clipboard |
| `max_age_days` | 0 | Drop unused entries older than this (0 = never) |
| `paste_key` | `<Control>v` | Auto-paste shortcut; `none` = copy-only |

Pinned entries are exempt from every pruning rule.

The extension asks the daemon for the limits over D-Bus (method `Config`), so raising `max_image_mb` actually takes effect.

### Backup

```bash
clipnest export ~/clipnest-backup          # folder + index.tsv + one file per entry
clipnest export ~/b --limit 50 --force     # newest 50, over an existing folder
clipnest import ~/clipnest-backup          # restore (existing entries merged, not duplicated)
```

Images keep their original bytes (PNG/JPEG), no re-encoding. Timestamps and pins are restored exactly, so history order is preserved. A directory without `index.tsv` still imports by filename.

---

## Supported

| Distro | Status |
| --- | --- |
| Ubuntu 24.04+ (GNOME 46+) | ✅ with the `.deb` |
| Debian 13 trixie (GNOME 48) | ✅ with the `.deb` |
| Fedora 40+ / openSUSE Tumbleweed | ✅ via RPM spec or tarball |
| Arch / CachyOS | ✅ via `PKGBUILD` |
| arm64 (any of the above) | ✅ prebuilt `.deb` and `.rpm` |
| Ubuntu 22.04 (GNOME 42) | ❌ GNOME 42 predates GTK 4.8; the extension targets GNOME 45+ |

Package dependencies are read from the binary itself (`dpkg-shlibdeps`), not guessed:

```
Depends: libadwaita-1-0 (>= 1.0.1), libc6 (>= 2.39), libglib2.0-0t64 (>= 2.54.0),
         libgraphene-1.0-0 (>= 1.5.4), libgtk-4-1 (>= 4.7.2), libsqlite3-0 (>= 3.5.9)
```

---

## Troubleshooting

Start here:

```bash
clipnest doctor
```

Every line is ✅ / ⚠️ / ❌ with the command that fixes it. The most common cases:

| Symptom | Fix |
| --- | --- |
| Nothing is captured | `clipnest status`; if no daemon: `systemctl --user restart clipnest` |
| New items don't appear (old ones do) | The extension in memory is old — **log out and back in** |
| Shortcut doesn't work | `clipnest setup-shortcut`; `doctor` flags conflicts with GNOME keys |
| Panel opens but doesn't take focus | Trigger it from GNOME, not from a terminal, so the activation token is passed |
| Database is large | `clipnest stats`, then `clipnest vacuum`; lower `image_budget_mb` |
| Click copies but doesn't paste | `clipnest paste-access`; if it says the screen is locked, unlock first |
| `doctor` warns a user install shadows the package | `make uninstall` — **never** `rm -rf` under `/usr`, that's the package |
| `auto-paste: needs permission` | `clipnest paste-access` (once), or set `paste_key = none` to disable |

Full table in [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md).

### Uninstall

```bash
# user install only
make uninstall

# system package
clipnest remove-shortcut
gnome-extensions disable clipnest@clipnest.dev
sudo dpkg -r clipnest

# history (kept on purpose across installs)
rm -rf ~/.local/share/clipnest
```

Never mix the two install methods. A user install shadows the package (`~/.local/bin` precedes `/usr/bin`, and GNOME Shell reads `~/.local/share/gnome-shell/extensions` first) — `clipnest doctor` reports this.

---

## Development

```bash
make check        # clippy -D warnings + 64 tests
make deb          # build .deb in target/debian/
make deb-verify   # inspect the package without installing
make dist         # build dist/ — .deb, tarballs, RPM spec, PKGBUILD, SHA256SUMS
make dist-verify  # verify artifacts and their checksums
make preflight    # pre-publish checks (git identity, stale dist/, leftover placeholders)
```

Portal tests open a permission dialog, so they're ignored by default:

```bash
cargo test --offline -- --ignored --nocapture
```

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for how the extension, daemon and portal fit together, and [`RELEASING.md`](RELEASING.md) for the release process.

---

## License

MIT — see [`LICENSE`](LICENSE).
