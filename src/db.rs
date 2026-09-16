use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::config::Config;

/// Bumped whenever the table layout changes; see `migrate`.
pub const SCHEMA_VERSION: i64 = 2;
/// How much text is loaded for a cheap list preview.
const SNIPPET_CHARS: i64 = 400;

const CREATE_ITEMS: &str = "
    CREATE TABLE IF NOT EXISTS items (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        kind TEXT NOT NULL,
        text TEXT,
        data BLOB,
        mime TEXT,
        width INTEGER NOT NULL DEFAULT 0,
        height INTEGER NOT NULL DEFAULT 0,
        hash TEXT NOT NULL UNIQUE,
        pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
        created_at INTEGER NOT NULL,
        last_used_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_items_order ON items(pinned DESC, last_used_at DESC);
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Image,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Image => "image",
        }
    }

    fn from_db(value: &str) -> Self {
        if value == "image" {
            Kind::Image
        } else {
            Kind::Text
        }
    }
}

/// What happened to one row handed to `Store::restore`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restored {
    /// The entry was not in the history and is now.
    Added,
    /// The entry was already there; only its pin could change.
    Present,
}

/// What actually sits on the clipboard: text, or an encoded image.
#[derive(Clone, Debug)]
pub enum Content {
    Text(String),
    Image {
        mime: String,
        data: Vec<u8>,
        width: u32,
        height: u32,
    },
}

/// Row metadata. Cheap enough to load for a whole list: text entries carry only
/// a short snippet, image bytes are fetched per row on demand.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: i64,
    pub kind: Kind,
    /// First `SNIPPET_CHARS` characters of a text entry.
    pub snippet: String,
    /// Character count of the whole text entry.
    pub text_len: i64,
    pub mime: Option<String>,
    pub width: u32,
    pub height: u32,
    pub pinned: bool,
    pub created_at: i64,
    pub last_used_at: i64,
}

impl Item {
    pub fn is_image(&self) -> bool {
        self.kind == Kind::Image
    }

    /// Single-line preview used in the panel and the `list` command.
    pub fn preview(&self) -> String {
        match self.kind {
            Kind::Image => format!("Image {}x{}", self.width, self.height),
            Kind::Text => {
                let line = self.snippet.lines().next().unwrap_or("").trim();
                if line.is_empty() {
                    format!("(whitespace, {} chars)", self.text_len)
                } else {
                    line.chars().take(120).collect()
                }
            }
        }
    }
}

pub struct Store {
    conn: Connection,
    /// The limits in force. Every trimming decision reads them, so a settings
    /// file can change the history size without recompiling anything.
    config: Config,
    /// Where the database lives, so disk usage can include the `-wal` and `-shm`
    /// sidecars. `None` for an in-memory database, which the tests use.
    path: Option<PathBuf>,
}

impl Store {
    pub fn path() -> PathBuf {
        data_dir().join("history.db")
    }

    /// Opens the history with the settings the user configured.
    pub fn open_default() -> rusqlite::Result<Self> {
        Self::open_with(Config::load().config)
    }

    /// Opens the history with an explicit set of limits.
    pub fn open_with(config: Config) -> rusqlite::Result<Self> {
        Self::open_at(&Self::path(), config)
    }

