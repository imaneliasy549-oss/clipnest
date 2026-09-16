# Architecture

## Why an extension?

On Wayland, the clipboard belongs to the compositor. **Mutter does not implement either data-control protocol** (`wlr-data-control-unstable-v1` or `ext-data-control-v1`), so `wl-clipboard` and any background process that wants to read the clipboard directly does not work on GNOME. That's why the work is split:

```
┌──────────────────────────────┐         D-Bus          ┌────────────────────────┐
│ GNOME Shell extension (JS)   │  PushText / PushImage  │  clipnest daemon (Rust)│
│ the only piece allowed to    │ ─────────────────────► │  history in SQLite     │
│ read the clipboard in the    │                        │  GTK4/Adw panel        │
│ background                   │                        │                        │
└──────────────────────────────┘                        └────────────────────────┘
                                                                 ▲
                                              Super+Shift+V → clipnest toggle
```

- **Extension** — reads `St.Clipboard` every 400 ms (`St.Clipboard` has no change signal, so polling is required) and sends only when the value actually changed.
- **Daemon** — keeps history in `~/.local/share/clipnest/history.db`, shows the panel. Writing to the clipboard happens from the panel itself (while it has focus); because the daemon stays alive, the selection survives.
- **Shortcut** — a regular GNOME Custom Shortcut (default `Super+Shift+V`, changeable via `clipnest setup-shortcut --binding`). The activation token (`XDG_ACTIVATION_TOKEN`) is passed to the daemon so the panel actually takes keyboard focus.

## Why the daemon isn't started at login

The daemon is **D-Bus-activated, not started on login**: the unit is `Type=dbus` with `BusName=dev.clipnest.Daemon`, so the first call (a copy from the extension, or `clipnest toggle`) brings it up. `make install` writes a D-Bus activation file to `~/.local/share/dbus-1/services/dev.clipnest.Daemon.service` — the same thing `xdg-desktop-portal` does.

This is intentional. If the daemon started under `graphical-session.target`, GTK4 would initialize before the session was ready and touch `org.freedesktop.portal.Settings`; the GTK backend of that service dies with `cannot open display`, and from that moment every application that asks the portal for anything hangs behind the 25-second D-Bus timeout — to the point where opening Chrome or a terminal takes minutes. Deferring to the first copy makes that ordering impossible, because the daemon's only data source (the shell extension) doesn't exist until the shell is up.

The panel is also lazily constructed: no window exists until `Super+Shift+V`. `clipnest list` / `status` / `pick` never create a window, so they work in a headless environment too.

## Optional portal guard

Independent of ClipNest, you can harden the portal services themselves:

```bash
make install-portal-guard     # Restart=on-failure + TimeoutStartSec=30 for portal services
make uninstall-portal-guard   # revert
```

## Why nothing was captured in v0.2

`St.Clipboard` in GJS **has no `_finish` method**; `get_text` and `get_content` are callback-based:

```js
clipboard.get_text(St.ClipboardType.CLIPBOARD, (clipboard, text) => { ... });
clipboard.get_content(St.ClipboardType.CLIPBOARD, 'image/png', (clipboard, bytes) => { ... });
```

The earlier version called `get_text_finish` inside the callback, which doesn't exist; the exception was caught by a `try/catch` and silently swallowed, so **no text was captured at all**. Worse: image reads used the wrong signature `get_content(type, callback)`, which made the exception bubble out and the `_reading` flag stay `true` forever — meaning the first image copy put all capture to sleep until restart. Both paths are now called correctly and reads have a 3-second guard so a stuck transfer doesn't lock the poll loop.

Images are no longer stored as raw RGBA pixels: the encoding the application supplied (PNG/JPEG/…) is kept and GDK decodes it for display. A 1024×1024 image used to be ~4 MB; now it's typically a few hundred KB. Colors are correct too (R/B channel order no longer swapped).

## Auto-paste internals

Selecting an entry (click or `Enter`) both puts it on the clipboard and sends the paste shortcut to the previous window:

```
panel hides  ──►  focus returns  ──►  Portal RemoteDesktop  ──►  Ctrl+V
```

1. The panel hides, and the daemon polls every 30 ms for "has focus left?" (ceiling 750 ms, plus a 120 ms grace period for the compositor to hand focus to the next window). A plain `set_visible(false)` isn't enough: in the window between closing and focus moving, keystrokes go to the exiting panel.
2. The daemon requests a portal session: `CreateSession` → `SelectDevices(types=keyboard)` → `Start`. This is the moment GNOME asks "allow keyboard control?".
3. Keys go via `NotifyKeyboardKeysym`: `Control` down, `v`, `v` up, `Control` up — with 4 ms between them, because a modifier that arrives at the same time as its key is not always applied first.

The portal grant is keyboard-only (`types=keyboard`): no screen capture, no mouse. The panel always writes to both `CLIPBOARD` and `PRIMARY`, so middle-click paste works too.
