//! User settings, read from `~/.config/clipnest/config.ini`.
//!
//! The file is deliberately a flat list of `key = value` lines: every value is a
//! number, `#` and `;` start comments, and a line that cannot be understood is
//! reported rather than ignored. Keeping the parser this small is what makes it
//! possible to test completely - and it has to be tested, because a typo here
//! silently changes how much history ClipNest keeps.

use std::path::{Path, PathBuf};

use crate::paste::Accelerator;

/// How many unpinned entries we keep around.
pub const DEFAULT_MAX_ITEMS: i64 = 300;
/// Total size all unpinned images may occupy.
pub const DEFAULT_IMAGE_BUDGET_MB: i64 = 128;
/// Longer texts are refused; 0 stops text from being recorded at all.
pub const DEFAULT_MAX_TEXT_MB: i64 = 1;
/// Bigger images are refused; 0 stops images from being recorded at all.
pub const DEFAULT_MAX_IMAGE_MB: i64 = 8;
/// How many rows the panel loads at once.
pub const DEFAULT_PANEL_LIMIT: i64 = 300;
/// How often the shell extension looks at the clipboard.
pub const DEFAULT_POLL_INTERVAL_MS: u32 = 400;
/// Entries unused for longer than this are dropped; 0 keeps them forever.
pub const DEFAULT_MAX_AGE_DAYS: i64 = 0;
/// The shortcut typed into the focused window after an entry is picked. This is
/// the one setting whose value is not a number, and `none` turns auto-paste off
/// while leaving the copy in place.
pub const DEFAULT_PASTE_KEY: &str = "<Control>v";

pub const BYTES_PER_MB: i64 = 1024 * 1024;

/// The subset of settings the daemon hands to the shell extension over D-Bus.
///
/// The extension drops oversized payloads and starts the read before sending
/// them, so it needs these numbers too. It has to ask for them at runtime: with
/// its own compiled-in copy, raising `max_image_mb` in the config would silently
/// have no effect, because the extension would keep refusing the big images
/// before the daemon ever saw them.
pub const WIRE_KEYS: [&str; 3] = ["poll_interval_ms", "max_text_bytes", "max_image_bytes"];

/// The settings whose value is a number, in the order they are documented.
const NUMBER_KEYS: [&str; 7] = [
    "max_items",
    "image_budget_mb",
    "max_text_mb",
    "max_image_mb",
    "panel_limit",
    "poll_interval_ms",
    "max_age_days",
];
/// The settings whose value is text. Kept apart from the numbers so a value can
/// be checked against what it actually means, instead of parsed as an integer
/// and quietly rejected.
const TEXT_KEYS: [&str; 1] = ["paste_key"];

/// Every setting the file may hold. Used for the message a typo gets.
fn known_keys() -> Vec<&'static str> {
    NUMBER_KEYS
        .iter()
        .chain(TEXT_KEYS.iter())
        .copied()
        .collect()
}

/// The effective settings. Sizes are kept in bytes because that is what every
/// caller compares against; the file and the CLI speak megabytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub max_items: i64,
    pub image_budget_bytes: i64,
    pub max_text_bytes: i64,
    pub max_image_bytes: i64,
    pub panel_limit: i64,
    pub poll_interval_ms: u32,
    pub max_age_days: i64,
    /// The shortcut auto-paste types, or `None` when it is switched off.
    pub paste_key: Option<Accelerator>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_items: DEFAULT_MAX_ITEMS,
            image_budget_bytes: DEFAULT_IMAGE_BUDGET_MB * BYTES_PER_MB,
            max_text_bytes: DEFAULT_MAX_TEXT_MB * BYTES_PER_MB,
            max_image_bytes: DEFAULT_MAX_IMAGE_MB * BYTES_PER_MB,
            panel_limit: DEFAULT_PANEL_LIMIT,
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
            max_age_days: DEFAULT_MAX_AGE_DAYS,
            // Ctrl+V, the shortcut every desktop pastes with. A binding that did
            // not parse would silently disable the feature, so it would be a bug
            // in this constant - `the_default_shortcut_parses` fails for it.
            paste_key: Accelerator::parse(DEFAULT_PASTE_KEY).ok(),
        }
    }
}

/// A config file as it was found on disk.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    /// Whether the file was actually read; `false` means the defaults are in use.
    pub found: bool,
    /// Lines that could not be understood, plus read errors.
    pub notes: Vec<String>,
}