    /// Opens (creating it if needed) the history at an explicit path.
    pub fn open_at(path: &Path, config: Config) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut store = Self::from_conn(Connection::open(path)?)?;
        store.config = config;
        store.path = Some(path.to_path_buf());
        Ok(store)
    }

    /// The settings in force.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Read-only handle used by the CLI when the daemon is not running.
    pub fn open_reader() -> rusqlite::Result<Self> {
        Self::open_read_only_at(&Self::path(), Config::load().config)
    }

    /// Opening a write-ahead-log database with `SQLITE_OPEN_READ_ONLY` fails
    /// whenever SQLite would have to create the `-shm` sidecar first, and a
    /// daemon that was stopped (or killed) leaves the database in exactly that
    /// state. Such a file is opened normally instead and put in `query_only`
    /// mode, where writes still fail but reads no longer depend on the sidecar.
    /// A file that is not there keeps the original read-only error, which is how
    /// the caller learns it may create a fresh database.
    fn open_read_only_at(path: &std::path::Path, config: Config) -> rusqlite::Result<Self> {
        // A read-only handle never trims, so the limits only matter if a caller
        // asks; keep whatever it was given either way.
        match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => {
                conn.execute_batch("PRAGMA busy_timeout=5000;")?;
                Ok(Self {
                    conn,
                    config,
                    path: Some(path.to_path_buf()),
                })
            }
            Err(err) if !path.exists() => Err(err),
            Err(_) => {
                let conn = Connection::open(path)?;
                conn.execute_batch("PRAGMA query_only=ON; PRAGMA busy_timeout=5000;")?;
                Ok(Self {
                    conn,
                    config,
                    path: Some(path.to_path_buf()),
                })
            }
        }
    }

    /// Wraps an existing connection (an in-memory one, in the tests).
    pub(crate) fn from_conn(conn: Connection) -> rusqlite::Result<Self> {
        Self::from_conn_with(conn, Config::default())
    }

    pub(crate) fn from_conn_with(conn: Connection, config: Config) -> rusqlite::Result<Self> {
        conn.execute_batch(
            // `busy_timeout` is what keeps a read running while the daemon is
            // writing: without it the two collide instantly and SQLite answers
            // SQLITE_BUSY instead of waiting.
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA auto_vacuum=INCREMENTAL;
             PRAGMA busy_timeout=5000;",
        )?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            config,
            path: None,
        })
    }

    /// Insert a new entry, or bump an identical one back to the top.
    pub fn push(&self, content: &Content) -> rusqlite::Result<()> {
        let now = now_ms();
        let hash = hash_of(content);
        let (kind, text, data, mime, width, height) = columns_of(content);
        self.conn.execute(
            "INSERT INTO items (kind, text, data, mime, width, height, hash, created_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT(hash) DO UPDATE SET last_used_at = excluded.last_used_at",
            params![kind, text, data, mime, width, height, hash, now],
        )?;
        // Trimming is the only thing that ever removes rows, so it is also the
        // only place that has to hand the freed pages back to the file system.
        if self.trim()? > 0 {
            self.reclaim();
        }
        Ok(())
    }

    /// Inserts an entry that came out of a backup, with the timestamps it had.
    ///
    /// Without this a restored history would look as if every entry had just
    /// been copied, and the order would be whatever the backup listed. An entry
    /// that is already here keeps its own timestamps - importing the same backup
    /// twice must not reshuffle the history - and only gains pinning.
    ///
    /// `keep_id` is the row id the entry had when it was exported. Two copies
    /// inside the same millisecond are ordered by id, so without it a restored
    /// history could put them in the other order. The id is only kept when that
    /// id is free: importing into a history that already uses it would otherwise
    /// have to move rows out of the way.
    pub fn restore(
        &self,
        content: &Content,
        pinned: bool,
        created_at: i64,
        last_used_at: i64,
        keep_id: Option<i64>,
    ) -> rusqlite::Result<Restored> {
        let hash = hash_of(content);
        let existed = self
            .conn
            .query_row("SELECT 1 FROM items WHERE hash = ?1", params![hash], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if existed {
            // Never unpins: the live row may have been pinned on purpose.
            self.conn.execute(
                "UPDATE items SET pinned = ?1 WHERE hash = ?2 AND ?1 = 1",
                params![pinned as i64, hash],
            )?;
            return Ok(Restored::Present);
        }

        let (kind, text, data, mime, width, height) = columns_of(content);
        self.conn.execute(
            "INSERT INTO items (kind, text, data, mime, width, height, hash, pinned, created_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                kind,
                text,
                data,
                mime,
                width,
                height,
                hash,
                pinned as i64,
                created_at,
                last_used_at
            ],
        )?;

        if let Some(id) = keep_id.filter(|id| *id > 0) {
            let inserted = self.conn.last_insert_rowid();
            if id != inserted && !self.has_id(id)? {
                self.conn.execute(
                    "UPDATE items SET id = ?1 WHERE id = ?2",
                    params![id, inserted],
                )?;
            }
        }
        Ok(Restored::Added)
    }

    fn has_id(&self, id: i64) -> rusqlite::Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM items WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()?
            .is_some())
    }

    /// Applied once after a whole import, so the restored history is trimmed to
    /// the configured limits exactly like a live one.
    pub fn trim_now(&self) -> rusqlite::Result<usize> {
        self.trim()
    }

    /// Newest first, pinned entries on top. An empty needle lists everything, and
    /// a negative limit lists the whole history (which is what `export` wants).
    ///
    /// No `COLLATE` here on purpose: `LIKE` is already case-insensitive for ASCII
    /// unless `case_sensitive_like` is on, and putting a collation after `ESCAPE`
    /// would apply to the boolean result rather than to the comparison.
    pub fn list(&self, needle: &str, limit: i64) -> rusqlite::Result<Vec<Item>> {
        let needle = escape_like(needle.trim());
        self.query_items(
            "SELECT id, kind, substr(coalesce(text, ''), 1, ?3), length(coalesce(text, '')),
                    mime, width, height, pinned, created_at, last_used_at
             FROM items
             WHERE ?1 = '' OR (kind = 'text' AND text LIKE '%' || ?1 || '%' ESCAPE '\\')
             ORDER BY pinned DESC, last_used_at DESC, id DESC
             LIMIT ?2",
            params![needle, limit, SNIPPET_CHARS],
        )
    }

    /// The full payload of one entry, for copying it back to the clipboard.
    pub fn content(&self, id: i64) -> rusqlite::Result<Option<Content>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, text, mime, data, width, height FROM items WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let kind: String = row.get(0)?;
        if kind == "image" {
            let mime: Option<String> = row.get(2)?;
            let Some(data) = row.get::<_, Option<Vec<u8>>>(3)? else {
                return Ok(None);
            };
            Ok(Some(Content::Image {
                mime: mime.unwrap_or_else(|| "image/png".to_string()),
                data,
                width: row.get::<_, i64>(4)? as u32,
                height: row.get::<_, i64>(5)? as u32,
            }))
        } else {
            Ok(row.get::<_, Option<String>>(1)?.map(Content::Text))
        }
    }

    /// Mark an entry as just used so it moves back to the top of the history.
    pub fn touch(&self, id: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE items SET last_used_at = ?1 WHERE id = ?2",
            params![now_ms(), id],
        )?;
        Ok(())
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE items SET pinned = ?1 WHERE id = ?2",
            params![pinned as i64, id],
        )?;
        Ok(())
    }

    /// Flips the pin and reports the new state.
    pub fn toggle_pinned(&self, id: i64) -> rusqlite::Result<bool> {
        self.conn.execute(
            "UPDATE items SET pinned = CASE pinned WHEN 1 THEN 0 ELSE 1 END WHERE id = ?1",
            params![id],
        )?;
        let pinned: Option<i64> = self
            .conn
            .query_row("SELECT pinned FROM items WHERE id = ?1", params![id], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(pinned.unwrap_or(0) != 0)
    }

    /// Removes one entry and reports how many bytes that gave back.
    pub fn delete(&self, id: i64) -> rusqlite::Result<i64> {
        let removed = self
            .conn
            .execute("DELETE FROM items WHERE id = ?1", params![id])?;
        if removed == 0 {
            return Ok(0);
        }
        Ok(self.reclaim())
    }

    /// Removes everything and reports how many entries are gone and how much
    /// space the file gave back.
    pub fn clear(&self) -> rusqlite::Result<(usize, i64)> {
        let removed = self.conn.execute("DELETE FROM items", [])?;
        Ok((removed, self.reclaim()))
    }

    /// Returns `(total, images, pinned)`.
    pub fn stats(&self) -> rusqlite::Result<(i64, i64, i64)> {
        let total = self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;
        let images = self.conn.query_row(
            "SELECT COUNT(*) FROM items WHERE kind = 'image'",
            [],
            |row| row.get(0),
        )?;
        let pinned = self.conn.query_row(
            "SELECT COUNT(*) FROM items WHERE pinned = 1",
            [],
            |row| row.get(0),
        )?;
        Ok((total, images, pinned))
    }

    pub fn schema_version(&self) -> i64 {
        self.conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0)
    }

    fn page_count(&self) -> i64 {
        self.conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap_or(0)
    }

    fn page_size(&self) -> i64 {
        self.conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap_or(0)
    }

    /// Every byte this history occupies on disk, the `-wal` and `-shm` sidecars
    /// included: that is the number the user sees in a file manager. An
    /// in-memory database has no files, so it is measured in pages instead.
    pub fn disk_bytes(&self) -> i64 {
        let Some(path) = self.path.as_ref() else {
            return self.page_count() * self.page_size();
        };
        let sidecar = |suffix: &str| {
            std::fs::metadata(format!("{}{suffix}", path.display()))
                .map(|meta| meta.len() as i64)
                .unwrap_or(0)
        };
        let main = std::fs::metadata(path)
            .map(|meta| meta.len() as i64)
            .unwrap_or_else(|_| self.page_count() * self.page_size());
        main + sidecar("-wal") + sidecar("-shm")
    }

    /// Bytes SQLite is sitting on in its free list: what a vacuum can win back
    /// without losing a single entry. Deleting an entry already hands this back
    /// (see `reclaim`), so this is normally zero until writes pile up.
    pub fn reclaimable_bytes(&self) -> i64 {
        let free_pages: i64 = self
            .conn
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .unwrap_or(0);
        free_pages * self.page_size()
    }

    /// Hands the pages freed by a delete back to the file system.
    ///
    /// Measured rather than assumed, and the write-ahead log has to be folded
    /// back first: in WAL mode the pages a delete freed are still in the `-wal`
    /// file, and the database cannot be truncated until they are not. Without
    /// that checkpoint this reports "freed nothing" for a file that really does
    /// shrink (checked against SQLite both ways).
    pub fn reclaim(&self) -> i64 {
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        let before = self.disk_bytes();
        let _ = finish(&self.conn, "PRAGMA incremental_vacuum;");
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        (before - self.disk_bytes()).max(0)
    }

    /// Compacts the database file and reports the bytes won back.
    pub fn vacuum(&self) -> rusqlite::Result<i64> {
        let before = self.disk_bytes();
        // Folding the write-ahead log back into the file is part of compacting:
        // the `-wal` file is real disk usage too.
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        self.conn.execute_batch("VACUUM")?;
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok((before - self.disk_bytes()).max(0))
    }

    fn query_items(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> rusqlite::Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, |row| {
            let kind: String = row.get(1)?;
            Ok(Item {
                id: row.get(0)?,
                kind: Kind::from_db(&kind),
                snippet: row.get(2)?,
                text_len: row.get(3)?,
                mime: row.get(4)?,
                width: row.get::<_, i64>(5)? as u32,
                height: row.get::<_, i64>(6)? as u32,
                pinned: row.get::<_, i64>(7)? != 0,
                created_at: row.get(8)?,
                last_used_at: row.get(9)?,
            })
        })?;
        rows.collect()
    }

    /// Drops entries that fell out of the configured limits, and reports how many
    /// rows went. One promise the documentation makes is kept here: a pinned
    /// entry is never removed - not even by an age limit.
    fn trim(&self) -> rusqlite::Result<usize> {
        let mut removed = 0;

        if self.config.max_age_days > 0 {
            let cutoff = now_ms() - self.config.max_age_days * 86_400_000;
            removed += self.conn.execute(
                "DELETE FROM items WHERE pinned = 0 AND last_used_at < ?1",
                params![cutoff],
            )?;
        }

        removed += self.conn.execute(
            "DELETE FROM items WHERE pinned = 0 AND id NOT IN (
                 SELECT id FROM items WHERE pinned = 0
                 ORDER BY last_used_at DESC, id DESC LIMIT ?1
             )",
            params![self.config.max_items],
        )?;

        // Images are much heavier than text, so they get their own budget. The
        // prefix sum means the newest image always survives - except when the
        // budget is zero, which means "keep no images at all".
        if self.config.image_budget_bytes <= 0 {
            removed += self.conn.execute(
                "DELETE FROM items WHERE pinned = 0 AND kind = 'image'",
                [],
            )?;
        } else {
            removed += self.conn.execute(
                "DELETE FROM items WHERE pinned = 0 AND kind = 'image' AND id NOT IN (
                     SELECT id FROM (
                         SELECT id, SUM(length(data)) OVER (ORDER BY last_used_at DESC, id DESC) AS running
                         FROM items WHERE pinned = 0 AND kind = 'image'
                     ) WHERE running - length(data) <= ?1
                 )",
                params![self.config.image_budget_bytes],
            )?;
        }

        Ok(removed)
    }
}

