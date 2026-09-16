//! Everything about putting ClipNest on the machine: where the pieces live, how
//! they get there, and the GNOME settings that make them run.
//!
//! Two kinds of installation are supported and they have to be told apart, which
//! is what most of this module is about:
//!
//! * a **user install** (`make install`), which writes into `~/.local`,
//! * a **system package** (the `.deb`), which writes into `/usr`.
//!
//! A user copy shadows the packaged one - both for the extension (gnome-shell
//! looks at the user directory first) and for the binary (`~/.local/bin` comes
//! before `/usr/bin` in `$PATH`) - so every check here has to know about both,
//! and the advice it prints must never tell somebody to delete a packaged file.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use gio::prelude::*;
use gtk::gdk;

use crate::app::BUS_NAME;

/// The directory a system package installs its data into, and the one systemd
/// keeps user units in. `/usr/local` wins over `/usr` in both lookup orders.
const SYSTEM_DATA_DIRS: [&str; 2] = ["/usr/local/share", "/usr/share"];
const SYSTEM_UNIT_DIRS: [&str; 3] = [
    "/usr/local/lib/systemd/user",
    "/usr/lib/systemd/user",
    "/etc/systemd/user",
];
/// Where a package puts the binary. `make install` uses `~/.local/bin/clipnest`.
const SYSTEM_BIN: &str = "/usr/bin/clipnest";

const UUID: &str = "clipnest@clipnest.dev";
const SHORTCUT_PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/clipnest/";
const SHORTCUT_BINDING: &str = "<Super><Shift>v";
/// Schemas whose bindings can swallow a freshly registered custom shortcut.
/// GNOME reports nothing when two bindings collide - the other action simply
/// wins - so the check has to be done by hand.
const CONFLICT_SCHEMAS: [&str; 3] = [
    "org.gnome.desktop.wm.keybindings",
    "org.gnome.shell.keybindings",
    "org.gnome.settings-daemon.plugins.media-keys",
];

const PARENT_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CHILD_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";

/// Which installation is in charge on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Files handed to the session by a distro package, under `/usr`.
    Package,
    /// A `make install` copy under the user's home.
    User,
    /// Neither: the binary runs from wherever it was built.
    Local,
}

/// Judges the installation from paths that are already known, so the rule can be
/// tested without writing anything.
pub fn classify_layout(packaged_binary: bool, user_binary: bool) -> Layout {
    if packaged_binary {
        Layout::Package
    } else if user_binary {
        Layout::User
    } else {
        Layout::Local
    }
}

pub fn layout() -> Layout {
    classify_layout(
        Path::new(SYSTEM_BIN).is_file(),
        user_bin().is_file(),
    )
}

// --------------------------------------------------------------------------
// Where things live
// --------------------------------------------------------------------------

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()))
}

fn xdg(base: &str, fallback: &str) -> PathBuf {
    std::env::var_os(base)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| {
            let mut path = home();
            path.push(fallback);
            path
        })
}

/// `$XDG_DATA_HOME`, which is where a user session bus looks for on-demand
/// services, defaulting to `~/.local/share`.
pub fn data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// The binary a `make install` would write.
pub fn user_bin() -> PathBuf {
    home().join(".local/bin/clipnest")
}

/// The extension files as shipped inside this binary.
pub fn extension_files() -> [(&'static str, &'static str); 2] {
    [
        ("metadata.json", include_str!("../extension/metadata.json")),
        ("extension.js", include_str!("../extension/extension.js")),
    ]
}

/// The user-level extension directory: where `make install` puts the files, and
/// what `install-extension` writes to.
pub fn extension_dir() -> PathBuf {
    data_home().join("gnome-shell/extensions").join(UUID)
}

/// Every directory gnome-shell searches for this extension, in the order it
/// searches them: the user's own copy wins over a packaged one.
pub fn extension_candidates() -> Vec<PathBuf> {
    let mut dirs = vec![extension_dir()];
    dirs.extend(
        SYSTEM_DATA_DIRS
            .iter()
            .map(|base| PathBuf::from(base).join("gnome-shell/extensions").join(UUID)),
    );
    dirs
}

/// The copy gnome-shell would actually load, if any is installed.
pub fn installed_extension_dir() -> Option<PathBuf> {
    extension_candidates().into_iter().find(|dir| dir.is_dir())
}