impl Config {
    /// `$XDG_CONFIG_HOME/clipnest/config.ini`, defaulting to `~/.config`.
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| {
                let mut home =
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()));
                home.push(".config");
                home
            });
        base.join("clipnest/config.ini")
    }

    pub fn load() -> Loaded {
        Self::load_from(&Self::path())
    }

    /// Reads a config file. This never fails: a missing file means the defaults,
    /// and a line that makes no sense leaves its setting at the default while
    /// saying so in `notes`.
    pub fn load_from(path: &Path) -> Loaded {
        let defaults = Config::default();
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let (config, notes) = Config::parse(&text);
                Loaded {
                    config,
                    path: path.to_path_buf(),
                    found: true,
                    notes,
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Loaded {
                config: defaults,
                path: path.to_path_buf(),
                found: false,
                notes: Vec::new(),
            },
            Err(err) => Loaded {
                config: defaults,
                path: path.to_path_buf(),
                found: false,
                notes: vec![format!("cannot read {}: {err}", path.display())],
            },
        }
    }

    /// Parses the text of a settings file. Unusable lines become notes.
    pub fn parse(text: &str) -> (Config, Vec<String>) {
        let mut config = Config::default();
        let mut notes = Vec::new();

        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let line_number = index + 1;
            let Some((key, value)) = line.split_once('=') else {
                notes.push(format!(
                    "line {line_number}: '{line}' is not a 'key = value' setting"
                ));
                continue;
            };
            // A trailing comment, as in `max_items = 300    # keep less`.
            let value = value.split('#').next().unwrap_or("").trim();
            let key = key.trim().to_ascii_lowercase();
            if !NUMBER_KEYS.contains(&key.as_str()) && !TEXT_KEYS.contains(&key.as_str()) {
                notes.push(format!(
                    "line {line_number}: unknown setting '{key}' (known: {})",
                    known_keys().join(", ")
                ));
                continue;
            }

            if key == "paste_key" {
                // Empty, `none` and `off` all mean "copy without pasting", which
                // is easier to write than a binding nobody can guess.
                let spelled = value.to_ascii_lowercase();
                config.paste_key = if spelled.is_empty()
                    || matches!(spelled.as_str(), "none" | "off" | "no")
                {
                    None
                } else {
                    match Accelerator::parse(value) {
                        Ok(accelerator) => Some(accelerator),
                        Err(err) => {
                            notes.push(format!(
                                "line {line_number}: {err}, paste_key keeps its default"
                            ));
                            continue;
                        }
                    }
                };
                continue;
            }

            let Ok(number) = value.parse::<i64>() else {
                notes.push(format!(
                    "line {line_number}: '{value}' is not a number, {key} keeps its default"
                ));
                continue;
            };

            match key.as_str() {
                "max_items" => {
                    config.max_items = bounded(&key, number, 1, 100_000, &mut notes);
                }
                "image_budget_mb" => {
                    let mb = bounded(&key, number, 0, 1_000_000, &mut notes);
                    config.image_budget_bytes = mb * BYTES_PER_MB;
                }
                "max_text_mb" => {
                    let mb = bounded(&key, number, 0, 64, &mut notes);
                    config.max_text_bytes = mb * BYTES_PER_MB;
                }
                "max_image_mb" => {
                    let mb = bounded(&key, number, 0, 1024, &mut notes);
                    config.max_image_bytes = mb * BYTES_PER_MB;
                }
                "panel_limit" => {
                    config.panel_limit = bounded(&key, number, 1, 1000, &mut notes);
                }
                "poll_interval_ms" => {
                    config.poll_interval_ms =
                        bounded(&key, number, 100, 10_000, &mut notes) as u32;
                }
                "max_age_days" => {
                    config.max_age_days = bounded(&key, number, 0, 3650, &mut notes);
                }
                other => {
                    // Unreachable: `KEYS` was checked above. Kept so adding a key
                    // to one list and forgetting the other cannot go unnoticed.
                    notes.push(format!("line {line_number}: '{other}' is not handled"));
                }
            }
        }

        (config, notes)
    }

    /// The values `clipnest config` prints, in the order they are documented.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        vec![
            ("max_items", self.max_items.to_string()),
            (
                "image_budget_mb",
                (self.image_budget_bytes / BYTES_PER_MB).to_string(),
            ),
            (
                "max_text_mb",
                (self.max_text_bytes / BYTES_PER_MB).to_string(),
            ),
            (
                "max_image_mb",
                (self.max_image_bytes / BYTES_PER_MB).to_string(),
            ),
            ("panel_limit", self.panel_limit.to_string()),
            ("poll_interval_ms", self.poll_interval_ms.to_string()),
            ("max_age_days", self.max_age_days.to_string()),
            (
                "paste_key",
                self.paste_key
                    .as_ref()
                    .map(Accelerator::describe)
                    .unwrap_or_else(|| "off".to_string()),
            ),
        ]
    }

    /// The values the shell extension asks for over D-Bus, with `WIRE_KEYS` as
    /// the list of names so the two can never drift apart.
    pub fn wire(&self) -> Vec<(&'static str, String)> {
        WIRE_KEYS
            .iter()
            .map(|key| (*key, self.wire_value(key).to_string()))
            .collect()
    }

    /// One setting as the extension needs it: sizes in bytes, the interval in
    /// milliseconds.
    pub fn wire_value(&self, key: &str) -> i64 {
        match key {
            "poll_interval_ms" => self.poll_interval_ms as i64,
            "max_text_bytes" => self.max_text_bytes,
            "max_image_bytes" => self.max_image_bytes,
            _ => 0,
        }
    }

    /// Whether text is recorded at all.
    pub fn takes_text(&self) -> bool {
        self.max_text_bytes > 0
    }

    /// Whether images are recorded at all.
    pub fn takes_images(&self) -> bool {
        self.max_image_bytes > 0
    }

    /// The file `clipnest config` suggests when there is nothing on disk yet.
    pub fn example() -> String {
        let config = Config::default();
        format!(
            "# ClipNest settings. Every line is 'key = value'; '#' starts a comment.
# Delete a line to go back to the default. Values are megabytes where the name
# says so, and 0 turns that kind of capture off completely.

# How many unpinned entries to keep. Pinned ones are never trimmed.
max_items = {max_items}
# Total size all unpinned images may occupy. Oldest images go first.
image_budget_mb = {image_budget}
# Largest single text / image accepted. A huge copy would bloat the database.
max_text_mb = {text}
max_image_mb = {image}
# How many rows the panel loads at once.
panel_limit = {panel}
# How often the shell extension looks at the clipboard, in milliseconds.
poll_interval_ms = {poll}
# Drop entries nothing has used for this many days; 0 keeps them forever.
max_age_days = {age}
# What auto-paste types into the window you came from, after picking an entry.
# Write `none` to only copy, and leave the pasting to you.
paste_key = {paste}
",
            max_items = config.max_items,
            image_budget = config.image_budget_bytes / BYTES_PER_MB,
            text = config.max_text_bytes / BYTES_PER_MB,
            image = config.max_image_bytes / BYTES_PER_MB,
            panel = config.panel_limit,
            poll = config.poll_interval_ms,
            age = config.max_age_days,
            paste = config
                .paste_key
                .as_ref()
                .map(Accelerator::describe)
                .unwrap_or_else(|| DEFAULT_PASTE_KEY.to_string()),
        )
    }
}

