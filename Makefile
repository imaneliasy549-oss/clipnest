BIN         := $(HOME)/.local/bin/clipnest
UNIT        := $(HOME)/.config/systemd/user/clipnest.service
# The keybinding `install` registers. Override it per machine, e.g.
#   make install BINDING='<Super>v'
BINDING     ?= <Super><Shift>v
EXT_DIR     := $(HOME)/.local/share/gnome-shell/extensions/clipnest@clipnest.dev
GUARD_DIR   := $(HOME)/.config/systemd/user
PORTAL_UNITS := xdg-desktop-portal.service xdg-desktop-portal-gtk.service
DBUS_DIR    := $(HOME)/.local/share/dbus-1/services
DBUS_SERVICE := $(DBUS_DIR)/dev.clipnest.Daemon.service

# The account the project lives under on GitHub. It appears in the packaging
# metadata and in the instructions, so it is set in one place and substituted
# everywhere: `make set-github GH=yourname`.
#
# The default is read back out of Cargo.toml rather than being a second copy of
# the same fact: `set-github` writes both, so a literal here could disagree with
# it - which is exactly what happened, warning about a placeholder while the
# files held the real account. Empty means "no account set", and the two places
# that care about it say so.
GH       ?= $(shell sed -n 's|^repository *= *"https://github.com/\(.*\)/clipnest".*|\1|p' Cargo.toml | head -1)

# Release artifacts, laid out one directory per target. See dist/README.md once
# they are built, and RELEASING.md for how a release is made.
VERSION  ?= $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH     ?= $(shell uname -m)
DEB_ARCH ?= $(shell dpkg --print-architecture)
OS_ID    ?= $(shell . /etc/os-release 2>/dev/null && echo $$ID)
OS_VER   ?= $(shell . /etc/os-release 2>/dev/null && echo $$VERSION_ID)
DIST     := dist
PORTABLE := clipnest-$(VERSION)-$(ARCH)-linux-gnu
EXT_UUID := clipnest@clipnest.dev
# The .deb goes into a directory named after the *oldest* release it can be
# installed on, which is read back out of the package itself (its `libc6 (>=
# ...)` line) rather than taken from this build host. A package built on 26.04
# declares 2.39 - which is 24.04 - and a directory called `ubuntu-24.04-amd64`
# says that in the name; `ubuntu-26.04-amd64` would say the opposite and be
# true about the wrong thing. Filled in by `dist-deb`; see the ubuntu_release
# macro below for the map from glibc to Ubuntu release.
# (Spaces, not tabs: make reads a tab-indented line inside `define` as the start
# of a recipe and stops with "missing separator".)
define ubuntu_release
  case "$(1)" in \
  2.31) echo 20.04 ;; \
  2.35) echo 22.04 ;; \
  2.39) echo 24.04 ;; \
  2.43) echo 26.04 ;; \
  *) echo "" ;; \
  esac
endef

.PHONY: build check check-rust test-extension bootstrap install uninstall enable \
        disable restart clean deb deb-verify smoke \
        install-portal-guard uninstall-portal-guard \
        dist dist-deb dist-tarball dist-source dist-extra dist-notes dist-verify \
        set-github preflight

build:
	cargo build --release

# Everything the test suite is: the Rust tests, the JavaScript ones, and the
# linter. `check-rust` is the half that needs no gjs or node.
check: check-rust test-extension

check-rust:
	cargo clippy --all-targets -- -D warnings
	cargo test

# The extension's decisions (image fingerprinting, limits, timeouts) are plain
# functions in `extension/lib/clipboard.js`, and this runs them for real. gjs is
# what the extension actually runs on; node is the fallback for a machine without
# GNOME. Neither is a build dependency, so a missing one is reported, not fatal.
test-extension:
	@if command -v gjs >/dev/null 2>&1; then \
	  echo "extension: gjs -m extension/tests/units.js"; \
	  gjs -m extension/tests/units.js; \
	elif command -v node >/dev/null 2>&1; then \
	  echo "extension: node (--experimental-default-type=module)"; \
	  node --experimental-default-type=module extension/tests/units.js; \
	else \
	  echo "extension: skipped, no gjs and no node on PATH"; \
	fi