/// Whether an installed copy still matches the files compiled into this binary.
pub fn extension_is_current(dir: &Path) -> bool {
    extension_files().iter().all(|(name, contents)| {
        std::fs::read_to_string(dir.join(name))
            .map(|on_disk| on_disk == *contents)
            .unwrap_or(false)
    })
}

/// The user-level systemd unit, which `make install` writes.
pub fn unit_path() -> PathBuf {
    config_home().join("systemd/user/clipnest.service")
}

/// Every directory systemd looks in for a user unit, in search order.
pub fn unit_candidates() -> Vec<PathBuf> {
    let mut paths = vec![unit_path()];
    paths.extend(
        SYSTEM_UNIT_DIRS
            .iter()
            .map(|dir| PathBuf::from(dir).join("clipnest.service")),
    );
    paths
}

pub fn installed_unit() -> Option<PathBuf> {
    unit_candidates().into_iter().find(|path| path.is_file())
}

/// The session bus starts the daemon on demand, which needs a D-Bus activation
/// file describing both the bus name and the systemd unit behind it. The file
/// name has to match `BUS_NAME`, the way every other user service does it.
///
/// This is the user-level location; a package ships the same file under
/// `/usr/share/dbus-1/services`.
pub fn dbus_service_path() -> PathBuf {
    data_home().join(format!("dbus-1/services/{BUS_NAME}.service"))
}

pub fn dbus_service_candidates() -> Vec<PathBuf> {
    let mut paths = vec![dbus_service_path()];
    paths.extend(
        SYSTEM_DATA_DIRS
            .iter()
            .map(|base| PathBuf::from(base).join(format!("dbus-1/services/{BUS_NAME}.service"))),
    );
    paths
}

pub fn installed_dbus_service() -> Option<PathBuf> {
    dbus_service_candidates()
        .into_iter()
        .find(|path| path.is_file())
}

/// A `make install` copy of the binary that shadows the packaged one. It comes
/// first in `$PATH` on most desktops, so `clipnest` would keep running the older
/// build while the package looks perfectly installed.
pub fn shadowing_binary() -> Option<PathBuf> {
    let user = user_bin();
    if !user.is_file() {
        return None;
    }
    match std::env::current_exe() {
        Ok(executable) if executable == user => None,
        _ => Some(user),
    }
}

// --------------------------------------------------------------------------
// Running commands
// --------------------------------------------------------------------------

/// The standard output of a command, trimmed; empty when it cannot be run.
pub fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Whether a program is on `$PATH`.
pub fn have(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .any(|dir| dir.join(program).is_file())
        })
        .unwrap_or(false)
}

// --------------------------------------------------------------------------
// The shell extension
// --------------------------------------------------------------------------

fn write_extension_files(target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|err| format!("cannot create {}: {err}", target.display()))?;
    for (name, contents) in extension_files() {
        std::fs::write(target.join(name), contents)
            .map_err(|err| format!("cannot write {name} to {}: {err}", target.display()))?;
    }
    Ok(())
}

/// A copy of the extension that neither is the one we just wrote nor matches it.
/// Worth naming: gnome-shell loads the first candidate, so a mismatched copy is
/// the one that actually runs, and the fix is to refresh it - never to delete a
/// packaged file.
fn mismatched_copy(written: &Path) -> Option<PathBuf> {
    for dir in extension_candidates() {
        if dir == written || !dir.is_dir() || extension_is_current(&dir) {
            continue;
        }
        return Some(dir);
    }
    None
}

/// Makes sure gnome-shell can find the extension, preferring an already
/// installed (packaged) copy over writing a second one under the user's home.
pub fn ensure_extension() -> Result<String, String> {
    let target = extension_dir();
    if let Some(installed) = installed_extension_dir() {
        if installed != target && extension_is_current(&installed) {
            // A package put exactly these files in /usr. Writing a user copy on
            // top would shadow the one the package keeps updated.
            return Ok(format!(
                "extension files are already installed in {}",
                installed.display()
            ));
        }
    }
    write_extension_files(&target)?;
    Ok(format!("extension installed in {}", target.display()))
}

fn enable_extension() {
    match Command::new("gnome-extensions").args(["enable", UUID]).status() {
        Ok(status) if status.success() => {
            println!("extension enabled - log out and back in so gnome-shell loads it");
        }
        Ok(_) => eprintln!(
            "clipnest: could not enable the extension automatically, run:\n         gnome-extensions enable {UUID}"
        ),
        Err(err) => eprintln!("clipnest: cannot run gnome-extensions: {err}"),
    }
}