/// A value outside its sensible range is clamped, and the file is told about it.
fn bounded(key: &str, value: i64, low: i64, high: i64, notes: &mut Vec<String>) -> i64 {
    if value < low || value > high {
        notes.push(format!(
            "{key} = {value} is outside {low}..={high}; using {}",
            value.clamp(low, high)
        ));
        return value.clamp(low, high);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_changes_nothing() {
        let (config, notes) = Config::parse("");
        assert_eq!(config, Config::default());
        assert!(notes.is_empty());
    }

    #[test]
    fn reads_every_setting() {
        let (config, notes) = Config::parse(
            "
            # a comment
            ; another comment
            max_items = 12
            image_budget_mb=64
            max_text_mb = 2
            max_image_mb = 32
            panel_limit = 50
            poll_interval_ms = 250
            max_age_days = 7
            paste_key = <Super>v
            ",
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(config.max_items, 12);
        assert_eq!(config.image_budget_bytes, 64 * BYTES_PER_MB);
        assert_eq!(config.max_text_bytes, 2 * BYTES_PER_MB);
        assert_eq!(config.max_image_bytes, 32 * BYTES_PER_MB);
        assert_eq!(config.panel_limit, 50);
        assert_eq!(config.poll_interval_ms, 250);
        assert_eq!(config.max_age_days, 7);
        assert_eq!(
            config.paste_key.as_ref().map(Accelerator::describe),
            Some("<super>v".to_string())
        );
    }

    #[test]
    fn the_default_shortcut_parses() {
        // Auto-paste is on unless the file says otherwise, and a constant that
        // did not parse would switch it off without a word to anybody.
        let key = Config::default().paste_key;
        assert_eq!(key.as_ref().map(Accelerator::describe), Some("<ctrl>v".to_string()));
    }

    #[test]
    fn pasting_can_be_switched_off_in_words() {
        for spelling in ["paste_key = none", "paste_key = off", "paste_key = NO", "paste_key ="] {
            let (config, notes) = Config::parse(&format!("{spelling}\n"));
            assert!(notes.is_empty(), "{spelling}: {notes:?}");
            assert!(config.paste_key.is_none(), "{spelling} should disable pasting");
        }
    }

    #[test]
    fn a_shortcut_that_cannot_be_typed_is_reported() {
        // The value is checked with the same grammar the keybinding command uses,
        // so a typo is named instead of leaving auto-paste quietly wrong.
        let (config, notes) = Config::parse("paste_key = Control+v\npaste_key = <Bogus>v\n");
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("missing its brackets"), "{notes:?}");
        assert!(notes[0].contains("keeps its default"));
        assert_eq!(config.paste_key, Config::default().paste_key);
    }

    #[test]
    fn tolerates_spacing_case_and_trailing_comments() {
        let (config, notes) = Config::parse("  MAX_ITEMS\t=\t9   # keep less\n\n");
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(config.max_items, 9);
    }

    #[test]
    fn reports_what_it_cannot_understand() {
        let (config, notes) = Config::parse(
            "max_items = 5
             minimum = 3
             max_text_mb = lots
             poll_interval_ms = 20
             this line has no equals sign",
        );
        // The good line still applies.
        assert_eq!(config.max_items, 5);
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(notes[0].contains("unknown setting 'minimum'") && notes[0].contains("max_items"));
        assert!(notes[1].contains("'lots' is not a number") && notes[1].contains("keeps its default"));
        // Out of range is clamped rather than dropped.
        assert!(notes[2].contains("outside 100..=10000"), "{:?}", notes[2]);
        assert_eq!(config.poll_interval_ms, 100);
        assert!(notes[3].contains("is not a 'key = value' setting"));
        // The unreadable setting kept its default.
        assert_eq!(config.max_text_bytes, Config::default().max_text_bytes);
    }

    #[test]
    fn zero_turns_capture_off() {
        let (config, notes) = Config::parse("max_image_mb = 0\nmax_text_mb = 0\nimage_budget_mb = 0\n");
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!config.takes_images());
        assert!(!config.takes_text());
        assert_eq!(config.image_budget_bytes, 0);
    }

    #[test]
    fn the_suggested_file_means_the_defaults() {
        // `clipnest config` prints this when there is no file yet, so it has to
        // be a valid file that changes nothing.
        let (config, notes) = Config::parse(&Config::example());
        assert_eq!(config, Config::default());
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn prints_the_same_numbers_it_handed_out() {
        let config = Config {
            max_items: 7,
            image_budget_bytes: 9 * BYTES_PER_MB,
            max_text_bytes: 2 * BYTES_PER_MB,
            max_image_bytes: 3 * BYTES_PER_MB,
            panel_limit: 11,
            poll_interval_ms: 900,
            max_age_days: 4,
            paste_key: None,
        };
        let rows = config.rows();
        // Every setting is listed, and the ones the file spells out are all here.
        assert_eq!(rows.len(), NUMBER_KEYS.len() + TEXT_KEYS.len());
        for key in known_keys() {
            assert!(rows.iter().any(|(name, _)| *name == key), "{key} is missing");
        }
        assert_eq!(rows[0], ("max_items", "7".to_string()));
        assert_eq!(rows[1], ("image_budget_mb", "9".to_string()));
        assert_eq!(rows[6], ("max_age_days", "4".to_string()));
        // A shortcut that is switched off is printed as such, not as an empty
        // space that looks like a missing value.
        assert_eq!(
            rows.iter().find(|(key, _)| *key == "paste_key"),
            Some(&("paste_key", "off".to_string()))
        );

        // The extension gets bytes, under the names it asks for.
        let wire = config.wire();
        assert_eq!(
            wire.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            WIRE_KEYS.to_vec()
        );
        assert_eq!(wire[1].1, (2 * BYTES_PER_MB).to_string());
        assert_eq!(config.wire_value("poll_interval_ms"), 900);
        assert_eq!(config.wire_value("nonsense"), 0);
    }

    #[test]
    fn a_missing_file_is_the_defaults() {
        let path = std::env::temp_dir().join(format!(
            "clipnest-config-absent-{}-{}.ini",
            std::process::id(),
            crate::db::now_ms()
        ));
        let _ = std::fs::remove_file(&path);
        let loaded = Config::load_from(&path);
        assert!(!loaded.found);
        assert!(loaded.notes.is_empty());
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.path, path);
    }

    #[test]
    fn a_file_on_disk_is_read() {
        let path = std::env::temp_dir().join(format!(
            "clipnest-config-{}-{}.ini",
            std::process::id(),
            crate::db::now_ms()
        ));
        std::fs::write(&path, "max_items = 42\noops\n").unwrap();
        let loaded = Config::load_from(&path);
        assert!(loaded.found);
        assert_eq!(loaded.config.max_items, 42);
        assert_eq!(loaded.notes.len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
