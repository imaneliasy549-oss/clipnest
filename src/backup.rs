//! Backing the history up into a directory, and reading it back.
//!
//! The format is deliberately the file system itself: one file per entry, plus a
//! tab-separated `index.tsv` that says which file is which entry.
//!
//! ```text
//! ~/clipnest-backup/index.tsv      id, kind, file, pinned, timestamps, size, mime
//! ~/clipnest-backup/000012.txt     the text of entry 12, byte for byte
//! ~/clipnest-backup/000007.png     the encoded image of entry 7, byte for byte
//! ```
//!
//! Images keep the exact bytes the clipboard offered, so a backup round trip is
//! lossless; nothing is re-encoded. Keeping the payloads in separate files means
//! neither the export nor the import has to hold the whole history in memory,
//! and a backup can be read, grepped or repaired with ordinary tools.

use std::path::{Path, PathBuf};

use crate::db::{now_ms, Content, Kind, Restored, Store};

/// The first line of `index.tsv`: a marker and the format version.
const HEADER: &str = "#clipnest-export\t1";
const INDEX: &str = "index.tsv";
/// Column order of `index.tsv`. The header line spells the names out.
const COLUMNS: [&str; 9] = [
    "id",
    "kind",
    "file",
    "pinned",
    "created_at",
    "last_used_at",
    "width",
    "height",
    "mime",
];

/// What an export or an import did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub text: usize,
    pub images: usize,
    pub pinned: usize,
    pub bytes: i64,
    /// Entries that were already in the history (an import is a merge).
    pub duplicates: usize,
    /// Entries that could not be read, decoded or stored.
    pub skipped: usize,
    pub notes: Vec<String>,
}

impl Summary {
    pub fn entries(&self) -> usize {
        self.text + self.images
    }

    fn note(&mut self, message: String) {
        // A damaged backup can produce thousands of identical complaints; say it
        // once and keep counting.
        if !self.notes.contains(&message) {
            self.notes.push(message);
        }
    }
}

/// One row of `index.tsv`.
#[derive(Debug)]
struct Entry {
    /// The row id the entry had when it was exported; 0 when the index did not
    /// say. Restoring it keeps entries copied in the same millisecond in order.
    id: i64,
    file: String,
    kind: Kind,
    pinned: bool,
    created_at: i64,
    last_used_at: i64,
    mime: String,
}

/// Writes the history into `dir`. `limit` is how many entries to take, newest
/// first; a negative limit takes all of them.
pub fn export(store: &Store, dir: &Path, limit: i64, force: bool) -> Result<Summary, String> {
    if dir.is_dir() && !force && !is_empty(dir)? {
        return Err(format!(
            "{} is not empty; pass --force to write into it anyway",
            dir.display()
        ));
    }
    std::fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;

    let items = store
        .list("", limit)
        .map_err(|err| format!("cannot read the history: {err}"))?;

    let mut summary = Summary::default();
    let mut rows = String::new();
    for item in &items {
        let Some(content) = store.content(item.id).ok().flatten() else {
            summary.skipped += 1;
            summary.note(format!("entry {} could not be read", item.id));
            continue;
        };

        let (kind, payload, extension) = match &content {
            Content::Text(text) => ("text", text.as_bytes(), "txt"),
            Content::Image { data, mime, .. } => ("image", data.as_slice(), extension_for(mime)),
        };
        let file = format!("{:06}.{extension}", item.id);
        if let Err(err) = std::fs::write(dir.join(&file), payload) {
            summary.skipped += 1;
            summary.note(format!("cannot write {file}: {err}"));
            continue;
        }

        summary.bytes += payload.len() as i64;
        match content {
            Content::Text(_) => summary.text += 1,
            Content::Image { .. } => summary.images += 1,
        }
        if item.pinned {
            summary.pinned += 1;
        }
        rows.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            item.id,
            kind,
            file,
            i32::from(item.pinned),
            item.created_at,
            item.last_used_at,
            item.width,
            item.height,
            item.mime.as_deref().unwrap_or(""),
        ));
    }

    let index = format!("{HEADER}\n{}\n{rows}", COLUMNS.join("\t"));
    std::fs::write(dir.join(INDEX), index)
        .map_err(|err| format!("cannot write {}/{INDEX}: {err}", dir.display()))?;
    Ok(summary)
}

