#!/usr/bin/env bash
# Checks a .deb the way an install does, not the way a build does.
#
#   bash packaging/tests/install-smoke.sh [--no-install] dist/ubuntu-24.04-amd64/clipnest_*.deb
#
# Two halves:
#
#   * the payload checks (always): the metadata a package manager reads, the
#     files that have to be in there, and - the one that keeps biting - that the
#     extension shipped inside the package is byte for byte the one in the
#     repository. A package whose extension files are older than its binary
#     captures nothing, and nothing else in the pipeline notices.
#
#   * the install checks (only with root, or `--install`): `apt-get install` so
#     the declared dependencies are resolved for real, then the binary runs and
#     every shared library it needs is actually present.
#
# Exits non-zero on the first real problem, with the problem printed.
set -u

install_pkg=0
[ "${1:-}" = "--install" ] && { install_pkg=1; shift; }
[ "${1:-}" = "--no-install" ] && shift

deb="${1:-}"
if [ -z "$deb" ] || [ ! -f "$deb" ]; then
    echo "usage: $0 [--install|--no-install] <clipnest_*.deb>" >&2
    exit 2
fi

problems=0
pass() { printf '  ok   %s\n' "$1"; }
fail() { printf '  FAIL %s\n' "$1"; problems=$((problems + 1)); }

repo_dir=$(cd "$(dirname "$0")/../.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo_dir/Cargo.toml" | head -1)

echo "== $deb (repository version $version)"

# --- the metadata a package manager decides with ---------------------------
field() { dpkg-deb -f "$deb" "$1" 2>/dev/null || true; }
for name in Package Version Architecture Depends Description; do
    value=$(field "$name")
    if [ -n "$value" ]; then pass "$name: $(echo "$value" | head -1)"; else fail "$name is empty"; fi
done
[ "$(field Package)" = "clipnest" ] || fail "Package is not 'clipnest'"
[ "$(field Version)" = "$version-1" ] || fail "Version is $(field Version), expected $version-1"

# Every library the binary links has to be named, or apt installs a package that
# cannot start. These are read out of the binary by cargo-deb, so a missing one
# means the build environment was strange rather than the metadata being short.
depends=$(field Depends)
for lib in libc6 libgtk-4-1 libsqlite3-0; do
    case "$depends" in
        *"$lib"*) pass "Depends names $lib" ;;
        *) fail "Depends does not name $lib" ;;
    esac
done

# --- the payload -----------------------------------------------------------
contents=$(dpkg-deb -c "$deb")
for path in \
    ./usr/bin/clipnest \
    ./usr/lib/systemd/user/clipnest.service \
    ./usr/share/dbus-1/services/dev.clipnest.Daemon.service \
    ./usr/share/applications/clipnest.desktop \
    ./usr/share/gnome-shell/extensions/clipnest@clipnest.dev/metadata.json \
    ./usr/share/gnome-shell/extensions/clipnest@clipnest.dev/extension.js \
    ./usr/share/gnome-shell/extensions/clipnest@clipnest.dev/lib/clipboard.js \
    ./usr/share/doc/clipnest/README.md
do
    case "$contents" in
        *"$path"*) pass "$path" ;;
        *) fail "$path is missing from the package" ;;
    esac
done

# The extension the package ships has to be the extension compiled into the
# binary's `setup` as well: both write the same files into the same directory, and
# a user-level copy that disagrees with the packaged one shadows it.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
dpkg-deb -x "$deb" "$work"
for file in metadata.json extension.js lib/clipboard.js; do
    shipped="$work/usr/share/gnome-shell/extensions/clipnest@clipnest.dev/$file"
    if cmp -s "$shipped" "$repo_dir/extension/$file"; then
        pass "extension/$file matches the repository"
    else
        fail "extension/$file in the package differs from the repository copy"
    fi
done

# The version the binary reports has to be the version on the package, or
# `doctor` tells a user to restart a daemon that is already the right one.
if [ -x "$work/usr/bin/clipnest" ]; then
    reported=$("$work/usr/bin/clipnest" version 2>/dev/null | head -1)
    case "$reported" in
        *"$version"*) pass "the binary reports $reported" ;;
        *) fail "the binary reports '$reported', the package says $version" ;;
    esac
    # Nothing is missing from the libraries it links against. This is checked
    # before installing, because a missing one is the difference between "install
    # the package" and "install the package and its dependencies first".
    missing=$(ldd "$work/usr/bin/clipnest" 2>/dev/null | grep 'not found' || true)
    if [ -z "$missing" ]; then
        pass "every shared library it links is present here"
    else
        fail "unsatisfied libraries: $missing"
    fi
else
    fail "usr/bin/clipnest is not executable inside the package"
fi

# --- the install itself ----------------------------------------------------
if [ "$(id -u)" -ne 0 ] && [ "$install_pkg" -eq 0 ]; then
    echo "  --   skipping the install: needs root (pass --install, or run under sudo)"
else
    if apt-get install -y "$(cd "$(dirname "$deb")" && pwd)/$(basename "$deb")" >"$work/apt.log" 2>&1; then
        pass "apt-get install resolved every dependency"
    else
        fail "apt-get install failed, see below"
        tail -20 "$work/apt.log"
    fi
    installed=$(dpkg-query -W -f '${Version}' clipnest 2>/dev/null || true)
    [ "$installed" = "$version-1" ] && pass "dpkg says $installed is installed" \
        || fail "dpkg says '${installed:-nothing}' is installed"
    # The systemd unit the package writes is the one a package ships, not the one
    # a user install writes: it points at /usr/bin.
    unit=/usr/lib/systemd/user/clipnest.service
    if grep -q '^ExecStart=/usr/bin/clipnest daemon' "$unit" 2>/dev/null; then
        pass "$unit starts the packaged binary"
    else
        fail "$unit does not start /usr/bin/clipnest"
    fi
fi

echo
if [ "$problems" -eq 0 ]; then
    echo "install smoke test: everything checked out"
else
    echo "install smoke test: $problems problem(s)"
    exit 1
fi