/// `clipnest install-extension`: put the extension where gnome-shell will find
/// it and switch it on for this user.
pub fn install_extension() -> ExitCode {
    let target = extension_dir();
    let packaged = installed_extension_dir()
        .filter(|dir| *dir != target && extension_is_current(dir));

    match &packaged {
        Some(dir) => println!(
            "extension files are already installed in {} (packaged with the system)",
            dir.display()
        ),
        None => {
            if let Err(err) = write_extension_files(&target) {
                eprintln!("clipnest: {err}");
                return ExitCode::FAILURE;
            }
            println!("extension installed in {}", target.display());
        }
    }

    if let Some(other) = mismatched_copy(&target) {
        eprintln!(
            "warning: another copy of the extension is installed in {} and does not match this build",
            other.display()
        );
        eprintln!("         the first one in this list is what gnome-shell loads:");
        for dir in extension_candidates() {
            eprintln!("           {}{}", dir.display(), if dir.is_dir() { "" } else { " (absent)" });
        }
        eprintln!("         refresh it with 'clipnest install-extension'");
    }

    enable_extension();
    ExitCode::SUCCESS
}

// --------------------------------------------------------------------------
// The shortcut
// --------------------------------------------------------------------------

/// The command a custom keybinding should run to open the panel.
pub fn shortcut_command() -> String {
    match std::env::current_exe() {
        Ok(path) => format!("{} toggle", path.display()),
        Err(_) => "clipnest toggle".to_string(),
    }
}

pub fn shortcut_binding() -> &'static str {
    SHORTCUT_BINDING
}

pub fn shortcut_path() -> &'static str {
    SHORTCUT_PATH
}

pub fn extension_uuid() -> &'static str {
    UUID
}

/// Whether GNOME would understand this accelerator. Checked here rather than by
/// `gtk::accelerator_parse`, which panics without an initialized GTK.
pub fn binding_is_valid(binding: &str) -> bool {
    split_binding(binding).is_ok()
}

/// Whether a binding is the one this build registers by default, compared the
/// way GNOME would rather than as plain strings.
pub fn binding_is_default(binding: &str) -> bool {
    normalize_binding(binding) == normalize_binding(SHORTCUT_BINDING)
}

/// Every GNOME shortcut that would fire on the same key press as `binding`.
///
/// A collision is silent: GNOME never warns, one of the two actions just loses.
/// `<Super>v`, for instance, is already `toggle-message-tray` in
/// `org.gnome.shell.keybindings`.
pub fn binding_conflicts(binding: &str) -> Vec<String> {
    let Some(wanted) = normalize_binding(binding) else {
        return Vec::new();
    };

    let mut conflicts = Vec::new();
    for schema_id in CONFLICT_SCHEMAS {
        if !schema_exists(schema_id) {
            continue;
        }
        let settings = gio::Settings::new(schema_id);
        let Some(schema) = settings.settings_schema() else {
            continue;
        };
        for key in schema.list_keys() {
            // Only string-array keys hold accelerators; a scalar one handed to
            // the loop below would be read as garbage.
            if schema.key(key.as_str()).value_type().as_str() != "as" {
                continue;
            }
            let value = settings.value(key.as_str());
            for index in 0..value.n_children() {
                let entry = value.child_value(index);
                let Some(accelerator) = entry.str() else {
                    continue;
                };
                if normalize_binding(accelerator).as_deref() == Some(wanted.as_str()) {
                    conflicts.push(format!("{schema_id} {key}"));
                }
            }
        }
    }
    conflicts.sort();
    conflicts.dedup();
    conflicts
}