# What a first build needs, checked rather than installed: changing somebody's
# package manager state is their decision, and a script that quietly does it is
# how a build machine ends up with a compiler nobody asked for.
bootstrap:
	@missing=0; \
	for cmd in cargo pkg-config cc; do \
	  command -v $$cmd >/dev/null 2>&1 || { echo "missing: $$cmd"; missing=1; }; \
	done; \
	for pkg in gtk4 libadwaita-1 sqlite3; do \
	  pkg-config --exists $$pkg 2>/dev/null || { \
	    echo "missing: $$pkg (apt: libgtk-4-dev libadwaita-1-dev libsqlite3-dev)"; missing=1; }; \
	done; \
	command -v cargo-deb >/dev/null 2>&1 || \
	  echo "note: cargo-deb is missing, so 'make deb' will not work (cargo install cargo-deb)"; \
	command -v gjs >/dev/null 2>&1 || command -v node >/dev/null 2>&1 || \
	  echo "note: neither gjs nor node is installed, so 'make test-extension' will skip"; \
	if [ $$missing -eq 0 ]; then echo "build dependencies: all present"; \
	else echo "install the missing ones, then run 'make' again"; exit 1; fi

# `setup` is what puts the extension in place, enables it, registers the
# shortcut and restarts the service - the same command a package install asks the
# user to run, so both installations end in exactly the same state.
install: build
	install -Dm755 target/release/clipnest $(BIN)
	install -Dm644 data/clipnest.service $(UNIT)
	install -d $(DBUS_DIR)
	sed 's|@BIN@|$(BIN)|' data/dbus/dev.clipnest.Daemon.service > $(DBUS_SERVICE)
	systemctl --user daemon-reload
	-systemctl --user disable clipnest
	$(BIN) setup --binding '$(BINDING)'
	@echo
	@echo "ClipNest installed. Log out and back in so gnome-shell picks up the extension."
	@echo
	-$(BIN) doctor

uninstall:
	-$(BIN) remove-shortcut
	-gnome-extensions disable clipnest@clipnest.dev
	-systemctl --user disable --now clipnest
	rm -f $(BIN) $(UNIT) $(DBUS_SERVICE)
	rm -rf $(EXT_DIR)
	systemctl --user daemon-reload
	@echo "ClipNest removed. Log out and back in to unload the extension."

enable:
	systemctl --user enable --now clipnest

disable:
	-systemctl --user disable --now clipnest

restart:
	-systemctl --user restart clipnest

# Opt-in: make the desktop's own portal services survive a bad login instead of
# leaving the session blocked for minutes. See data/portal-guard.conf.
install-portal-guard:
	@for unit in $(PORTAL_UNITS); do \
		install -Dm644 data/portal-guard.conf $(GUARD_DIR)/$$unit.d/10-clipnest-guard.conf; \
		echo "guard installed for $$unit"; \
	done
	systemctl --user daemon-reload
	-systemctl --user restart xdg-desktop-portal.service
	@echo "Portal backends now retry instead of blocking the session."

uninstall-portal-guard:
	@for unit in $(PORTAL_UNITS); do \
		rm -f $(GUARD_DIR)/$$unit.d/10-clipnest-guard.conf; \
		rmdir --ignore-fail-on-non-empty $(GUARD_DIR)/$$unit.d 2>/dev/null || true; \
	done
	systemctl --user daemon-reload
	-systemctl --user restart xdg-desktop-portal.service
	@echo "Portal guard removed."

clean:
	cargo clean

# ---------------------------------------------------------------------------
# Release artifacts
#
# `make dist` fills dist/ with what a release is made of, one directory per
# target platform. It builds only what this machine can build - its own .deb,
# the portable tarball, the source tarball - and writes the recipes for the rest
# (RPM, Arch) plus a note saying which command produces the missing ones. The
# GitHub workflow does the same on the other architectures and distributions
# during a release; see RELEASING.md and dist/README.md.
# ---------------------------------------------------------------------------
dist: check dist-deb dist-tarball dist-source dist-extra dist-notes
	@cd $(DIST) && find . -type f \
		\( -name '*.deb' -o -name '*.rpm' -o -name '*.tar.gz' \) -printf '%P\n' \
		| sort | xargs -r sha256sum > SHA256SUMS
	@echo
	@echo "== $(DIST)/ =="
	@cd $(DIST) && find . -mindepth 1 -maxdepth 2 -printf '%y %p\n' | sort
	@echo
	@if [ -z "$(GH)" ] || [ "$(GH)" = "USERNAME" ]; then \
		echo "!! no GitHub account is set: dist/*/clipnest.spec and dist/*/PKGBUILD"; \
		echo "!! point at a URL that does not exist. Fix it with:"; \
		echo "!!     make set-github GH=<your account>   &&   make dist"; \
		echo; \
	fi
	@echo "Tag v$(VERSION) and push it: the release workflow builds the arm64, RPM"
	@echo "and Fedora-side artifacts and attaches everything to that tag."

