# Troubleshooting

Start here:

```bash
clipnest doctor
```

Every line is ✅ / ⚠️ / ❌ and next to each issue is the command to fix it.

## Nothing is captured

```bash
clipnest status
systemctl --user restart clipnest
```

If the daemon isn't running, `list` / `get` / `stats` still read the database read-only.

## New images/text don't appear, old ones do

The extension in `gnome-shell` memory is the old version. GNOME Shell only reads extension code at session start.

**Log out and back in.**

## The shortcut doesn't work

```bash
clipnest setup-shortcut
```

`clipnest doctor` flags conflicts with GNOME's own shortcuts with ⚠️. For example `<Super>v` is taken by GNOME's `toggle-message-tray`; if you want it, unbind GNOME's first.

## Panel opens but doesn't take focus

The shortcut must be triggered by GNOME itself (not by running `clipnest toggle` in a terminal), otherwise the activation token (`XDG_ACTIVATION_TOKEN`) isn't passed and the panel can't grab the keyboard.

## `make install` succeeded but nothing changed

```bash
systemctl --user daemon-reload
systemctl --user restart clipnest
```

## After installing the `.deb`, nothing is captured

Did you run `clipnest setup`? Then, **log out and back in** once so GNOME Shell loads the new extension.

## `clipnest doctor` says the user manager doesn't know the unit

```bash
systemctl --user daemon-reload
```

`clipnest setup` does this automatically.

## `doctor` warns a user install shadows the package

```bash
make uninstall
```

**Never** `rm -rf` under `/usr` — that's the package itself.

## Settings don't take effect

```bash
clipnest config               # see effective values and the file path
systemctl --user restart clipnest
```

The daemon reads config only at startup. The extension re-reads limits from the daemon about once a minute.

## Click copies but doesn't paste

```bash
clipnest paste-access
```

If the message says the screen is locked, unlock first. GNOME refuses RemoteDesktop sessions while the lock screen is up, and the portal just returns code 2 without explanation. ClipNest detects this with `org.gnome.ScreenSaver.GetActive` and tells you. Nothing is lost — the entry is on the clipboard and the next attempt creates a fresh session.

## `doctor` says `auto-paste: needs permission`

```bash
clipnest paste-access
```

Or set `paste_key = none` in `config.ini` to disable auto-paste and keep copy-only.

## `paste_key` changed but does nothing

```bash
systemctl --user restart clipnest
```

## Database is large

```bash
clipnest stats
clipnest vacuum
```

Also lower `image_budget_mb` in `config.ini`.

## `clipnest status` says the daemon version is old

```bash
systemctl --user restart clipnest
```

## Want to remove everything

```bash
# user install
make uninstall

# system package
clipnest remove-shortcut
gnome-extensions disable clipnest@clipnest.dev
sudo dpkg -r clipnest

# history (kept across installs on purpose)
rm -rf ~/.local/share/clipnest
```

## Notes

- While the daemon is running, **the clipboard is owned by it**; restarting the daemon clears the clipboard contents (the clipboard, not the history).
- The selected entry goes to both `CLIPBOARD` and `PRIMARY`, so regular paste and middle-click paste give the same entry.
- Before installing a freshly built `.deb`, run `make uninstall` first so a user install doesn't shadow it.