/// Brings an existing database up to `SCHEMA_VERSION`.
///
/// v0.2 stored images as raw premultiplied RGBA pixels in `items(pixels)`;
/// v0.3 stores the encoded image the clipboard offered, and keeps timestamps in
/// milliseconds so two copies inside one second still order correctly. A copy of
/// the history made by one of the earlier prototypes lived in a separate
/// `clipboard_items` table. Both are folded into the new table, and raw-pixel
/// rows are dropped (they were only ever written by a code path that could not
/// run).
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= SCHEMA_VERSION {
        conn.execute_batch(CREATE_ITEMS)?;
        return Ok(());
    }

    if table_exists(conn, "items")? {
        let columns = table_columns(conn, "items")?;
        if columns.iter().any(|name| name == "pixels") {
            conn.execute_batch("ALTER TABLE items RENAME TO items_legacy;")?;
            conn.execute_batch(CREATE_ITEMS)?;
            conn.execute_batch(&format!(
                "INSERT OR IGNORE INTO items
                     (kind, text, width, height, hash, pinned, created_at, last_used_at)
                 SELECT kind, text, width, height, hash, 0, {created}, {used}
                 FROM items_legacy WHERE kind = 'text' AND text IS NOT NULL;
                 DROP TABLE items_legacy;
                 DROP INDEX IF EXISTS idx_items_last_used;",
                created = millis_from_legacy("created_at"),
                used = millis_from_legacy("last_used_at"),
            ))?;
        }
    }

    if table_exists(conn, "clipboard_items")? {
        let columns = table_columns(conn, "clipboard_items")?;
        let usable = ["content", "content_hash", "pinned", "created_at", "last_used_at"]
            .iter()
            .all(|needed| columns.iter().any(|name| name == needed));
        if usable {
            conn.execute_batch(CREATE_ITEMS)?;
            conn.execute_batch(&format!(
                "INSERT OR IGNORE INTO items
                     (kind, text, hash, pinned, created_at, last_used_at)
                 SELECT 'text', content, content_hash, pinned, {created}, {used}
                 FROM clipboard_items WHERE content IS NOT NULL;
                 DROP TABLE clipboard_items;",
                created = millis_from_legacy("created_at"),
                used = millis_from_legacy("last_used_at"),
            ))?;
        }
    }

    conn.execute_batch(CREATE_ITEMS)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    // Converting an existing file to incremental auto-vacuum needs a rebuild.
    conn.execute_batch("VACUUM")?;
    Ok(())
}