# The .deb for this machine's own architecture. cargo-deb asks dpkg-shlibdeps
# what the binary links, so the declared dependencies are the real ones; that is
# also what says which systems can install it (see dist/README.md).
dist-deb: build
	@cargo deb $(CARGO_FLAGS)
# A stale directory from a previous run would sit next to the new one with an
# older version in it, so the .deb directories are rebuilt rather than added to.
	@rm -rf $(DIST)/ubuntu-*
	@deb=target/debian/clipnest_$(VERSION)-1_$(DEB_ARCH).deb; \
	floor=$$(dpkg-deb -f $$deb Depends | tr ',' '\n' \
	        | sed -n 's/.*libc6 (>= \([0-9][0-9.]*\)).*/\1/p' | head -1); \
	ubuntu=$$($(call ubuntu_release,$$floor)); \
	dir=$(DIST)/ubuntu-$${ubuntu:-$(OS_VER)}-$(DEB_ARCH); \
	mkdir -p $$dir; \
	cp $$deb $$dir/; \
	echo "packaged $$dir/$$(basename $$deb)"; \
	echo "          glibc floor $${floor:-unknown} => installable on Ubuntu $${ubuntu:-$(OS_ID) $(OS_VER)} and newer"

# `--offline` by default: this repository is developed where the crate registry
# is a local cache, and cargo-deb would otherwise ask the network for an index it
# already has. A build machine without that cache passes CARGO_FLAGS= .
CARGO_FLAGS ?= --offline

# The tarball for every distribution without a package here: no root, no package
# manager, everything under $HOME. It is also what the RPM spec packages, so both
# layouts have to be inside it - including the `data/deb/` pair, which is what a
# *system-wide* installation starts. Shipping only the per-user copies left the
# RPM with a unit that pointed at ~/.local/bin, that is, a daemon that could not
# start; `data/` mirrors the repository layout instead.
dist-tarball: build
	@rm -rf $(DIST)/tarball
	@mkdir -p $(DIST)/tarball/$(PORTABLE)/bin \
	          $(DIST)/tarball/$(PORTABLE)/data/deb \
	          $(DIST)/tarball/$(PORTABLE)/data/gnome-shell/$(EXT_UUID)
	install -m755 target/release/clipnest $(DIST)/tarball/$(PORTABLE)/bin/clipnest
	install -m644 data/clipnest.service data/dbus/dev.clipnest.Daemon.service \
	                data/clipnest.desktop $(DIST)/tarball/$(PORTABLE)/data/
	install -m644 data/deb/clipnest.service data/deb/dev.clipnest.Daemon.service \
	                $(DIST)/tarball/$(PORTABLE)/data/deb/
	install -m644 data/icons/hicolor/scalable/apps/dev.clipnest.Panel.svg \
	                $(DIST)/tarball/$(PORTABLE)/data/dev.clipnest.Panel.svg
	install -m644 extension/metadata.json extension/extension.js \
	                $(DIST)/tarball/$(PORTABLE)/data/gnome-shell/$(EXT_UUID)/
	install -d $(DIST)/tarball/$(PORTABLE)/data/gnome-shell/$(EXT_UUID)/lib
	install -m644 extension/lib/clipboard.js \
	                $(DIST)/tarball/$(PORTABLE)/data/gnome-shell/$(EXT_UUID)/lib/
	install -m755 packaging/tarball/install.sh packaging/tarball/uninstall.sh \
	                $(DIST)/tarball/$(PORTABLE)/
	install -m644 README.md CHANGELOG.md LICENSE $(DIST)/tarball/$(PORTABLE)/
	@cd $(DIST)/tarball && tar czf $(PORTABLE).tar.gz $(PORTABLE) && rm -rf $(PORTABLE)
	@echo "packaged $(DIST)/tarball/$(PORTABLE).tar.gz"