/// Reads a directory written by `export` back into the history.
///
/// Entries that are already there are left alone: importing the same backup
/// twice must not duplicate rows or move timestamps around.
pub fn import(store: &Store, dir: &Path) -> Result<Summary, String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let config = store.config().clone();
    let mut summary = Summary::default();

    let entries = match std::fs::read_to_string(dir.join(INDEX)) {
        Ok(text) => parse_index(&text, &mut summary),
        Err(_) => {
            summary.note(format!(
                "no {INDEX} in {}; reading every file in the directory instead",
                dir.display()
            ));
            scan_directory(dir)?
        }
    };

    for entry in entries {
        let Ok(data) = std::fs::read(dir.join(&entry.file)) else {
            summary.skipped += 1;
            summary.note(format!("{} is missing", entry.file));
            continue;
        };
        let content = match entry.kind {
            Kind::Text => {
                let text = match String::from_utf8(data) {
                    Ok(text) => text,
                    Err(_) => {
                        summary.skipped += 1;
                        summary.note(format!("{} is not valid UTF-8", entry.file));
                        continue;
                    }
                };
                if text.is_empty() {
                    summary.skipped += 1;
                    summary.note(format!("{} is empty", entry.file));
                    continue;
                }
                if !config.takes_text() || text.len() as i64 > config.max_text_bytes {
                    summary.skipped += 1;
                    summary.note(format!(
                        "{} is larger than max_text_mb ({})",
                        entry.file,
                        config.max_text_bytes / crate::config::BYTES_PER_MB
                    ));
                    continue;
                }
                Content::Text(text)
            }
            Kind::Image => {
                if !config.takes_images() || data.len() as i64 > config.max_image_bytes {
                    summary.skipped += 1;
                    summary.note(format!(
                        "{} is larger than max_image_mb ({})",
                        entry.file,
                        config.max_image_bytes / crate::config::BYTES_PER_MB
                    ));
                    continue;
                }
                // Decoding is also the validation: an entry that cannot be
                // decoded could never be pasted, so it is not worth keeping.
                let Some((width, height)) = crate::app::image_size(&data) else {
                    summary.skipped += 1;
                    summary.note(format!("{} is not an image this build can decode", entry.file));
                    continue;
                };
                let mime = if entry.mime.is_empty() {
                    mime_for_name(&entry.file).to_string()
                } else {
                    entry.mime
                };
                Content::Image {
                    mime,
                    data,
                    width,
                    height,
                }
            }
        };
        let now = now_ms();
        let created_at = if entry.created_at > 0 { entry.created_at } else { now };
        let last_used_at = if entry.last_used_at > 0 {
            entry.last_used_at
        } else {
            created_at
        };
        match store.restore(
            &content,
            entry.pinned,
            created_at,
            last_used_at,
            Some(entry.id),
        ) {
            Ok(Restored::Added) => {
                summary.bytes += match &content {
                    Content::Text(text) => text.len() as i64,
                    Content::Image { data, .. } => data.len() as i64,
                };
                if entry.pinned {
                    summary.pinned += 1;
                }
                match content {
                    Content::Text(_) => summary.text += 1,
                    Content::Image { .. } => summary.images += 1,
                }
            }
            Ok(Restored::Present) => summary.duplicates += 1,
            Err(err) => {
                summary.skipped += 1;
                summary.note(format!("cannot store {}: {err}", entry.file));
            }
        }
    }

    // Trimming once at the end rather than per row: the limits would otherwise be
    // applied over and over to a history that is still being rebuilt.
    if let Err(err) = store.trim_now() {
        summary.note(format!("cannot trim the restored history: {err}"));
    }
    Ok(summary)
}

/// Reads what an `index.tsv` says. It is a plain text file, so it is also the
/// most likely part of a backup to be damaged; every unusable row is reported
/// and the rest still imports.
fn parse_index(text: &str, summary: &mut Summary) -> Vec<Entry> {
    let mut entries = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            if !line.starts_with("#clipnest-export\t1") {
                summary.note(format!(
                    "line {}: unknown format version, trying anyway",
                    number + 1
                ));
            }
            continue;
        }
        if line.starts_with("id\t") {
            // The column names, for whoever opens the file in an editor.
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 3 {
            summary.skipped += 1;
            summary.note(format!("line {}: not enough columns", number + 1));
            continue;
        }
        let number_at = |index: usize| -> Option<i64> {
            fields
                .get(index)
                .and_then(|value| value.trim().parse::<i64>().ok())
        };
        entries.push(Entry {
            id: number_at(0).unwrap_or(0),
            kind: if fields[1].trim() == "image" {
                Kind::Image
            } else {
                Kind::Text
            },
            file: fields[2].trim().to_string(),
            pinned: number_at(3).unwrap_or(0) != 0,
            created_at: number_at(4).unwrap_or(0),
            last_used_at: number_at(5).unwrap_or(0),
            mime: fields.get(8).map(|value| value.trim().to_string()).unwrap_or_default(),
        });
    }
    entries
}