/// Earlier versions counted in whole seconds. Values that look like seconds are
/// widened to milliseconds; anything already large enough is left alone, so the
/// conversion is idempotent.
fn millis_from_legacy(column: &str) -> String {
    format!("CASE WHEN {column} < 100000000000 THEN {column} * 1000 ELSE {column} END")
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
}

fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect()
}

/// `LIKE` reads `%` and `_` as wildcards, so searching for `100%` would match
/// half the history. Escaping them (with the backslash the query declares as its
/// `ESCAPE` character) makes a query mean what it looks like.
fn escape_like(needle: &str) -> String {
    let mut escaped = String::with_capacity(needle.len());
    for ch in needle.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// The `items` columns an entry fills, in the order both insert paths bind them.
#[allow(clippy::type_complexity)]
fn columns_of(
    content: &Content,
) -> (
    &'static str,
    Option<&str>,
    Option<&[u8]>,
    Option<&str>,
    u32,
    u32,
) {
    match content {
        Content::Text(text) => ("text", Some(text.as_str()), None, None, 0, 0),
        Content::Image {
            mime,
            data,
            width,
            height,
        } => ("image", None, Some(data), Some(mime.as_str()), *width, *height),
    }
}

/// Steps a statement until it is done.
///
/// `Connection::execute_batch` only takes the *first* step of a statement, which
/// is not enough for `PRAGMA incremental_vacuum`: one step moves a single page,
/// so a delete that freed 1176 pages was measured leaving 1175 of them behind.
/// Finishing the statement is what makes the pragma do its whole job.
fn finish(conn: &Connection, sql: &str) -> rusqlite::Result<()> {
    let mut statement = conn.prepare(sql)?;
    let mut rows = statement.query([])?;
    while rows.next()?.is_some() {}
    Ok(())
}

fn hash_of(content: &Content) -> String {
    let mut hasher = Sha256::new();
    match content {
        Content::Text(text) => {
            hasher.update(b"text");
            hasher.update(text.as_bytes());
        }
        Content::Image {
            data,
            width,
            height,
            ..
        } => {
            // The mime type stays out of the hash: the same pixels offered as
            // PNG and as TIFF are one entry, not two.
            hasher.update(b"image");
            hasher.update(width.to_le_bytes());
            hasher.update(height.to_le_bytes());
            hasher.update(data);
        }
    }
    format!("{:x}", hasher.finalize())
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| {
            let mut home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()));
            home.push(".local/share");
            home
        });
    base.join("clipnest")
}