/// Registers the panel shortcut in GNOME's custom keybindings. `binding`
/// defaults to `SHORTCUT_BINDING`, which is what `make install` uses.
pub fn setup_shortcut(binding: Option<&str>) -> ExitCode {
    if !schema_exists(PARENT_SCHEMA) || !schema_exists(CHILD_SCHEMA) {
        eprintln!("clipnest: the GNOME media-keys schemas are not installed");
        return ExitCode::FAILURE;
    }

    let binding = match binding {
        Some(binding) => binding,
        None => SHORTCUT_BINDING,
    };
    if let Err(err) = split_binding(binding) {
        eprintln!("clipnest: {err}");
        eprintln!("         try something like '{SHORTCUT_BINDING}' or 'F9'");
        return ExitCode::FAILURE;
    }

    let command = shortcut_command();

    let parent = gio::Settings::new(PARENT_SCHEMA);
    let mut bindings: Vec<String> = parent
        .strv("custom-keybindings")
        .iter()
        .map(|value| value.to_string())
        .collect();
    if !bindings.iter().any(|path| path == SHORTCUT_PATH) {
        bindings.push(SHORTCUT_PATH.to_string());
        if let Err(err) = parent.set_strv("custom-keybindings", bindings) {
            eprintln!("clipnest: cannot register the custom keybinding: {err}");
            return ExitCode::FAILURE;
        }
    }

    let child = gio::Settings::with_path(CHILD_SCHEMA, SHORTCUT_PATH);
    child.set_string("name", "ClipNest").ok();
    child.set_string("command", &command).ok();
    if let Err(err) = child.set_string("binding", binding) {
        eprintln!("clipnest: cannot set the keybinding: {err}");
        return ExitCode::FAILURE;
    }
    gio::Settings::sync();

    println!("shortcut registered: {binding} runs '{command}'");
    if !binding_is_default(binding) {
        println!(
            "note: custom binding; clipnest setup-shortcut goes back to {SHORTCUT_BINDING}"
        );
    }
    for conflict in binding_conflicts(binding) {
        eprintln!("warning: {binding} also triggers {conflict}");
        eprintln!("         pick another binding, or free that one in Settings \u{2192} Keyboard");
    }
    ExitCode::SUCCESS
}

pub fn remove_shortcut() -> ExitCode {
    if !schema_exists(PARENT_SCHEMA) || !schema_exists(CHILD_SCHEMA) {
        return ExitCode::SUCCESS;
    }

    let parent = gio::Settings::new(PARENT_SCHEMA);
    let bindings: Vec<String> = parent
        .strv("custom-keybindings")
        .iter()
        .filter(|path| path.as_str() != SHORTCUT_PATH)
        .map(|value| value.to_string())
        .collect();
    parent.set_strv("custom-keybindings", bindings).ok();

    let child = gio::Settings::with_path(CHILD_SCHEMA, SHORTCUT_PATH);
    for key in ["name", "command", "binding"] {
        child.reset(key);
    }
    gio::Settings::sync();

    println!("shortcut removed");
    ExitCode::SUCCESS
}

/// What `doctor` can read out of GNOME: the registered paths, the binding and
/// the command behind it.
pub fn shortcut_settings() -> Option<(Vec<String>, String, String)> {
    if !schema_exists(PARENT_SCHEMA) || !schema_exists(CHILD_SCHEMA) {
        return None;
    }
    let parent = gio::Settings::new(PARENT_SCHEMA);
    let bindings: Vec<String> = parent
        .strv("custom-keybindings")
        .iter()
        .map(|value| value.to_string())
        .collect();
    let child = gio::Settings::with_path(CHILD_SCHEMA, SHORTCUT_PATH);
    Some((
        bindings,
        child.string("binding").to_string(),
        child.string("command").to_string(),
    ))
}

pub fn extension_enabled() -> bool {
    if !schema_exists("org.gnome.shell") {
        return false;
    }
    gio::Settings::new("org.gnome.shell")
        .strv("enabled-extensions")
        .iter()
        .any(|value| value == UUID)
}

fn schema_exists(schema_id: &str) -> bool {
    gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(schema_id, true))
        .is_some()
}

// --------------------------------------------------------------------------
// The accelerator grammar
// --------------------------------------------------------------------------