/// A backup without an index: every file in the directory, by name, so the
/// numbering an export used keeps the order. Text and image are told apart by
/// their content, not by their name.
fn scan_directory(dir: &Path) -> Result<Vec<Entry>, String> {
    let read = std::fs::read_dir(dir).map_err(|err| format!("cannot read {}: {err}", dir.display()))?;
    let mut files: Vec<PathBuf> = read
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    Ok(files
        .into_iter()
        .filter_map(|path| {
            let file = path.file_name()?.to_string_lossy().to_string();
            if file == INDEX {
                return None;
            }
            Some(Entry {
                id: 0,
                kind: if mime_for_name(&file).is_empty() {
                    Kind::Text
                } else {
                    Kind::Image
                },
                file,
                pinned: false,
                created_at: 0,
                last_used_at: 0,
                mime: String::new(),
            })
        })
        .collect())
}

fn is_empty(dir: &Path) -> Result<bool, String> {
    let mut entries =
        std::fs::read_dir(dir).map_err(|err| format!("cannot read {}: {err}", dir.display()))?;
    Ok(entries.next().is_none())
}

/// The file extension an image entry is stored under.
pub fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/tiff" => "tiff",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/gif" => "gif",
        _ => "png",
    }
}

/// The mime type of a file name, or `""` when the extension is not an image we
/// know. The payload is what decides in the end; this only tells the import
/// whether to treat a file as text or as an image.
pub fn mime_for_name(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "tiff" | "tif" => "image/tiff",
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use rusqlite::Connection;

    /// The smallest PNG there is: 1x1, transparent. Used to prove that decoding
    /// works in a process that never initialized GTK, which is what `clipnest
    /// import` is.
    const ONE_PIXEL_PNG: [u8; 67] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn store() -> Store {
        Store::from_conn(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clipnest-backup-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn an_image_can_be_measured_without_a_display() {
        // `import` runs outside any GTK application, so the decode used for
        // validation has to work there too.
        assert_eq!(
            crate::app::image_size(&ONE_PIXEL_PNG),
            Some((1, 1)),
            "decoding an image needs no display"
        );
        assert_eq!(crate::app::image_size(b"not an image at all"), None);
    }

    #[test]
    fn exports_and_imports_the_history_losslessly() {
        let now = now_ms();
        let source = store();
        source.push(&Content::Text("hello backup".into())).unwrap();
        source
            .push(&Content::Image {
                mime: "image/png".into(),
                data: ONE_PIXEL_PNG.to_vec(),
                width: 1,
                height: 1,
            })
            .unwrap();
        source.push(&Content::Text("pinned text".into())).unwrap();
        let pinned_id = source.list("", -1).unwrap()[0].id;
        source.set_pinned(pinned_id, true).unwrap();

        let dir = temp_dir("roundtrip");
        let exported = export(&source, &dir, -1, false).unwrap();
        assert_eq!(exported.entries(), 3);
        assert_eq!(exported.text, 2);
        assert_eq!(exported.images, 1);
        assert_eq!(exported.pinned, 1);
        assert!(exported.bytes > 0);

        // The payload files are the entries themselves.
        let index = std::fs::read_to_string(dir.join(INDEX)).unwrap();
        assert!(index.starts_with(HEADER));
        assert!(index.contains("id\tkind\tfile"));
        let png = std::fs::read(dir.join(format!("{pinned_id:06}.txt"))).unwrap();
        assert_eq!(png, b"pinned text");

        let target = store();
        let imported = import(&target, &dir).unwrap();
        assert_eq!(imported.entries(), 3);
        assert_eq!(imported.duplicates, 0);
        assert_eq!(imported.skipped, 0);
        assert!(imported.notes.is_empty(), "{:?}", imported.notes);

        // Same entries, same order, same pin, same timestamps, same bytes.
        let before = source.list("", -1).unwrap();
        let after = target.list("", -1).unwrap();
        assert_eq!(before.len(), after.len());
        for (left, right) in before.iter().zip(after.iter()) {
            assert_eq!(left.preview(), right.preview());
            assert_eq!(left.pinned, right.pinned);
            assert_eq!(left.created_at, right.created_at);
            assert_eq!(left.last_used_at, right.last_used_at);
        }
        let image = after.iter().find(|item| item.is_image()).unwrap();
        match target.content(image.id).unwrap() {
            Some(Content::Image { data, mime, .. }) => {
                assert_eq!(data, ONE_PIXEL_PNG.to_vec());
                assert_eq!(mime, "image/png");
            }
            other => panic!("unexpected content: {other:?}"),
        }
        // Timestamps came from the backup, not from the import.
        assert!(after
            .iter()
            .all(|item| (item.last_used_at - now).abs() < 60_000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn importing_twice_merges_instead_of_duplicating() {
        let source = store();
        source.push(&Content::Text("first".into())).unwrap();
        source.push(&Content::Text("second".into())).unwrap();
        let dir = temp_dir("twice");
        export(&source, &dir, -1, false).unwrap();

        let target = store();
        let first = import(&target, &dir).unwrap();
        assert_eq!(first.entries(), 2);
        let times: Vec<i64> = target.list("", -1).unwrap().iter().map(|i| i.last_used_at).collect();

        let second = import(&target, &dir).unwrap();
        assert_eq!(second.entries(), 0);
        assert_eq!(second.duplicates, 2);
        assert_eq!(target.stats().unwrap().0, 2);
        let after: Vec<i64> = target.list("", -1).unwrap().iter().map(|i| i.last_used_at).collect();
        assert_eq!(times, after, "a second import must not move rows around");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_backup_imports_what_it_can() {
        let dir = temp_dir("damaged");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("000001.txt"), "fine").unwrap();
        std::fs::write(
            dir.join(INDEX),
            "#clipnest-export\t99\nid\tkind\tfile\tpinned\tcreated_at\tlast_used_at\twidth\theight\tmime\n\
             1\ttext\t000001.txt\t0\t0\t0\t0\t0\t\n\
             2\timage\t000002.png\t0\t0\t0\t0\t0\timage/png\n\
             3\ttext\t000003.txt\t0\t0\t0\t0\t0\t\n\
             4\tbroken\n\
             5\ttext\t000005.txt\t0\t0\t0\t0\t0\t\n",
        )
        .unwrap();
        std::fs::write(dir.join("000005.txt"), [0xff, 0xfe, 0x00]).unwrap();

        let target = store();
        let summary = import(&target, &dir).unwrap();
        assert_eq!(summary.text, 1);
        assert_eq!(target.stats().unwrap().0, 1);
        // Both files the index names but that are not there, the invalid UTF-8
        // file and the short row are all accounted for, and the unknown format
        // version was mentioned.
        assert_eq!(summary.skipped, 4);
        assert!(
            summary.notes.iter().any(|note| note.contains("unknown format version")),
            "{:?}",
            summary.notes
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_without_an_index_is_read_by_name() {
        let dir = temp_dir("loose");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "just some text").unwrap();
        std::fs::write(dir.join("shot.png"), ONE_PIXEL_PNG).unwrap();

        let target = store();
        let summary = import(&target, &dir).unwrap();
        assert_eq!(summary.text, 1);
        assert_eq!(summary.images, 1);
        let items = target.list("", -1).unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|item| item.is_image() && item.width == 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_refuses_to_scatter_files_into_a_used_directory() {
        let dir = temp_dir("notempty");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("something-else.txt"), "do not touch").unwrap();

        let source = store();
        source.push(&Content::Text("x".into())).unwrap();
        let refusal = export(&source, &dir, -1, false).unwrap_err();
        assert!(refusal.contains("not empty") && refusal.contains("--force"));
        // With --force it writes, and leaves the other files alone.
        assert_eq!(export(&source, &dir, -1, true).unwrap().entries(), 1);
        assert!(dir.join("something-else.txt").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_respects_the_configured_limits() {
        let source = store();
        source.push(&Content::Text("long enough".into())).unwrap();
        let dir = temp_dir("limits");
        export(&source, &dir, -1, false).unwrap();

        // A history that refuses text takes nothing from the backup.
        let target = Store::from_conn_with(
            Connection::open_in_memory().unwrap(),
            Config {
                max_text_bytes: 0,
                ..Config::default()
            },
        )
        .unwrap();
        let summary = import(&target, &dir).unwrap();
        assert_eq!(summary.text, 0);
        assert_eq!(summary.skipped, 1);
        assert_eq!(target.stats().unwrap().0, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn knows_the_image_types_it_can_write() {
        for (mime, extension) in [
            ("image/png", "png"),
            ("image/jpeg", "jpg"),
            ("image/webp", "webp"),
            ("image/tiff", "tiff"),
            ("image/bmp", "bmp"),
            ("image/gif", "gif"),
        ] {
            assert_eq!(extension_for(mime), extension);
            assert_eq!(mime_for_name(&format!("x.{extension}")), mime);
        }
        // Unknown types still get a usable extension.
        assert_eq!(extension_for("image/avif"), "png");
        assert_eq!(mime_for_name("notes.txt"), "");
        assert_eq!(mime_for_name("no-extension"), "");
    }
}