/// Milliseconds since the epoch: second precision made two copies inside the
/// same second order unpredictably.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::from_conn(Connection::open_in_memory().unwrap()).unwrap()
    }

    /// A store with explicit limits, on an in-memory database.
    fn store_with(config: Config) -> Store {
        Store::from_conn_with(Connection::open_in_memory().unwrap(), config).unwrap()
    }

    /// A store on a real file, which is the only way to observe disk usage.
    /// Returns the store and the directory it lives in.
    fn file_store(config: Config, name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "clipnest-db-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let store = Store::open_at(&dir.join("history.db"), config).unwrap();
        (store, dir)
    }

    #[test]
    fn pushes_and_dedupes() {
        let store = store();
        store.push(&Content::Text("hello".into())).unwrap();
        store.push(&Content::Text("hello".into())).unwrap();
        assert_eq!(store.stats().unwrap().0, 1);

        store.push(&Content::Text("world".into())).unwrap();
        assert_eq!(store.stats().unwrap().0, 2);
        let listed = store.list("", 10).unwrap();
        assert_eq!(listed[0].preview(), "world");
    }

    #[test]
    fn searches_and_keeps_pinned() {
        let store = store();
        store.push(&Content::Text("first".into())).unwrap();
        store.push(&Content::Text("second".into())).unwrap();
        let pinned = store.list("", 10).unwrap()[1].id;
        store.set_pinned(pinned, true).unwrap();

        // Pinned entries sort first even though they are older.
        assert_eq!(store.list("", 10).unwrap()[0].id, pinned);
        // Searching only matches text.
        assert_eq!(store.list("sec", 10).unwrap().len(), 1);
        assert!(store.list("nothing-matches", 10).unwrap().is_empty());
    }

    #[test]
    fn search_treats_wildcards_literally() {
        let store = store();
        store.push(&Content::Text("100% cotton".into())).unwrap();
        store.push(&Content::Text("1000 threads".into())).unwrap();
        store.push(&Content::Text("a_b".into())).unwrap();

        let hits = store.list("100%", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].preview(), "100% cotton");

        // `_` matches any single character in LIKE; here it is a real underscore.
        assert_eq!(store.list("a_b", 10).unwrap().len(), 1);
        assert!(store.list("axb", 10).unwrap().is_empty());

        // No text holds a backslash, and the escape character must not eat the
        // rest of the pattern.
        assert!(store.list("\\", 10).unwrap().is_empty());
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
    }

    #[test]
    fn trim_never_drops_pinned_entries() {
        let store = store();
        let max = Config::default().max_items;
        for index in 0..(max + 5) {
            store.push(&Content::Text(format!("entry {index}"))).unwrap();
        }
        let survivor = store.list("", max + 10).unwrap().last().unwrap().id;
        store.set_pinned(survivor, true).unwrap();
        store.push(&Content::Text("one more".into())).unwrap();

        let items = store.list("", max + 10).unwrap();
        assert!(items.iter().any(|item| item.id == survivor && item.pinned));
        assert!(items.len() as i64 <= max + 1);
    }

    #[test]
    fn the_settings_drive_the_history_size() {
        // The whole point of the config file: the limits are not compiled in.
        let store = store_with(Config {
            max_items: 4,
            ..Config::default()
        });
        for index in 0..20 {
            store.push(&Content::Text(format!("entry {index}"))).unwrap();
        }
        let items = store.list("", -1).unwrap();
        assert_eq!(items.len(), 4);
        // The newest ones are the ones that stayed.
        assert_eq!(items[0].preview(), "entry 19");
    }

    #[test]
    fn an_age_limit_spares_pinned_entries() {
        let store = store_with(Config {
            max_age_days: 1,
            ..Config::default()
        });
        store.push(&Content::Text("old".into())).unwrap();
        store.push(&Content::Text("old but pinned".into())).unwrap();
        store.push(&Content::Text("fresh".into())).unwrap();

        // Two days ago: past the limit.
        let stale = now_ms() - 2 * 86_400_000;
        let old_id = store.list("", -1).unwrap()[2].id;
        let pinned_id = store.list("", -1).unwrap()[1].id;
        store
            .conn
            .execute(
                "UPDATE items SET last_used_at = ?1",
                params![stale],
            )
            .unwrap();
        store.set_pinned(pinned_id, true).unwrap();

        // Any write trims.
        store.push(&Content::Text("another".into())).unwrap();

        let left: Vec<i64> = store.list("", -1).unwrap().iter().map(|i| i.id).collect();
        assert!(!left.contains(&old_id), "the stale entry should be gone");
        assert!(
            left.contains(&pinned_id),
            "pinning has to survive the age limit"
        );
    }

    #[test]
    fn a_zero_image_budget_keeps_no_images() {
        let store = store_with(Config {
            image_budget_bytes: 0,
            ..Config::default()
        });
        for byte in 0..4u8 {
            store
                .push(&Content::Image {
                    mime: "image/png".into(),
                    data: vec![byte; 1024],
                    width: 8,
                    height: 8,
                })
                .unwrap();
        }
        let items = store.list("", -1).unwrap();
        assert!(items.is_empty(), "{items:?}");
    }

    #[test]
    fn deleting_hands_the_space_back() {
        // The user-facing promise: removing entries removes bytes, not just rows.
        let (store, dir) = file_store(Config::default(), "reclaim");
        for byte in 0..24u8 {
            store
                .push(&Content::Image {
                    mime: "image/png".into(),
                    data: vec![byte; 200_000],
                    width: 64,
                    height: 64,
                })
                .unwrap();
        }
        let full = store.disk_bytes();
        assert!(full > 2_000_000, "expected a fat database, got {full}");

        let (removed, freed) = store.clear().unwrap();
        assert_eq!(removed, 24);
        assert!(freed > 0, "clearing freed nothing");
        assert_eq!(store.reclaimable_bytes(), 0);
        assert!(
            store.disk_bytes() < full / 4,
            "the file should have shrunk: {} -> {}",
            full,
            store.disk_bytes()
        );

        // A single delete behaves the same way.
        store.push(&Content::Text("kept".into())).unwrap();
        let id = store.list("", -1).unwrap()[0].id;
        let freed = store.delete(id).unwrap();
        assert_eq!(store.stats().unwrap().0, 0);
        assert!(freed >= 0);
        // Deleting something that is not there is not a problem.
        assert_eq!(store.delete(id).unwrap(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restores_a_backup_without_reshuffling() {
        let store = store();
        // Two days apart, newest first.
        let now = now_ms();
        let day = 86_400_000;
        assert_eq!(
            store
                .restore(&Content::Text("newer".into()), false, now, now, None)
                .unwrap(),
            Restored::Added
        );
        assert_eq!(
            store
                .restore(
                    &Content::Text("older".into()),
                    true,
                    now - day,
                    now - day,
                    None
                )
                .unwrap(),
            Restored::Added
        );

        let items = store.list("", -1).unwrap();
        assert_eq!(items.len(), 2);
        // The timestamps came from the backup, not from the import. The pinned
        // entry sorts first, so the fields are looked up instead of indexed.
        let newer = items.iter().find(|item| item.preview() == "newer").unwrap();
        let older = items.iter().find(|item| item.preview() == "older").unwrap();
        assert_eq!(newer.last_used_at, now);
        assert!(!newer.pinned);
        assert!(older.pinned);
        assert_eq!(older.last_used_at, now - day);
        assert_eq!(items[0].preview(), "older");

        // Importing the same backup again changes nothing but pins.
        let older_id = older.id;
        store.set_pinned(older_id, false).unwrap();
        store
            .conn
            .execute("UPDATE items SET last_used_at = ?1 WHERE id = ?2", params![now, older_id])
            .unwrap();
        assert_eq!(
            store
                .restore(
                    &Content::Text("older".into()),
                    true,
                    now - day,
                    now - day,
                    None
                )
                .unwrap(),
            Restored::Present
        );
        let items = store.list("", -1).unwrap();
        assert_eq!(items.len(), 2);
        let older = items.iter().find(|item| item.id == older_id).unwrap();
        // The pin came from the backup, the live timestamp did not move back.
        assert!(older.pinned);
        assert_eq!(older.last_used_at, now);
        assert!(items.iter().any(|item| item.preview() == "newer"));
    }

    #[test]
    fn trims_an_imported_history_too() {
        let store = store_with(Config {
            max_items: 3,
            ..Config::default()
        });
        let now = now_ms();
        for index in 0..10 {
            store
                .restore(
                    &Content::Text(format!("backed up {index}")),
                    false,
                    now - index,
                    now - index,
                    None,
                )
                .unwrap();
        }
        assert_eq!(store.trim_now().unwrap() as i64, 7);
        assert_eq!(store.list("", -1).unwrap().len(), 3);
    }

    #[test]
    fn image_round_trip() {
        let store = store();
        let content = Content::Image {
            mime: "image/png".into(),
            data: vec![1, 2, 3, 4],
            width: 2,
            height: 1,
        };
        store.push(&content).unwrap();
        let item = store.list("", 10).unwrap().remove(0);
        assert!(item.is_image());
        assert_eq!(item.preview(), "Image 2x1");
        match store.content(item.id).unwrap() {
            Some(Content::Image { data, .. }) => assert_eq!(data, vec![1, 2, 3, 4]),
            other => panic!("unexpected content: {other:?}"),
        }
    }

    #[test]
    fn waits_for_the_daemon_instead_of_failing() {
        let store = store();
        let timeout: i64 = store
            .conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
    }

    #[test]
    fn a_read_only_open_refuses_a_missing_file() {
        // The CLI relies on this error to know it may create the database.
        let path = std::env::temp_dir().join(format!(
            "clipnest-absent-{}-{}.db",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_file(&path);
        assert!(Store::open_read_only_at(&path, Config::default()).is_err());
    }

    #[test]
    fn migrates_the_legacy_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE items (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind TEXT NOT NULL, text TEXT, width INTEGER NOT NULL DEFAULT 0,
                 height INTEGER NOT NULL DEFAULT 0, stride INTEGER NOT NULL DEFAULT 0,
                 pixels BLOB, hash TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL,
                 last_used_at INTEGER NOT NULL
             );
             INSERT INTO items (kind, text, hash, created_at, last_used_at)
             VALUES ('text', 'kept', 'h1', 10, 10);
             INSERT INTO items (kind, width, height, stride, pixels, hash, created_at, last_used_at)
             VALUES ('image', 1, 1, 4, x'000000ff', 'h2', 11, 11);
             CREATE TABLE clipboard_items (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL,
                 content_hash TEXT NOT NULL UNIQUE, content_type TEXT NOT NULL DEFAULT 'text',
                 created_at INTEGER NOT NULL, last_used_at INTEGER NOT NULL,
                 use_count INTEGER NOT NULL DEFAULT 0,
                 pinned INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO clipboard_items (content, content_hash, created_at, last_used_at, pinned)
             VALUES ('ancient', 'h3', 5, 5, 1);",
        )
        .unwrap();

        let store = Store::from_conn(conn).unwrap();
        assert_eq!(store.schema_version(), SCHEMA_VERSION);
        let items = store.list("", 10).unwrap();
        let previews: Vec<String> = items.iter().map(Item::preview).collect();
        assert!(previews.contains(&"kept".to_string()));
        assert!(previews.contains(&"ancient".to_string()));
        assert!(!previews.iter().any(|preview| preview.starts_with("Image")));
        assert!(items.iter().any(|item| item.pinned));

        // Old entries counted in seconds and are widened to milliseconds, so
        // they still display as "long ago" rather than as the year 1970.
        let kept = items.iter().find(|item| item.preview() == "kept").unwrap();
        assert_eq!(kept.created_at, 10_000);
        let ancient = items.iter().find(|item| item.preview() == "ancient").unwrap();
        assert_eq!(ancient.created_at, 5_000);
        assert!(ancient.pinned);
    }
}