# The source, for anybody whose distribution is not in dist/ - or whose glibc is
# older than the one the binary above was built against. `make && make install`
# on the target machine can never be too new for it.
dist-source:
	@mkdir -p $(DIST)/source
	tar czf $(DIST)/source/clipnest-$(VERSION).tar.gz \
	    --transform 's,^,clipnest-$(VERSION)/,' \
	    Cargo.toml Cargo.lock Makefile README.md CHANGELOG.md LICENSE \
	    RELEASING.md src extension data debian packaging .github
	@echo "packaged $(DIST)/source/clipnest-$(VERSION).tar.gz"

# The recipes for the distributions this machine cannot package for, with the
# version already filled in so they can be used as they are.
dist-extra: dist-source
	@mkdir -p $(DIST)/arch-$(ARCH) $(DIST)/fedora-$(ARCH)
	@sed -e 's/@VERSION@/$(VERSION)/g' -e 's|github.com/USERNAME|github.com/$(GH)|g' \
	    packaging/rpm/clipnest.spec > $(DIST)/fedora-$(ARCH)/clipnest.spec
	@sha=$$(sha256sum $(DIST)/source/clipnest-$(VERSION).tar.gz | cut -d' ' -f1); \
	sed -e "s/@VERSION@/$(VERSION)/g" -e "s/@SHA256@/$$sha/g" \
	    -e 's|github.com/USERNAME|github.com/$(GH)|g' \
	    packaging/arch/PKGBUILD > $(DIST)/arch-$(ARCH)/PKGBUILD
	@echo "wrote $(DIST)/arch-$(ARCH)/PKGBUILD and $(DIST)/fedora-$(ARCH)/clipnest.spec"

# A note in every directory that would otherwise be missing, saying what belongs
# there, why it is not there, and the one command that produces it. An empty
# directory would look like a mistake; silence would look like it works.
dist-notes:
	@mkdir -p $(DIST)/linux-arm64 $(DIST)/fedora-$(ARCH) $(DIST)/arch-$(ARCH)
	@sed -e 's/@VERSION@/$(VERSION)/g' \
	    packaging/notes/linux-arm64.md > $(DIST)/linux-arm64/README.md
	@sed -e 's/@VERSION@/$(VERSION)/g' \
	    packaging/notes/fedora.md > $(DIST)/fedora-$(ARCH)/README.md
	@sed -e 's/@VERSION@/$(VERSION)/g' \
	    packaging/notes/arch.md > $(DIST)/arch-$(ARCH)/README.md
# The 22.04 directory exists to answer the question rather than to leave a hole -
# *unless* the package really does install there, which is read from the package
# and not assumed. `libc6 (>= 2.35)` is what jammy has; anything newer means the
# answer below is the true one.
	@floor=$$(ls $(DIST)/ubuntu-*/clipnest_*.deb 2>/dev/null | head -1); \
	if [ -n "$$floor" ] && [ "$$(dpkg-deb -f "$$floor" Depends \
	        | tr ',' '\n' | sed -n 's/.*libc6 (>= \([0-9][0-9.]*\)).*/\1/p' \
	        | head -1)" != "2.35" ]; then \
	    mkdir -p $(DIST)/ubuntu-22.04-$(DEB_ARCH); \
	    sed -e 's/@VERSION@/$(VERSION)/g' \
	        packaging/notes/ubuntu-22.04.md > $(DIST)/ubuntu-22.04-$(DEB_ARCH)/README.md; \
	fi
	@cp packaging/dist-README.md $(DIST)/README.md
	@echo "wrote the per-directory notes and $(DIST)/README.md"

# The checks a package manager cannot make for you: that the checksums match the
# files, and that every file the packaging recipes name is really inside the
# tarball they package it from. The RPM recipe once installed a user-level unit
# that the tarball did not carry - a package whose daemon could never start - and
# this is the target that catches that class of mistake.
dist-verify:
	@test -f $(DIST)/SHA256SUMS || { echo "run 'make dist' first"; exit 1; }
	@echo "== checksums =="
	@cd $(DIST) && sha256sum -c SHA256SUMS
	@echo
	@echo "== every file the recipes name is inside the tarball =="
	@rm -rf target/tarcheck && mkdir -p target/tarcheck
	@tar xzf $(DIST)/tarball/$(PORTABLE).tar.gz -C target/tarcheck