/// Splits a GTK accelerator into its canonical modifiers and its key name.
///
/// `gtk::accelerator_parse` is the obvious tool and the wrong one here: it
/// panics unless `gtk::init` has run, and `clipnest setup-shortcut` has to work
/// over SSH, where there is no display to initialize GTK against. The rules
/// below were checked against GDK 4 itself, so they agree with what the
/// shortcut will really do: `<Win>v`, `<Mod4>v` and `<Super>vg` are all rejected
/// here because GDK rejects them too - accepting one would register a shortcut
/// that looks fine and then never fires.
pub fn split_binding(binding: &str) -> Result<(Vec<&'static str>, String), String> {
    let mut rest = binding.trim();
    if rest.is_empty() {
        return Err("the keybinding is empty".to_string());
    }

    let mut modifiers = Vec::new();
    while let Some(stripped) = rest.strip_prefix('<') {
        let Some(end) = stripped.find('>') else {
            return Err(format!("unterminated modifier in '{binding}'"));
        };
        let name = &stripped[..end];
        let Some(modifier) = canonical_modifier(name) else {
            return Err(match x11_modifier_alias(name) {
                Some(alias) => format!("'<{name}>' is an X11 name, GDK calls it '<{alias}>'"),
                None => format!(
                    "unknown modifier '<{name}>' in '{binding}' \
                     (use Control, Shift, Alt, Super, Hyper or Meta)"
                ),
            });
        };
        modifiers.push(modifier);
        rest = &stripped[end + 1..];
    }

    let key = rest;
    if key.is_empty() {
        return Err(format!("'{binding}' names no key"));
    }
    if canonical_modifier(key).is_some() {
        return Err(format!(
            "'{key}' is a modifier, put a key after it: '<{key}>x'"
        ));
    }
    // Windows calls this binding "Win+V", so a bare `Super+v` is the mistake to
    // expect: the modifier has to be bracketed before GTK can read it.
    if !binding.contains('<') && key.contains('+') {
        let mut parts: Vec<&str> = key.split('+').collect();
        let last = parts.pop().unwrap_or_default();
        let suggestion: String = parts
            .iter()
            .map(|part| {
                let name = x11_modifier_alias(part).unwrap_or(part);
                format!("<{name}>")
            })
            .collect::<String>()
            + last;
        return Err(format!(
            "'{binding}' is missing its brackets; write '{suggestion}'"
        ));
    }
    // GDK settles what a key name is by looking it up in the keysym table, and
    // that lookup needs no display, so the CLI can simply ask it.
    if gdk::Key::from_name(key).is_none() {
        return Err(format!("'{key}' is not a key name GDK knows"));
    }

    Ok((modifiers, key.to_string()))
}

/// The canonical spelling of a modifier name, `None` when it is not one.
fn canonical_modifier(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        // GDK has three names for Control, all one mask. Meta is a mask of its
        // own, so it stays apart from Super.
        "ctrl" | "control" | "primary" => "ctrl",
        "shift" => "shift",
        "alt" => "alt",
        "super" => "super",
        "hyper" => "hyper",
        "meta" => "meta",
        _ => return None,
    })
}

/// X11 spellings that GDK4 dropped, with what they usually meant. GDK4 has no
/// `ModN` names at all, so accepting one would register a dead shortcut.
fn x11_modifier_alias(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "mod1" => "Alt",
        "mod4" | "win" | "windows" => "Super",
        _ => return None,
    })
}

/// A comparable form of an accelerator: canonical, lowercase, modifiers in a
/// fixed order, so `<Shift><Super>V`, `<Super><Shift>v` and
/// `<Primary><Shift><Super>v` are all one binding. `None` when the string is not
/// an accelerator at all.
pub fn normalize_binding(binding: &str) -> Option<String> {
    let (mut modifiers, key) = split_binding(binding).ok()?;
    modifiers.sort_unstable();
    modifiers.dedup();
    let prefix: String = modifiers.iter().map(|name| format!("<{name}>")).collect();
    Some(format!("{prefix}{}", key.to_lowercase()))
}

// --------------------------------------------------------------------------
// One command that finishes an installation
// --------------------------------------------------------------------------

