# Fedora / RHEL / openSUSE spec for the portable tarball `make dist` produces.
#
#   make dist-tarball dist-extra
#   rpmbuild -bb dist/fedora-x86_64/clipnest.spec \
#       --define "_sourcedir $PWD/dist/tarball" --define "_rpmdir $PWD/dist/rpm"
#
# `@VERSION@` below is a placeholder that rpmbuild cannot expand - a Version tag
# has to be a literal. `make dist-extra` (or the sed in release.yml) writes the
# filled-in copy next to this one; passing `--define "version ..."` does *not*
# work and was a bug here until it was measured.
#
# It packages the binary the tarball already contains instead of building one,
# and that is on purpose. The tarball is built against the libraries of the
# oldest supported distribution; a build here would drag the glibc floor up to
# whatever this machine happens to run, and the package would then refuse to
# install on older, still-supported releases for no reason a user could act on.
# Building from source is what the source tarball is for.

%global debug_package %{nil}
Name:           clipnest
Version:        @VERSION@
Release:        1%{?dist}
Summary:        Clipboard history for GNOME on Wayland

# `USERNAME` is the one placeholder `make set-github GH=<account>` replaces, in
# this file and in the PKGBUILD, the README and RELEASING.md at once. Spelling an
# account out here instead would leave this the one file that command does not
# fix, which is how a package ends up pointing at somebody else's repository.
License:        MIT
URL:            https://github.com/imaneliasy549-oss/clipnest
Source0:        clipnest-%{version}-%{_target_cpu}-linux-gnu.tar.gz

Requires:       gtk4 >= 4.8
Requires:       libadwaita >= 1.0.1
Requires:       sqlite-libs
Requires:       glibc >= 2.39
Recommends:     gnome-shell >= 45
# Auto-paste goes through the desktop portal. Without it ClipNest still works,
# it just copies instead of pasting, which is what "Suggests" says.
Suggests:       xdg-desktop-portal

BuildArch:      %{_target_cpu}

# Defined on Fedora and openSUSE; older RHEL-based systems need the fallback.
%{!?_userunitdir:%global _userunitdir %{_prefix}/lib/systemd/user}

%description
ClipNest keeps a searchable history of everything you copy on GNOME/Wayland.
Press Super+Shift+V, type, and pick an entry: it goes back on the clipboard and
is pasted into the window you came from, like the clipboard popup on Windows.

On Wayland the clipboard belongs to the compositor, and Mutter implements no
data-control protocol, so an ordinary background process cannot read it.
ClipNest therefore comes in two halves: a small gnome-shell extension that
forwards clipboard changes, and a GTK4 daemon that stores them in SQLite, shows
the panel and puts an entry back on the clipboard.

Text and images are both supported, entries can be pinned so trimming never
removes them, and the history has size limits that a settings file can adjust.

Run `clipnest setup` once as each user, then log out and back in.

%prep
%setup -q -n clipnest-%{version}-%{_target_cpu}-linux-gnu

%install
mkdir -p %{buildroot}%{_bindir} %{buildroot}%{_datadir}/applications

install -m755 bin/clipnest %{buildroot}%{_bindir}/clipnest

# The `data/deb/` pair, not the `data/` one: those are the copies that name
# /usr/bin/clipnest. The others are for a per-user installation and start
# `%h/.local/bin/clipnest`, which a system-wide package does not create - using
# them here left an RPM whose daemon could never start.
install -Dm644 data/deb/clipnest.service \
    %{buildroot}%{_userunitdir}/clipnest.service
install -Dm644 data/deb/dev.clipnest.Daemon.service \
    %{buildroot}%{_datadir}/dbus-1/services/dev.clipnest.Daemon.service

install -m644 data/clipnest.desktop \
    %{buildroot}%{_datadir}/applications/clipnest.desktop
install -Dm644 data/dev.clipnest.Panel.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/dev.clipnest.Panel.svg

install -d %{buildroot}%{_datadir}/gnome-shell/extensions/clipnest@clipnest.dev
install -m644 data/gnome-shell/clipnest@clipnest.dev/metadata.json \
    data/gnome-shell/clipnest@clipnest.dev/extension.js \
    %{buildroot}%{_datadir}/gnome-shell/extensions/clipnest@clipnest.dev/

%post
# The parts that belong to a user's own session cannot be done from here, and
# saying so once is more useful than a silent half-installation.
echo "ClipNest: run 'clipnest setup' once as each user, then log out and back in."

%files
%license LICENSE
%doc README.md CHANGELOG.md
%{_bindir}/clipnest
%{_userunitdir}/clipnest.service
%{_datadir}/dbus-1/services/dev.clipnest.Daemon.service
%{_datadir}/applications/clipnest.desktop
%{_datadir}/icons/hicolor/scalable/apps/dev.clipnest.Panel.svg
%{_datadir}/gnome-shell/extensions/clipnest@clipnest.dev/metadata.json
%{_datadir}/gnome-shell/extensions/clipnest@clipnest.dev/extension.js

%changelog

* Tue Sep 16 2026 Iman Elyasi <imaneliasy549@gmail.com> - @VERSION@-1
- Initial package for Fedora, RHEL and openSUSE