# The spec names files relative to the unpacked tarball; install.sh goes through
# `$here`, which is its own directory. Both spellings are checked, and nothing
# else: install.sh's other paths are `$data/...` variables that only exist at run
# time, and reading those as tarball paths was a false alarm the first time this
# target ran.
	@root=target/tarcheck/$(PORTABLE); missing=0; \
	for f in $$(grep -ho 'data/[a-zA-Z0-9_./@-]*' packaging/rpm/clipnest.spec) \
	         $$(grep -ho '\$$here/[a-zA-Z0-9_./@-]*' packaging/tarball/install.sh \
	           | sed 's|.*here/||'); do \
		if [ ! -e "$$root/$$f" ]; then echo "  MISSING from the tarball: $$f"; missing=1; fi; \
	done; \
	if [ $$missing -eq 0 ]; then echo "  all present"; else exit 1; fi
	@echo
	@echo "== the .deb says what it needs =="
	@dpkg-deb -f $(DIST)/ubuntu-*/clipnest_*.deb Package Version Architecture Depends
	@echo
	@echo "== layout =="
	@cd $(DIST) && find . -mindepth 1 -maxdepth 2 -printf '%y %p\n' | sort

# The steps printed at the end differ with the repository: inside a recipe every
# line is part of one shell command (a `#` there comments out the rest of it, not
# just that line), so the two cases are one `if` rather than two comment blocks.
# Saying `git init` to somebody whose repository is on its third release is noise,
# and worse, `git init` there is a step backwards.
#
# Everything a first push trips over, in one read-only pass: an unset git identity
# (git refuses to commit at all), a `USERNAME` left in the metadata (the release
# would point at a repository that does not exist), and a dist/ built before the
# last edit (you would tag the source but publish an older binary). None of these
# are visible from the code, and all of them look like a broken project instead of
# a missing step. Nothing here changes anything; it prints what to run.
preflight:
	@echo "== preflight for publishing $(VERSION) =="
	@ok=1; \
	name=$$(git config user.name 2>/dev/null); mail=$$(git config user.email 2>/dev/null); \
	if [ -n "$$name" ] && [ -n "$$mail" ]; then \
		echo "  [ok]   git identity: $$name <$$mail>"; \
	else \
		echo "  [FAIL] no git identity, so git will not commit"; \
		echo "         git config --global user.name 'Your Name'"; \
		echo "         git config --global user.email 'you@example.com'"; ok=0; \
	fi; \
	if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then \
		echo "  [ok]   credentials: gh is logged in"; \
	elif [ -n "$$(git config --get credential.helper)" ]; then \
		echo "  [ok]   credentials: stored by git ($$(git config --get credential.helper))"; \
	elif ls $$HOME/.ssh/id_* >/dev/null 2>&1; then \
		echo "  [ok]   credentials: ssh key $$(ls $$HOME/.ssh/id_* | head -1)"; \
	else \
		echo "  [warn] neither gh, nor a credential helper, nor an ssh key"; \
		echo "         https push will ask for the account and a token from"; \
		echo "         github.com/settings/tokens (scope: Contents -> read and write)"; \
	fi; \
	if grep -q '^homepage' Cargo.toml; then \
		account=$$(sed -n 's|^homepage = "https://github.com/\([^/]*\)/clipnest"|\1|p' Cargo.toml | head -1); \
		echo "  [ok]   github account in Cargo.toml: $$account"; \
	else \
		echo "  [FAIL] Cargo.toml still has homepage commented out"; \
		echo "         make set-github GH=<your github account>"; ok=0; \
	fi; \
	if grep -rqs 'github\.com/USERNAME' README.md RELEASING.md packaging Cargo.toml; then \
		echo "  [FAIL] github.com/USERNAME is still in README/RELEASING/packaging"; ok=0; \
	else echo "  [ok]   no USERNAME placeholder left in the metadata"; fi; \
	deb=$$(ls $(DIST)/ubuntu-*/clipnest_$(VERSION)-1_*.deb 2>/dev/null | head -1); \
	if [ -z "$$deb" ]; then \
		echo "  [FAIL] no $(VERSION) .deb under $(DIST)/, so there is nothing to attach"; \
		echo "         make dist"; ok=0; \
	elif [ -n "$$(find src extension data Cargo.toml -newer "$$deb" -print -quit 2>/dev/null)" ]; then \
		echo "  [FAIL] $(DIST) source files are newer than $$deb: that package predates them"; \
		echo "         make dist"; ok=0; \
	else echo "  [ok]   $(DIST)/ has a $(VERSION) .deb newer than the sources"; fi; \
	if grep -qx '/target' .gitignore && grep -qx '/dist' .gitignore; then \
		echo "  [ok]   .gitignore keeps /target and /dist out of the repository"; \
	else echo "  [FAIL] .gitignore must list /target and /dist"; ok=0; fi; \
	if git rev-parse --git-dir >/dev/null 2>&1; then \
		echo "  [info] repository: $$(git rev-parse --abbrev-ref HEAD), origin $$(git remote get-url origin 2>/dev/null || echo none)"; \
	else echo "  [info] not a repository yet: 'git init -b main' is the first step"; fi; \
	if command -v curl >/dev/null 2>&1; then \
		code=$$(timeout 8 curl -sS -o /dev/null -w '%{http_code}' https://api.github.com 2>/dev/null); \
		if [ "$$code" = 200 ]; then echo "  [ok]   api.github.com reachable"; \
		else echo "  [warn] api.github.com answered '$$code': the push itself may still work"; fi; \
	fi; \
	echo; \
	if [ $$ok -eq 1 ]; then \
		echo "  ready to push. next:"; \
	else \
		echo "  fix the [FAIL] lines above, then run 'make preflight' again. next:"; \
	fi; \
	if git rev-parse --git-dir >/dev/null 2>&1; then \
		echo "    git add -A && git status --short      # review, then:"; \
		echo "    git commit -m 'ClipNest $(VERSION)'"; \
		echo "    git push origin main"; \
	else \
		echo "    git init -b main && git add . && git status --short"; \
		echo "    git commit -m 'ClipNest $(VERSION)'"; \
		echo "    git remote add origin https://github.com/<account>/clipnest.git"; \
		echo "    git push -u origin main"; \
	fi; \
	echo "    git tag -a v$(VERSION) -m 'ClipNest $(VERSION)' && git push origin v$(VERSION)"; \
	echo; \
	if [ $$ok -eq 1 ]; then exit 0; else exit 1; fi

# Fills in the GitHub account everywhere it is named, so a release does not go
# out with USERNAME in its metadata. Cargo.toml keeps those two lines commented
# until this runs, so the package's Homepage and Repository fields stay empty
# until there is a repository to point at.
set-github:
	@test -n "$(GH)" -a "$(GH)" != "USERNAME" || \
		{ echo "usage: make set-github GH=<github account>"; exit 1; }
# One -e per expression, and every backslash-newline outside the quotes. Inside
# single quotes the shell keeps `\` and the newline literally, and sed then reads
# them as part of the script ("unterminated address regex") - which is exactly how
# the Cargo.toml half of this target used to fail: it had already rewritten the
# spec, the PKGBUILD and RELEASING.md with the account name and left Cargo.toml
# untouched, half a release.
	@sed -i -e 's|github\.com/USERNAME|github.com/$(GH)|g' \
		README.md RELEASING.md packaging/rpm/clipnest.spec packaging/arch/PKGBUILD
	@sed -i -e 's|^# homepage = .*|homepage = "https://github.com/$(GH)/clipnest"|' \
		-e 's|^# repository = .*|repository = "https://github.com/$(GH)/clipnest"|' \
		Cargo.toml
	@echo "GitHub account set to $(GH):"
	@grep -n "github.com/$(GH)" Cargo.toml | head -4

# The package, checked without installing anything: metadata and dependencies,
# the file list with its modes, and the packaged binary actually running from a
# scratch directory.
deb-verify: deb
	@deb=$$(ls -t target/debian/clipnest_*.deb | head -1); \
	echo "== $$deb =="; \
	dpkg-deb --info "$$deb"; \
	dpkg-deb --contents "$$deb"; \
	rm -rf target/deb-check; mkdir -p target/deb-check; \
	dpkg-deb --extract "$$deb" target/deb-check; \
	target/deb-check/usr/bin/clipnest version; \
	target/deb-check/usr/bin/clipnest help > /dev/null; \
	target/deb-check/usr/bin/clipnest config | head -4; \
	echo "the packaged binary runs"

deb: build
	cargo deb $(CARGO_FLAGS)
	@ls -l target/debian/*.deb