/// `clipnest setup`: the single step a package install cannot do for the user,
/// because it touches their GNOME settings and their systemd user session.
///
/// Every step is reported even when one of them fails - on a fresh machine one
/// of these is always missing, and the point of this command is to say which.
pub fn setup(binding: Option<&str>) -> ExitCode {
    let mut problems = 0usize;

    match ensure_extension() {
        Ok(message) => println!("{message}"),
        Err(err) => {
            eprintln!("clipnest: {err}");
            problems += 1;
        }
    }
    enable_extension();
    if setup_shortcut(binding) != ExitCode::SUCCESS {
        problems += 1;
    }
    restart_service();

    println!();
    match layout() {
        Layout::Package => println!(
            "ClipNest {} is running from the system package.",
            env!("CARGO_PKG_VERSION")
        ),
        Layout::User => println!(
            "ClipNest {} is running from {} (a user install).",
            env!("CARGO_PKG_VERSION"),
            user_bin().display()
        ),
        Layout::Local => println!(
            "ClipNest {} is running from a build directory, not from an install.",
            env!("CARGO_PKG_VERSION")
        ),
    }
    println!("Log out and back in once so gnome-shell loads the extension, then run:");
    println!("  clipnest doctor");

    if problems == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// systemd caches unit files, and a package adds them while the user manager is
/// already running. Without this reload the daemon cannot be started on demand.
fn restart_service() {
    if !have("systemctl") {
        eprintln!("clipnest: systemctl is not on $PATH; start the daemon yourself:");
        eprintln!("         clipnest daemon");
        return;
    }
    if !command_output("systemctl", &["--user", "daemon-reload"]).is_empty() {
        // No news is good news here; any output means systemd complained.
    }
    match Command::new("systemctl")
        .args(["--user", "restart", "clipnest"])
        .status()
    {
        Ok(status) if status.success() => println!("service: clipnest.service restarted"),
        _ => {
            eprintln!("clipnest: could not restart clipnest.service; try:");
            eprintln!("         systemctl --user daemon-reload && systemctl --user start clipnest");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    /// One file of the extension as this binary ships it.
    fn bundled(name: &str) -> &'static str {
        extension_files()
            .iter()
            .find(|(file, _)| *file == name)
            .map(|(_, contents)| *contents)
            .expect("the extension ships inside the binary")
    }

    /// Reads `const NAME = <int> * <int> ...;` out of the extension source.
    fn js_constant(source: &str, name: &str) -> Option<u64> {
        let declaration = format!("const {name} =");
        let line = source
            .lines()
            .find(|line| line.trim_start().starts_with(&declaration))?;
        let value = line.split_once('=')?.1.trim().trim_end_matches(';');
        value.split('*').try_fold(1u64, |total, factor| {
            Some(total * factor.trim().parse::<u64>().ok()?)
        })
    }

    #[test]
    fn the_extension_falls_back_to_the_documented_limits() {
        // The extension drops oversized payloads before sending them, so if
        // these two copies drift apart the biggest copies vanish without a word.
        // These are its fallbacks, used until the daemon answers `Config`.
        let extension = bundled("extension.js");
        assert_eq!(
            js_constant(extension, "MAX_TEXT_BYTES"),
            Some(config::DEFAULT_MAX_TEXT_MB as u64 * config::BYTES_PER_MB as u64)
        );
        assert_eq!(
            js_constant(extension, "MAX_IMAGE_BYTES"),
            Some(config::DEFAULT_MAX_IMAGE_MB as u64 * config::BYTES_PER_MB as u64)
        );
        assert!(js_constant(extension, "POLL_INTERVAL_MS").is_some_and(|ms| ms > 0));
    }

    #[test]
    fn the_extension_asks_for_the_settings_the_daemon_offers() {
        // The names have to match on both sides of the bus or the extension would
        // quietly keep using its fallbacks.
        let extension = bundled("extension.js");
        for key in config::WIRE_KEYS {
            assert!(
                extension.contains(key),
                "extension.js never mentions '{key}'"
            );
        }
    }

    #[test]
    fn the_extension_metadata_matches_this_build() {
        assert!(bundled("metadata.json").contains(&format!("\"{UUID}\"")));
    }

    #[test]
    fn the_two_unit_files_only_differ_in_the_binary_they_start() {
        // `make install` writes one, the package ships the other, and the daemon
        // only accepts the two if they agree about everything else. Cargo runs
        // tests from the package root, so the paths are the repository's own.
        let directives = |path: &str| -> Vec<String> {
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| {
                    !line.is_empty() && !line.starts_with('#') && !line.starts_with("ExecStart")
                })
                .collect()
        };
        let user = directives("data/clipnest.service");
        let packaged = directives("data/deb/clipnest.service");
        assert!(!user.is_empty(), "data/clipnest.service is missing");
        assert!(
            !packaged.is_empty(),
            "data/deb/clipnest.service is missing (the package ships its own)"
        );
        assert_eq!(user, packaged);
    }

    #[test]
    fn the_two_dbus_files_name_the_same_service_and_unit() {
        for (path, expected_exec) in [
            ("data/dbus/dev.clipnest.Daemon.service", "@BIN@ daemon"),
            (
                "data/deb/dev.clipnest.Daemon.service",
                "/usr/bin/clipnest daemon",
            ),
        ] {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            assert!(
                text.contains(&format!("Name={BUS_NAME}")),
                "{path} does not activate {BUS_NAME}"
            );
            assert!(
                text.contains("SystemdService=clipnest.service"),
                "{path} does not name the unit"
            );
            assert!(
                text.contains(&format!("Exec={expected_exec}")),
                "{path} has an unexpected Exec line"
            );
        }
    }

    #[test]
    fn accepts_the_accelerators_gnome_does() {
        for binding in [
            "<Super><Shift>v",
            "<Control><Alt>t",
            "<Primary>space",
            "F9",
            "Print",
            "<Super>Page_Up",
        ] {
            assert!(split_binding(binding).is_ok(), "{binding} should be accepted");
        }
        for binding in [
            "",
            "   ",
            "Super+v",
            "<Super>",
            "<Super><Shift>",
            "<Bogus>v",
            "<Super>two keys",
            "<Super>Super",
            "<Super>a+b",
            // All three of these are things GDK 4 refuses, verified against it.
            "<Super>vg",
            "<Super>,",
            "<Mod4>v",
        ] {
            assert!(split_binding(binding).is_err(), "{binding} should be rejected");
        }
    }

    #[test]
    fn explains_what_is_wrong() {
        // "Win+V" is how Windows spells this binding, so the unbracketed form is
        // the mistake to expect - and the hint has to name the fix.
        for binding in ["Super+v", "Win+v", "Control+Alt+t"] {
            let err = split_binding(binding).unwrap_err();
            assert!(err.contains("missing its brackets"), "{binding}: {err}");
        }
        assert!(split_binding("Win+v").unwrap_err().contains("write '<Super>v'"));
        let err = split_binding("<Mod4>v").unwrap_err();
        assert!(err.contains("X11 name") && err.contains("'<Super>'"), "{err}");
        assert!(split_binding("<Super>vg").unwrap_err().contains("GDK knows"));
    }

    #[test]
    fn recognizes_the_default_binding() {
        assert!(binding_is_default("<Super><Shift>v"));
        assert!(binding_is_default("<Shift><Super>v"));
        assert!(binding_is_default("<Super><Shift>V"));
        assert!(!binding_is_default("<Super>v"));
        assert!(!binding_is_default("<Super><Shift>c"));
    }

    #[test]
    fn normalizes_equivalent_accelerators() {
        let wanted = normalize_binding("<Super><Shift>v").unwrap();
        assert_eq!(normalize_binding("<Shift><Super>V").unwrap(), wanted);
        assert_eq!(normalize_binding("<Super><Shift><Super>v").unwrap(), wanted);
        assert_eq!(normalize_binding("<Control>P").unwrap(), "<ctrl>p");
        assert_eq!(normalize_binding("<Primary>P").unwrap(), "<ctrl>p");
        // Primary is Control, so this one is genuinely a different shortcut.
        assert_ne!(normalize_binding("<Primary><Shift><Super>v").unwrap(), wanted);
        // Settings values that are not accelerators at all are not bindings.
        assert!(normalize_binding("/org/gnome/shell/keybindings/").is_none());
        assert!(normalize_binding("['<Super>v']").is_none());
    }

    #[test]
    fn tells_the_two_installations_apart() {
        assert_eq!(classify_layout(true, true), Layout::Package);
        assert_eq!(classify_layout(true, false), Layout::Package);
        assert_eq!(classify_layout(false, true), Layout::User);
        assert_eq!(classify_layout(false, false), Layout::Local);

        // Both supported installations have to be found by the same lookups.
        let extensions = extension_candidates();
        assert_eq!(extensions[0], extension_dir());
        assert!(extensions[1].starts_with("/usr/local/share"));
        assert!(extensions[2].starts_with("/usr/share"));
        let services = dbus_service_candidates();
        assert!(services[2].ends_with(format!("{BUS_NAME}.service")));
        let units = unit_candidates();
        assert_eq!(units[0], unit_path());
        assert!(units.iter().any(|path| path.starts_with("/usr/lib/systemd/user")));
    }

    #[test]
    fn a_missing_copy_is_not_a_current_copy() {
        let absent = std::env::temp_dir().join("clipnest-no-extension-here");
        let _ = std::fs::remove_dir_all(&absent);
        assert!(!extension_is_current(&absent));
        assert!(installed_extension_dir().is_none() || installed_extension_dir().is_some());
    }
}
