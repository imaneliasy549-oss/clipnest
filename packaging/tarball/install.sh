#!/bin/sh
# ClipNest, installed for the current user: no root, no package manager, no
# distribution in particular. Everything goes where the XDG specification says
# it should, and the last step is `clipnest setup`, which does the parts only the
# user can do (enabling the shell extension, registering the shortcut).
#
# The same files as a package would place, just inside $HOME. Do not run this on
# top of the distribution package: a copy under ~/.local shadows the packaged one
# (it comes first in $PATH, and gnome-shell reads ~/.local/share first), which is
# exactly the confusion `clipnest doctor` warns about.

set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
uuid=clipnest@clipnest.dev

xdg() {
    # $1: variable name, $2: the ~/.local default for it.
    eval "value=\${$1:-}"
    case $value in
        /*) printf '%s\n' "$value" ;;
        *) printf '%s\n' "$HOME/$2" ;;
    esac
}

data=$(xdg XDG_DATA_HOME .local/share)
config=$(xdg XDG_CONFIG_HOME .config)
bin="$HOME/.local/bin/clipnest"

# `systemctl` and `gnome-extensions` are deliberately not required: setup
# reports what to do by hand when either is missing, and a machine without them
# can still have the files in place.
require() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "clipnest: '$1' is missing" >&2
        exit 1
    }
}
require install
require sed

install -d "$(dirname "$bin")" \
    "$data/dbus-1/services" \
    "$config/systemd/user" \
    "$data/gnome-shell/extensions/$uuid"

install -m755 "$here/bin/clipnest" "$bin"
install -m644 "$here/data/clipnest.service" "$config/systemd/user/clipnest.service"
# The D-Bus activation file names the binary that starts the daemon, so its path
# is filled in here instead of being guessed at build time.
sed "s|@BIN@|$bin|" "$here/data/dev.clipnest.Daemon.service" \
    > "$data/dbus-1/services/dev.clipnest.Daemon.service"
install -m644 "$here/data/gnome-shell/$uuid/metadata.json" \
    "$here/data/gnome-shell/$uuid/extension.js" \
    "$data/gnome-shell/extensions/$uuid/"

# A desktop file and an icon give the panel a name and a picture in the shell's
# window list; without them it is an anonymous window. Its Exec line is filled in
# for the same reason as the D-Bus service above: there is no /usr/bin/clipnest
# in an installation that lives in $HOME.
install -d "$data/applications"
sed "s|/usr/bin/clipnest|$bin|" "$here/data/clipnest.desktop" \
    > "$data/applications/clipnest.desktop"
install -d "$data/icons/hicolor/scalable/apps"
install -m644 "$here/data/dev.clipnest.Panel.svg" \
    "$data/icons/hicolor/scalable/apps/dev.clipnest.Panel.svg"

echo "installed $bin"
echo

# systemd caches unit files it has already read.
if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload || true
fi

# The extension, the shortcut, the service, and - if auto-paste is on - the
# permission question. All of it lives in the binary so both the tarball and the
# package finish in exactly the same state.
"$bin" setup

echo
echo "ClipNest is installed. Log out and back in once so gnome-shell loads the"
echo "extension, then run: $bin doctor"
