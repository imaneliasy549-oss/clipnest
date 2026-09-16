#!/bin/sh
# Removes a tarball installation of ClipNest: exactly what install.sh placed, and
# nothing else. The history in ~/.local/share/clipnest/history.db is left alone,
# along with the settings file and the portal restore token.
#
# A package installation is not touched: `/usr` belongs to the package manager
# (`sudo apt remove clipnest` or `sudo dnf remove clipnest`), and deleting those
# files by hand only confuses both.

set -eu

xdg() {
    eval "value=\${$1:-}"
    case $value in
        /*) printf '%s\n' "$value" ;;
        *) printf '%s\n' "$HOME/$2" ;;
    esac
}

data=$(xdg XDG_DATA_HOME .local/share)
config=$(xdg XDG_CONFIG_HOME .config)
bin="$HOME/.local/bin/clipnest"
uuid=clipnest@clipnest.dev

# Take the shortcut and the extension back out of GNOME first: the binary is
# about to disappear, and it is what knows how to do that.
if [ -x "$bin" ]; then
    "$bin" remove-shortcut || true
fi
if command -v gnome-extensions >/dev/null 2>&1; then
    gnome-extensions disable "$uuid" || true
fi

# The unit has no [Install] section (the session bus starts it on demand), so
# there is nothing enabled to turn off.
if command -v systemctl >/dev/null 2>&1; then
    systemctl --user stop clipnest 2>/dev/null || true
    systemctl --user daemon-reload || true
fi

rm -f "$bin" \
    "$config/systemd/user/clipnest.service" \
    "$data/dbus-1/services/dev.clipnest.Daemon.service" \
    "$data/applications/clipnest.desktop" \
    "$data/icons/hicolor/scalable/apps/dev.clipnest.Panel.svg"
rm -rf "$data/gnome-shell/extensions/$uuid"

echo "ClipNest removed. Log out and back in once to unload the extension."
echo "Your history is still in $data/clipnest/history.db"
