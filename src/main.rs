mod app;
mod backup;
mod config;
mod db;
mod install;
mod paste;

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use glib::variant::ToVariant;

const HELP: &str = "\
clipnest - minimal clipboard history for GNOME on Wayland

usage: clipnest <command> [options]

commands:
  daemon             run the background service (used by systemd)
  setup              finish an installation: extension, shortcut, service
  toggle             show or hide the clipboard panel
  list               print the recent history
  search <query>     print the history entries containing a query
  get <id>           print one entry (text, or an image with --out)
  pick <n>           copy the n-th newest entry back to the clipboard
  paste <n>          copy the n-th newest entry and paste it where you are
  paste-access       ask for permission to paste into other windows
  paste-reset        forget that permission (the history is left alone)
  pin <id>           keep an entry through trimming
  unpin <id>         stop keeping an entry
  delete <id>        remove one entry, and hand its space back
  clear              delete the whole history
  export <dir>       write the history into a directory
  import <dir>       read a history back out of a directory
  config             print the settings in use, and where they come from
  stats              print counters, file size and reclaimable space
  vacuum             compact the database file
  status             print daemon status
  doctor             check the whole installation and suggest fixes
  setup-shortcut     register the clipboard shortcut in GNOME
  remove-shortcut    unregister that shortcut
  install-extension  install/enable the GNOME Shell extension
  version            print version
  help               print this help

options:
  --limit N          how many entries to list (default 50; export: all)
  --search QUERY     list only the entries containing QUERY
  --json             machine readable output for list
  --out FILE         write a fetched entry to FILE
  --raw              stream a fetched image to stdout
  --force            export into a directory that is not empty
  --binding ACCEL    keybinding for setup and setup-shortcut
";

fn main() -> ExitCode {
    let mut argv = std::env::args().skip(1);
    let command = argv.next().unwrap_or_else(|| "daemon".into());
    let args = Args::parse(argv.collect());

    if !args.problems.is_empty() {
        // A typo used to be a warning and then the command ran anyway, with the
        // option silently dropped; `--limt 5` listing the whole history is
        // exactly the kind of quiet wrong this refuses to do.
        for problem in &args.problems {
            eprintln!("clipnest: {problem}");
        }
        eprintln!("         run 'clipnest help' for the options this build takes");
        return ExitCode::from(2);
    }

    match command.as_str() {
        "daemon" | "run" => {
            app::run();
            ExitCode::SUCCESS
        }
        "toggle" => {
            let params = (activation_token(),).to_variant();
            match call_or_fail("Toggle", Some(&params)) {
                Ok(_) => ExitCode::SUCCESS,
                Err(code) => code,
            }
        }
        "status" => {
            let reply = match call_or_fail("Status", None) {
                Ok(reply) => reply,
                Err(code) => return code,
            };
            println!("{}", reply.child_value(0).str().unwrap_or("unknown"));
            ExitCode::SUCCESS
        }
        "stats" => stats(),
        "config" => print_config(),
        "export" => {
            let Some(dir) = args.positional.first() else {
                eprintln!("usage: clipnest export <dir> [--limit N] [--force]");
                return ExitCode::FAILURE;
            };
            let limit = match args.limit(-1) {
                Ok(limit) => limit,
                Err(err) => {
                    eprintln!("clipnest: {err}");
                    return ExitCode::FAILURE;
                }
            };
            run_export(dir, limit, args.has("--force"))
        }
        "import" => {
            let Some(dir) = args.positional.first() else {
                eprintln!("usage: clipnest import <dir>");
                return ExitCode::FAILURE;
            };
            run_import(dir)
        }
        "list" | "search" => {
            let limit = match args.limit(50) {
                Ok(limit) => limit.min(100_000) as u32,
                Err(err) => {
                    eprintln!("clipnest: {err}");
                    return ExitCode::FAILURE;
                }
            };
            let json = args.has("--json");
            let wanted = if command == "search" {
                // `clipnest search hello world` looks for the whole phrase, not
                // for whichever word happened to come first.
                if args.positional.is_empty() {
                    eprintln!("usage: clipnest search <query> [--limit N] [--json]");
                    return ExitCode::FAILURE;
                }
                args.positional.join(" ")
            } else {
                args.value("--search").unwrap_or("").to_string()
            };
            let needle = wanted.as_str();
            match bus_call("Search", Some(&(needle, limit).to_variant())) {
                Ok(reply) => {
                    print_rows(&rows_of(&reply.child_value(0)), json);
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    // A daemon left over from an older build has no Search yet.
                    if err.contains("UnknownMethod") {
                        eprintln!(
                            "clipnest: the running daemon is older than this binary; restart it:"
                        );
                        eprintln!("         systemctl --user restart clipnest");
                    } else {
                        eprintln!(
                            "clipnest: daemon unavailable ({err}), reading the database directly"
                        );
                    }
                    list_offline(needle, limit, json)
                }
            }
        }
        "get" => {
            let Some(id) = args.id() else {
                eprintln!("usage: clipnest get <id> [--out FILE] [--raw]");
                return ExitCode::FAILURE;
            };
            let out = args.value("--out").map(str::to_string);
            let raw = args.has("--raw");
            match bus_call("GetItem", Some(&(id,).to_variant())) {
                Ok(reply) => match reply.child_value(0).str().unwrap_or("missing") {
                    "text" => write_text(reply.child_value(1).str().unwrap_or(""), out.as_deref()),
                    "image" => {
                        let mime = reply.child_value(4).str().unwrap_or("image/png").to_string();
                        let encoded = reply.child_value(5).str().unwrap_or("").to_string();
                        let data = glib::base64_decode(&encoded);
                        write_image(&data, &mime, out.as_deref(), raw)
                    }
                    _ => {
                        eprintln!("clipnest: no entry with id {id}");
                        ExitCode::FAILURE
                    }
                },
                Err(err) => {
                    eprintln!("clipnest: daemon unavailable ({err}), reading the database directly");
                    get_offline(id, out.as_deref(), raw)
                }
            }
        }
        "pick" => {
            let Some(index) = args.id() else {
                eprintln!("usage: clipnest pick <n>    (1 is the newest entry)");
                return ExitCode::FAILURE;
            };
            let id = match nth_entry(index) {
                Ok(id) => id,
                Err(code) => return code,
            };
            match call_or_fail("Pick", Some(&(id,).to_variant())) {
                Ok(_) => {
                    println!("entry {id} is on the clipboard");
                    ExitCode::SUCCESS
                }
                Err(code) => code,
            }
        }
        "paste" => {
            let Some(index) = args.id() else {
                eprintln!("usage: clipnest paste <n>    (1 is the newest entry)");
                return ExitCode::FAILURE;
            };
            let id = match nth_entry(index) {
                Ok(id) => id,
                Err(code) => return code,
            };
            match call_or_fail("Paste", Some(&(id,).to_variant())) {
                Ok(reply) => {
                    println!("{}", reply.child_value(0).str().unwrap_or("copied"));
                    ExitCode::SUCCESS
                }
                Err(code) => code,
            }
        }
        "paste-access" => paste_access(),
        "paste-reset" => paste_reset(),
        "pin" | "unpin" => {
            let Some(id) = args.id() else {
                eprintln!("usage: clipnest {command} <id>");
                return ExitCode::FAILURE;
            };
            let pinned = command == "pin";
            match call_or_fail("SetPin", Some(&(id, pinned).to_variant())) {
                Ok(_) => {
                    println!("entry {id} {}", if pinned { "pinned" } else { "unpinned" });
                    ExitCode::SUCCESS
                }
                Err(code) => code,
            }
        }
        "delete" => {
            let Some(id) = args.id() else {
                eprintln!("usage: clipnest delete <id>");
                return ExitCode::FAILURE;
            };
            match call_or_fail("Delete", Some(&(id,).to_variant())) {
                Ok(reply) => {
                    let freed = reply.child_value(0).get::<u64>().unwrap_or(0);
                    println!("entry {id} deleted{}", freed_note(freed));
                    ExitCode::SUCCESS
                }
                Err(code) => code,
            }
        }
        "clear" => match call_or_fail("Clear", None) {
            Ok(reply) => {
                let removed = reply.child_value(0).get::<u32>().unwrap_or(0);
                let freed = reply.child_value(1).get::<u64>().unwrap_or(0);
                println!("removed {removed} entries{}", freed_note(freed));
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        "vacuum" => match call_or_fail("Vacuum", None) {
            Ok(reply) => {
                let freed = reply.child_value(0).get::<u64>().unwrap_or(0);
                println!("reclaimed {}", app::human_bytes(freed as i64));
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        "doctor" => doctor(),
        "setup" => {
            let code = install::setup(args.value("--binding"));
            // The daemon is up again at this point, so the one thing that needs
            // the user's answer can be asked for now - while they are still
            // looking at the screen - instead of during the first click.
            if code == ExitCode::SUCCESS {
                ask_for_paste_permission();
            }
            code
        }
        "setup-shortcut" => install::setup_shortcut(args.value("--binding")),
        "remove-shortcut" => install::remove_shortcut(),
        "install-extension" => install::install_extension(),
        "version" | "--version" | "-V" => {
            println!("clipnest {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("clipnest: unknown command '{other}'\n");
            eprint!("{HELP}");
            ExitCode::FAILURE
        }
    }
}

/// One row of `clipnest list`, as the daemon reports it.
struct Row {
    id: u32,
    kind: String,
    preview: String,
    width: u32,
    height: u32,
    pinned: bool,
}

impl Row {
    fn from_variant(value: &glib::Variant) -> Self {
        Row {
            id: value.child_value(0).get().unwrap_or(0),
            kind: value.child_value(1).str().unwrap_or("text").to_string(),
            preview: value.child_value(2).str().unwrap_or("").to_string(),
            width: value.child_value(3).get().unwrap_or(0),
            height: value.child_value(4).get().unwrap_or(0),
            pinned: value.child_value(5).get().unwrap_or(false),
        }
    }

    fn json(&self) -> String {
        format!(
            "{{\"id\":{},\"kind\":\"{}\",\"preview\":\"{}\",\"width\":{},\"height\":{},\"pinned\":{}}}",
            self.id,
            json_escape(&self.kind),
            json_escape(&self.preview),
            self.width,
            self.height,
            self.pinned
        )
    }
}

fn rows_of(array: &glib::Variant) -> Vec<Row> {
    (0..array.n_children())
        .map(|index| Row::from_variant(&array.child_value(index)))
        .collect()
}

fn print_rows(rows: &[Row], json: bool) {
    if json {
        println!("{}", json_array(rows));
        return;
    }
    for row in rows {
        let marker = if row.pinned { "*" } else { " " };
        println!("{:>5} {} {}", row.id, marker, row.preview);
    }
}

/// The rows as one JSON array. Kept separate from `print_rows` so the escaping
/// can be tested without a terminal.
fn json_array(rows: &[Row]) -> String {
    let body: Vec<String> = rows.iter().map(Row::json).collect();
    format!("[{}]", body.join(","))
}

/// Escapes the four characters JSON reserves plus every other control code.
///
/// Everything else is copied through as-is: the output is UTF-8, so Persian
/// text and emoji are already legal JSON and re-encoding them would only make
/// the output unreadable to a human looking over the shoulder.
fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
}

fn list_offline(needle: &str, limit: u32, json: bool) -> ExitCode {
    let store = match open_store() {
        Ok(store) => store,
        Err(code) => return code,
    };
    let items = match store.list(needle, limit as i64) {
        Ok(items) => items,
        Err(err) => {
            eprintln!("clipnest: cannot read the history: {err}");
            return ExitCode::FAILURE;
        }
    };
    let rows: Vec<Row> = items
        .iter()
        .map(|item| Row {
            id: item.id as u32,
            kind: item.kind.as_str().to_string(),
            preview: item.preview(),
            width: item.width,
            height: item.height,
            pinned: item.pinned,
        })
        .collect();
    print_rows(&rows, json);
    ExitCode::SUCCESS
}

fn get_offline(id: u32, out: Option<&str>, raw: bool) -> ExitCode {
    let store = match open_store() {
        Ok(store) => store,
        Err(code) => return code,
    };
    match store.content(id as i64) {
        Ok(Some(db::Content::Text(text))) => write_text(&text, out),
        Ok(Some(db::Content::Image { mime, data, .. })) => write_image(&data, &mime, out, raw),
        Ok(None) => {
            eprintln!("clipnest: no entry with id {id}");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("clipnest: cannot read the entry: {err}");
            ExitCode::FAILURE
        }
    }
}

fn open_store() -> Result<db::Store, ExitCode> {
    match db::Store::open_reader().or_else(|_| db::Store::open_default()) {
        Ok(store) => Ok(store),
        Err(err) => {
            eprintln!("clipnest: cannot read {}: {err}", db::Store::path().display());
            Err(ExitCode::FAILURE)
        }
    }
}

fn write_text(text: &str, out: Option<&str>) -> ExitCode {
    match out {
        Some(path) => match std::fs::write(path, text) {
            Ok(()) => {
                println!("wrote {} characters to {path}", text.chars().count());
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("clipnest: cannot write {path}: {err}");
                ExitCode::FAILURE
            }
        },
        None => {
            // Written as-is so `clipnest get 12 > file` stays lossless.
            print!("{text}");
            if std::io::stdout().is_terminal() && !text.ends_with('\n') {
                println!();
            }
            let _ = std::io::stdout().flush();
            ExitCode::SUCCESS
        }
    }
}

fn write_image(data: &[u8], mime: &str, out: Option<&str>, raw: bool) -> ExitCode {
    if data.is_empty() {
        eprintln!("clipnest: this entry holds no image data");
        return ExitCode::FAILURE;
    }
    let streaming = std::io::stdout().is_terminal() && !raw;
    let target = match out {
        Some(path) => Some(path.to_string()),
        None if raw || !std::io::stdout().is_terminal() => None,
        None if streaming => {
            eprintln!("clipnest: this entry is an image; add --out FILE, or --raw to stream it:");
            eprintln!(
                "         clipnest get <id> --out clipboard.{}",
                backup::extension_for(mime)
            );
            return ExitCode::FAILURE;
        }
        None => None,
    };

    match target {
        Some(path) => match std::fs::write(&path, data) {
            Ok(()) => {
                println!("wrote {} to {path}", app::human_bytes(data.len() as i64));
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("clipnest: cannot write {path}: {err}");
                ExitCode::FAILURE
            }
        },
        None => match std::io::stdout().write_all(data) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("clipnest: cannot write to stdout: {err}");
                ExitCode::FAILURE
            }
        },
    }
}

/// The id of the entry a user numbers as `n`, where 1 is the newest. Shared by
/// `pick` and `paste` so the two cannot drift apart.
fn nth_entry(index: u32) -> Result<u32, ExitCode> {
    if index == 0 {
        eprintln!("clipnest: entries are numbered from 1");
        return Err(ExitCode::FAILURE);
    }
    let reply = call_or_fail("List", Some(&(index,).to_variant()))?;
    let array = reply.child_value(0);
    if array.n_children() < index as usize {
        eprintln!(
            "clipnest: the history only holds {} entries",
            array.n_children()
        );
        return Err(ExitCode::FAILURE);
    }
    Ok(array
        .child_value(index as usize - 1)
        .child_value(0)
        .get::<u32>()
        .unwrap_or(0))
}

/// How long `clipnest paste-access` waits for the permission dialog. Generous:
/// the question is answered by a person, who may be looking the other way.
const PASTE_ACCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// `clipnest paste-access`: asks for the permission to type into other windows
/// and waits for the answer, so a failure is reported here rather than on the
/// first entry somebody clicks.
fn paste_access() -> ExitCode {
    if config::Config::load().config.paste_key.is_none() {
        println!("auto-paste is switched off in the settings (paste_key = none)");
        return ExitCode::SUCCESS;
    }

    // The daemon owns the session, so it is the one that asks. Its answer is
    // also the app the permission will be remembered for.
    match call_when_ready("PreparePaste", None) {
        Ok(reply) => {
            let note = reply.child_value(0).str().unwrap_or("").to_string();
            println!("asked the daemon for a paste session: {note}");
        }
        Err(err) => {
            eprintln!("clipnest: the daemon did not answer ({err})");
            eprintln!("         is it running? try: systemctl --user restart clipnest");
            return ExitCode::FAILURE;
        }
    }

    println!("waiting for the permission dialog (Ctrl+C gives up, the answer is remembered)");
    let deadline = std::time::Instant::now() + PASTE_ACCESS_TIMEOUT;
    let mut waited = false;
    loop {
        match paste_state() {
            Ok((state, reason, note)) => match state {
                paste::State::Ready => {
                    println!("\u{2705} auto-paste is ready: {note}");
                    return ExitCode::SUCCESS;
                }
                // The daemon knows what happened, so the answer is reported the
                // moment it exists instead of after the timeout. A cancelled
                // dialog is an answer too, and the reason says which one it was
                // - which is the whole next step, so it is printed.
                paste::State::Failed => {
                    eprintln!("\u{274c} {note}");
                    let advice = reason.advice();
                    if !advice.is_empty() {
                        eprintln!("   {advice}");
                    }
                    return ExitCode::FAILURE;
                }
                paste::State::Idle | paste::State::Asking => {
                    if !waited {
                        waited = true;
                        println!("  {note}");
                    }
                    if std::time::Instant::now() >= deadline {
                        eprintln!("\u{26a0}\u{fe0f}  still waiting: {note}");
                        eprintln!("   answer the dialog, then run: clipnest doctor");
                        return ExitCode::FAILURE;
                    }
                }
            },
            Err(err) => {
                eprintln!("clipnest: the daemon stopped answering ({err})");
                return ExitCode::FAILURE;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}

/// `clipnest paste-reset`: hands the permission back by deleting the restore
/// token, which is what makes the portal ask again.
///
/// Deliberately not called `paste-forget-permission`: the point is that it is
/// the *permission* that is forgotten. The history lives in another file, and a
/// command that could lose it while revoking a grant would be a footgun.
fn paste_reset() -> ExitCode {
    let path = paste::token_path();
    match paste::forget_token() {
        Ok(true) => {
            println!("forgot the paste permission (removed {})", path.display());
            println!("the history was not touched");
            println!("ask for it again with: clipnest paste-access");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("nothing to forget: {} does not exist", path.display());
            println!("the permission was never granted, or was already reset");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("clipnest: {err}");
            ExitCode::FAILURE
        }
    }
}

/// What the daemon last knew about auto-paste: the state, why it is not ready,
/// and the sentence that explains it.
fn paste_state() -> Result<(paste::State, paste::Reason, String), String> {
    let reply = bus_call("PasteStatus", None)?;
    let state = paste::State::parse(reply.child_value(0).str().unwrap_or(""));
    let reason = paste::Reason::parse(reply.child_value(1).str().unwrap_or(""));
    let note = reply.child_value(2).str().unwrap_or("").to_string();
    Ok((state, reason, note))
}

/// What `setup` says about auto-paste. Never waits: the dialog is answered while
/// the rest of the installation is being read, and `clipnest doctor` reports the
/// outcome afterwards.
fn ask_for_paste_permission() {
    if config::Config::load().config.paste_key.is_none() {
        return;
    }
    if let Ok(reply) = call_when_ready("PreparePaste", None) {
        let note = reply.child_value(0).str().unwrap_or("").to_string();
        println!();
        println!("paste: {note}");
        println!(
            "       a permission dialog may appear now; allowing it (once) makes \
             picking an entry paste it"
        );
        println!("       if it does not, run 'clipnest paste-access'");
    }
}

/// How much space a delete or a clear handed back, phrased for a report.
fn freed_note(bytes: u64) -> String {
    if bytes == 0 {
        String::new()
    } else {
        format!(", freed {}", app::human_bytes(bytes as i64))
    }
}

/// `clipnest stats`: counters, file size and what a vacuum would win back.
///
/// All of it can be read from the file, so this works with the daemon stopped.
fn stats() -> ExitCode {
    match call_when_ready("Status", None) {
        Ok(reply) => {
            let version = reply.child_value(1).str().unwrap_or("?").to_string();
            let total = reply.child_value(2).get::<u32>().unwrap_or(0);
            let images = reply.child_value(3).get::<u32>().unwrap_or(0);
            let pinned = reply.child_value(4).get::<u32>().unwrap_or(0);
            let (database, reclaimable) = match bus_call("DiskUsage", None) {
                Ok(reply) => (
                    reply.child_value(0).get::<u64>().unwrap_or(0),
                    reply.child_value(1).get::<u64>().unwrap_or(0),
                ),
                Err(_) => (reply.child_value(5).get::<u64>().unwrap_or(0), 0),
            };
            println!("daemon     clipnest {version}");
            print_counters(total, images, pinned, database, reclaimable);
            ExitCode::SUCCESS
        }
        Err(_) => {
            let store = match open_store() {
                Ok(store) => store,
                Err(code) => return code,
            };
            let (total, images, pinned) = store.stats().unwrap_or((0, 0, 0));
            println!("daemon     not running (read from the database)");
            print_counters(
                total.max(0) as u32,
                images.max(0) as u32,
                pinned.max(0) as u32,
                store.disk_bytes().max(0) as u64,
                store.reclaimable_bytes().max(0) as u64,
            );
            ExitCode::SUCCESS
        }
    }
}

fn print_counters(total: u32, images: u32, pinned: u32, database: u64, reclaimable: u64) {
    println!("entries    {total}");
    println!("images     {images}");
    println!("pinned     {pinned}");
    println!(
        "database   {} at {}",
        app::human_bytes(database as i64),
        db::Store::path().display()
    );
    if reclaimable > 0 {
        println!(
            "reclaimable {} (run 'clipnest vacuum')",
            app::human_bytes(reclaimable as i64)
        );
    }
}

/// `clipnest config`: the settings in force, where they came from, and anything
/// in the file that could not be understood.
fn print_config() -> ExitCode {
    let loaded = config::Config::load();
    println!("clipnest {} settings\n", env!("CARGO_PKG_VERSION"));
    if loaded.found {
        println!("file       {}", loaded.path.display());
    } else {
        println!(
            "file       {} (not found, using the defaults)",
            loaded.path.display()
        );
    }
    println!();
    for (key, value) in loaded.config.rows() {
        println!("{key:<17} {value}");
    }

    if !loaded.notes.is_empty() {
        println!("\nproblems:");
        for note in &loaded.notes {
            println!("  - {note}");
        }
        return ExitCode::FAILURE;
    }
    if loaded.found {
        println!("\n0 turns that kind of capture off; max_age_days=0 keeps entries forever.");
    } else {
        println!(
            "\nWrite this into {} to change any of it:\n",
            loaded.path.display()
        );
        print!("{}", config::Config::example());
    }
    ExitCode::SUCCESS
}

/// `clipnest export <dir>`.
fn run_export(dir: &str, limit: i64, force: bool) -> ExitCode {
    let store = match open_store() {
        Ok(store) => store,
        Err(code) => return code,
    };
    match backup::export(&store, std::path::Path::new(dir), limit, force) {
        Ok(summary) => {
            println!(
                "exported {} entries ({} text, {} images, {} pinned, {}) to {dir}",
                summary.entries(),
                summary.text,
                summary.images,
                summary.pinned,
                app::human_bytes(summary.bytes)
            );
            report_notes(&summary.notes, summary.skipped);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("clipnest: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `clipnest import <dir>`.
fn run_import(dir: &str) -> ExitCode {
    // Anything already in the history is left alone, so importing the same
    // backup twice is a merge rather than a duplication.
    let store = match db::Store::open_default() {
        Ok(store) => store,
        Err(err) => {
            eprintln!("clipnest: cannot open {}: {err}", db::Store::path().display());
            return ExitCode::FAILURE;
        }
    };
    match backup::import(&store, std::path::Path::new(dir)) {
        Ok(summary) => {
            println!(
                "imported {} entries from {dir} ({} text, {} images, {} already present)",
                summary.entries(),
                summary.text,
                summary.images,
                summary.duplicates
            );
            report_notes(&summary.notes, summary.skipped);
            // Nothing went through PushText, so a panel that is already built
            // would keep showing the old list until it is told.
            let _ = bus_call("Refresh", None);
            if summary.bytes > 0 {
                println!("tip: 'clipnest vacuum' compacts the file after a large import");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("clipnest: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Prints what a backup run could not handle.
fn report_notes(notes: &[String], skipped: usize) {
    for note in notes {
        eprintln!("clipnest: {note}");
    }
    if skipped > 0 {
        eprintln!("clipnest: {skipped} entries were skipped");
    }
}

fn bus_call(method: &str, params: Option<&glib::Variant>) -> Result<glib::Variant, String> {
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
        .map_err(|err| format!("cannot reach the session bus: {err}"))?;

    connection
        .call_sync(
            Some(app::BUS_NAME),
            app::OBJECT_PATH,
            app::INTERFACE,
            method,
            params,
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .map_err(|err| err.message().to_string())
}

/// Like `bus_call`, but waits out a cold start and prints the usual "is the
/// daemon running?" advice.
fn call_or_fail(method: &str, params: Option<&glib::Variant>) -> Result<glib::Variant, ExitCode> {
    call_when_ready(method, params).map_err(|err| {
        eprintln!("clipnest: {method} failed: {err}");
        eprintln!("         is the daemon running? try: systemctl --user restart clipnest");
        ExitCode::FAILURE
    })
}

fn doctor() -> ExitCode {
    let mut problems = 0usize;
    let mut warnings = 0usize;

    println!(
        "clipnest doctor {} on {}\n",
        env!("CARGO_PKG_VERSION"),
        session_type()
    );
    // Which of the two supported installations is in charge changes what the
    // rest of the lines mean, so it is said first.
    match install::layout() {
        install::Layout::Package => pass("layout: system package (/usr/bin/clipnest)"),
        install::Layout::User => pass(&format!(
            "layout: user install ({})",
            install::user_bin().display()
        )),
        // Both, which is the state that looks fine and is not: the copy in the
        // user's home comes first in `$PATH`, so what runs is not what the
        // package manager installed.
        install::Layout::Both { running } => {
            warnings += 1;
            warn(&format!(
                "layout: a package and a user install are both present; {} is the one running",
                match running {
                    install::Running::User => format!("{}", install::user_bin().display()),
                    install::Running::Package => "/usr/bin/clipnest".to_string(),
                }
            ));
            hint("make uninstall   (drops the user copy; the package and the history stay)");
        }
        install::Layout::Local => pass("layout: running from a build directory"),
    }

    // The daemon answers only if it is running and speaking this protocol.
    match call_when_ready("Status", None) {
        Ok(reply) => {
            let version = reply.child_value(1).str().unwrap_or("?").to_string();
            let summary = reply.child_value(0).str().unwrap_or("").to_string();
            if version == env!("CARGO_PKG_VERSION") {
                pass(&format!("daemon: {summary}"));
            } else {
                warnings += 1;
                warn(&format!(
                    "daemon is running version {version}, this binary is {}",
                    env!("CARGO_PKG_VERSION")
                ));
                hint("systemctl --user restart clipnest");
            }
        }
        Err(err) => {
            problems += 1;
            fail(&format!("daemon is not answering: {err}"));
            hint("systemctl --user restart clipnest");
        }
    }

    // Storage.
    let path = db::Store::path();
    if path.exists() {
        match db::Store::open_reader().or_else(|_| db::Store::open_default()) {
            Ok(store) => {
                let (total, images, pinned) = store.stats().unwrap_or((0, 0, 0));
                let schema = store.schema_version();
                if schema == db::SCHEMA_VERSION {
                    pass(&format!(
                        "database: {} entries ({images} images, {pinned} pinned), {}, schema v{schema}",
                        total,
                        app::human_bytes(store.disk_bytes())
                    ));
                } else {
                    warnings += 1;
                    warn(&format!(
                        "database schema is v{schema}, expected v{}",
                        db::SCHEMA_VERSION
                    ));
                    hint("systemctl --user restart clipnest");
                }
                // A file with a lot of free pages is one a vacuum would shrink.
                let reclaimable = store.reclaimable_bytes();
                if reclaimable > 16 * 1024 * 1024 {
                    warnings += 1;
                    warn(&format!(
                        "{} of the database file is free space left by deleted entries",
                        app::human_bytes(reclaimable)
                    ));
                    hint("clipnest vacuum");
                }
            }
            Err(err) => {
                problems += 1;
                fail(&format!("cannot open {}: {err}", path.display()));
            }
        }
    } else {
        warnings += 1;
        warn("no history database yet (it appears the first time something is copied)");
    }

    // The shell extension that feeds us the clipboard. It can be installed
    // twice over and gnome-shell loads the first copy it finds, so the report has
    // to name the copy that is really in use - and never suggest deleting a file
    // that a package put there.
    let candidates = install::extension_candidates();
    match install::installed_extension_dir() {
        None => {
            problems += 1;
            fail("the GNOME Shell extension is not installed");
            hint("clipnest setup");
        }
        Some(dir) => {
            let user_copy = dir == candidates[0];
            let current = install::extension_is_current(&dir);
            let packaged_is_current = candidates
                .iter()
                .skip(1)
                .any(|other| other.is_dir() && install::extension_is_current(other));
            let located = if user_copy {
                format!(
                    "{} (a user copy, which wins over a packaged one)",
                    dir.display()
                )
            } else {
                format!("{} (packaged with the system)", dir.display())
            };
            match (current, user_copy, packaged_is_current) {
                (true, ..) => pass(&format!("extension files: up to date in {located}")),
                (false, true, true) => {
                    warnings += 1;
                    warn(&format!(
                        "the user copy in {} shadows the packaged one and is older",
                        dir.display()
                    ));
                    hint("clipnest install-extension   (or remove the copy under ~/.local)");
                }
                (false, ..) => {
                    warnings += 1;
                    warn(&format!(
                        "the extension files in {located} are from a different build"
                    ));
                    hint("clipnest install-extension   (then log out and back in)");
                }
            }
        }
    }

    let (state, loaded_version) = gnome_extension_info();
    let bundled_version = bundled_extension_version();
    // Read again by the session line further down: whether the shell really is
    // feeding the daemon is the difference between "Wayland" and "Wayland, and
    // nothing will be captured".
    let extension_running = install::extension_enabled() && state.eq_ignore_ascii_case("active");
    if !install::extension_enabled() {
        problems += 1;
        fail(&format!("extension {} is not in enabled-extensions", install::extension_uuid()));
        hint(&format!("gnome-extensions enable {}", install::extension_uuid()));
    } else if state.eq_ignore_ascii_case("active") {
        // An ACTIVE extension can still be running older files: gnome-shell only
        // rescans the extension folder when the session starts.
        match (bundled_version, loaded_version) {
            (Some(bundled), Some(loaded)) if bundled != loaded => {
                problems += 1;
                fail(&format!(
                    "extension is running version {loaded}, this build ships version {bundled}"
                ));
                hint("log out and back in so gnome-shell loads the new extension files");
            }
            _ => pass(&format!(
                "extension {}: enabled and running in this session",
                install::extension_uuid()
            )),
        }
    } else {
        // An enabled extension that is not ACTIVE cannot feed the clipboard:
        // gnome-shell only picks up changed files at session start.
        problems += 1;
        fail(&format!(
            "extension {} is enabled but {state} in this session",
            install::extension_uuid()
        ));
        if state.eq_ignore_ascii_case("error") {
            hint("journalctl /usr/bin/gnome-shell -b | grep -i clipnest");
        } else {
            hint("log out and back in so gnome-shell loads the extension files");
        }
    }

    // Shortcut.
    match install::shortcut_settings() {
        None => {
            problems += 1;
            fail("the GNOME media-keys settings schemas are missing");
        }
        Some((bindings, binding, command)) => {
            let registered = bindings.iter().any(|path| path == install::shortcut_path());
            let command_ok = command.contains(" toggle");
            match classify_shortcut(registered, command_ok, &binding) {
                ShortcutState::Broken => {
                    problems += 1;
                    fail(&format!(
                        "shortcut is not usable (listed: {registered}, binding: '{binding}', command: '{command}')"
                    ));
                    hint("clipnest setup-shortcut");
                }
                // A binding the user picked is not a problem: only the default
                // is worth spelling out.
                ShortcutState::Default => pass(&format!("shortcut: {binding} runs '{command}'")),
                ShortcutState::Custom => pass(&format!(
                    "shortcut: {binding} runs '{command}' (custom, default is {})",
                    install::shortcut_binding()
                )),
            }
            // GNOME never reports a collision, one action just stops working.
            for conflict in install::binding_conflicts(&binding) {
                warnings += 1;
                warn(&format!("{binding} is also bound to {conflict}"));
                hint("clipnest setup-shortcut --binding '<Super><Shift>v'");
            }
        }
    }

    // The systemd user service, which may come from a user install or from a
    // package. A unit a package added is only visible to the running user
    // manager after a reload, so that is checked rather than assumed.
    match install::installed_unit() {
        None => {
            warnings += 1;
            warn("the clipnest.service unit is not installed anywhere");
            hint("clipnest setup");
        }
        Some(unit) => {
            if !install::have("systemctl") {
                warnings += 1;
                warn("systemctl is not on $PATH; the daemon has to be started by hand");
                hint("clipnest daemon");
            } else {
                let fragment = install::command_output(
                    "systemctl",
                    &["--user", "show", "-p", "FragmentPath", "clipnest"],
                );
                let fragment = fragment
                    .trim_start_matches("FragmentPath=")
                    .trim()
                    .to_string();
                if fragment.is_empty() {
                    problems += 1;
                    fail(&format!(
                        "the user manager has not read {} yet",
                        unit.display()
                    ));
                    hint("systemctl --user daemon-reload   (clipnest setup does this)");
                } else {
                    let state =
                        install::command_output("systemctl", &["--user", "is-active", "clipnest"]);
                    if state == "active" {
                        pass(&format!("service: clipnest.service is active ({fragment})"));
                    } else {
                        // The unit is D-Bus activated and has no [Install]
                        // section, so waiting for the first copy is normal.
                        warnings += 1;
                        warn(&format!(
                            "service: clipnest.service is {state} (it starts on demand, at the first copy)"
                        ));
                        hint("systemctl --user start clipnest");
                    }
                }
            }
        }
    }

    // The unit has no [Install] section, so this file is what brings the daemon
    // up: without it nothing would ever start it.
    match install::installed_dbus_service() {
        Some(path) => pass(&format!(
            "service: the session bus starts it on demand ({})",
            path.display()
        )),
        None => {
            problems += 1;
            fail("there is no D-Bus activation file, so nothing ever starts the daemon");
            hint("clipnest setup");
        }
    }

    // A user-level binary comes first in $PATH on most desktops, so a leftover
    // `make install` copy keeps running while the package looks perfectly fine.
    if let Some(shadowing) = install::shadowing_binary() {
        warnings += 1;
        warn(&format!(
            "{} shadows the binary that is running",
            shadowing.display()
        ));
        hint("make uninstall   (to drop the user install and keep the package)");
    }

    // A typo in the settings file silently changes how much history is kept, so
    // anything that could not be understood is part of the report.
    let loaded = config::Config::load();
    if loaded.notes.is_empty() {
        pass(&format!(
            "settings: {} ({})",
            summarise_config(&loaded.config),
            if loaded.found {
                loaded.path.display().to_string()
            } else {
                "defaults, no file yet".to_string()
            }
        ));
    } else {
        warnings += 1;
        warn(&format!(
            "settings: {} line(s) in {} could not be used",
            loaded.notes.len(),
            loaded.path.display()
        ));
        for note in &loaded.notes {
            println!("       - {note}");
        }
        hint("clipnest config");
    }

    // Auto-paste: the only part that needs a permission the user grants by hand,
    // and the only one that cannot be checked by looking at a file.
    if loaded.config.paste_key.is_none() {
        pass("auto-paste: switched off (paste_key = none), picking an entry copies it");
    } else {
        match paste_state() {
            Ok((state, reason, note)) => {
                let mut complain = |note: &str, reason: paste::Reason| {
                    warnings += 1;
                    warn(&format!("auto-paste: {note}"));
                    // The reason comes from the daemon, which learned it while
                    // the failure happened - not from asking the shell whether
                    // the screen is locked right now, which answers a different
                    // question as soon as somebody unlocks it.
                    let advice = reason.advice();
                    if !advice.is_empty() {
                        hint(advice);
                    } else {
                        hint("clipnest paste-access");
                    }
                };
                match state {
                    paste::State::Ready => pass(&format!("auto-paste: ready ({note})")),
                    paste::State::Asking => {
                        complain("the portal is being asked right now", reason)
                    }
                    paste::State::Failed => complain(&note, reason),
                    paste::State::Idle => complain(&note, reason),
                }
            }
            Err(err) => {
                warnings += 1;
                warn(&format!("auto-paste: cannot ask the daemon ({err})"));
            }
        }
    }

    // The token the portal's grant is kept in. It is a permission to type into
    // this session, so a copy that other accounts can read is worth a warning of
    // its own - the file is small, and "who else can read my home directory" is
    // not a question most people can answer from memory.
    match paste::token_state() {
        paste::TokenState::Private => pass(&format!(
            "paste permission: remembered in {} (mode 0600)",
            paste::token_path().display()
        )),
        paste::TokenState::Missing => pass(
            "paste permission: not granted yet (clipnest paste-access asks for it)",
        ),
        paste::TokenState::Exposed(mode) => {
            warnings += 1;
            warn(&format!(
                "paste permission: {} is mode {mode:o}, readable by other users",
                paste::token_path().display()
            ));
            // Rewriting the file is the repair, and it is a write we can do
            // without asking the portal anything.
            match paste::tighten_token() {
                Ok(paste::TokenState::Private) => pass("and it was tightened to mode 0600"),
                Ok(_) => hint("clipnest paste-reset, then clipnest paste-access"),
                Err(err) => hint(&format!("fix it by hand: {err}")),
            }
        }
        paste::TokenState::Unreadable(why) => {
            warnings += 1;
            warn(&format!(
                "paste permission: {} {why}",
                paste::token_path().display()
            ));
            hint("clipnest paste-reset, then clipnest paste-access");
        }
    }

    if !install::have("gnome-extensions") {
        warnings += 1;
        warn("gnome-extensions is not on $PATH (install the gnome-shell package)");
    }

    // The session, and what it means. This is the line a user reads when the
    // history stays empty, so it names the reason and the next step rather than
    // printing `$XDG_SESSION_TYPE` and leaving the conclusion to them.
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let shell = install::have("gnome-shell");
    let shell_version = gnome_shell_version().unwrap_or_else(|| "GNOME Shell".to_string());
    match classify_session(&desktop, &session_type(), extension_running, shell) {
        SessionState::Supported => pass(&format!(
            "session: {shell_version} on Wayland (desktop: {desktop})"
        )),
        SessionState::ExtensionsOff => {
            problems += 1;
            fail("session: GNOME on Wayland, but the extension is not feeding the daemon");
            hint("nothing will be captured until it runs: clipnest install-extension");
        }
        SessionState::ForeignDesktop => {
            problems += 1;
            fail(&format!(
                "session: '{desktop}' on {}; the extension needs gnome-shell to capture anything",
                session_type()
            ));
            hint("ClipNest is built for GNOME; see the README for what works elsewhere");
        }
        SessionState::X11 => {
            warnings += 1;
            warn(&format!("session: {shell_version} on X11 (desktop: {desktop})"));
            // Said plainly because it is easy to promise more than is tested: the
            // extension still does the capturing, and the portal still needs a
            // RemoteDesktop backend for auto-paste.
            hint("X11 is not the tested path: capture still goes through the extension");
        }
        SessionState::Unknown => {
            warnings += 1;
            warn(&format!(
                "session: cannot tell (XDG_SESSION_TYPE={}, desktop='{desktop}')",
                session_type()
            ));
        }
    }

    println!();
    if problems == 0 {
        println!("everything looks fine ({warnings} warning(s))");
        ExitCode::SUCCESS
    } else {
        println!("{problems} problem(s), {warnings} warning(s) - see the hints above");
        ExitCode::FAILURE
    }
}

/// What the session a user is in means for ClipNest.
#[derive(Debug, PartialEq, Eq)]
enum SessionState {
    /// GNOME on Wayland with the extension running: the supported combination.
    Supported,
    /// GNOME on Wayland, but nothing is capturing: an empty history, and no
    /// obvious reason for it.
    ExtensionsOff,
    /// A desktop whose shell has no `St.Clipboard` extension to hand: on Wayland
    /// nothing can be read at all.
    ForeignDesktop,
    /// GNOME on X11: works through the same extension, but not the tested path.
    X11,
    /// No environment to judge from (a `sudo` shell, a container, ssh).
    Unknown,
}

/// Judges the session from what `doctor` has already looked up, so the rule can
/// be tested without logging into another desktop.
///
/// The desktop *name* is deliberately not what decides this. On Ubuntu 26.04,
/// `XDG_CURRENT_DESKTOP` is `Unity` while GNOME Shell 50.1 is what is running -
/// a classifier that trusted the name would tell exactly the users this is built
/// for that their desktop is unsupported. `shell_installed` is the fact that
/// matters: without gnome-shell there is no extension, and on Wayland nothing
/// reads the clipboard.
fn classify_session(
    desktop: &str,
    session: &str,
    extension_running: bool,
    shell_installed: bool,
) -> SessionState {
    match session.to_ascii_lowercase().as_str() {
        "x11" if shell_installed => SessionState::X11,
        "x11" => SessionState::ForeignDesktop,
        "wayland" => match (shell_installed, extension_running) {
            (false, _) => SessionState::ForeignDesktop,
            (true, true) => SessionState::Supported,
            (true, false) => SessionState::ExtensionsOff,
        },
        // No session type at all: a container, a `sudo` shell, a serial console.
        // A named desktop without gnome-shell is still worth saying out loud.
        _ if !shell_installed && !desktop.is_empty() => SessionState::ForeignDesktop,
        _ => SessionState::Unknown,
    }
}

/// What `doctor` can say about the registered shortcut.
#[derive(Debug, PartialEq, Eq)]
enum ShortcutState {
    /// Not registered, not running this binary, or carrying no usable binding.
    Broken,
    /// Registered and working, on the default binding.
    Default,
    /// Registered and working, on a binding the user chose.
    Custom,
}

/// Judges the shortcut from values already read out of GNOME, so the rule can be
/// tested without a settings daemon.
fn classify_shortcut(registered: bool, command_ok: bool, binding: &str) -> ShortcutState {
    if !registered || !command_ok || !install::binding_is_valid(binding) {
        return ShortcutState::Broken;
    }
    if install::binding_is_default(binding) {
        ShortcutState::Default
    } else {
        ShortcutState::Custom
    }
}

/// The daemon is started on demand, and GApplication takes the bus name before
/// it exports its own interface, so the first call of a session can land in that
/// gap. Retrying for a moment keeps `Super+Shift+V` right after login from
/// looking like a broken daemon.
fn call_when_ready(method: &str, params: Option<&glib::Variant>) -> Result<glib::Variant, String> {
    let attempts = 25;
    let mut last = String::new();
    for attempt in 0..attempts {
        match bus_call(method, params) {
            Ok(reply) => return Ok(reply),
            // Anything that is not "still starting up" is the real answer.
            Err(err) if is_startup_race(&err) => last = err,
            Err(err) => return Err(err),
        }
        if attempt + 1 < attempts {
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
    }
    Err(last)
}

/// The errors that only mean "the on-demand start is still in flight".
fn is_startup_race(message: &str) -> bool {
    message.contains("No such interface")
        || message.contains("not provided by any .service files")
        || message.contains("StartServiceByName")
}

fn session_type() -> String {
    std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".to_string())
}

/// `GNOME Shell 50.1`, when the shell can be asked. Which version is running is
/// the second question after "is it running": the extension declares the versions
/// it supports, and a mismatch is the difference between a bug report and a
/// supported installation.
fn gnome_shell_version() -> Option<String> {
    let output = install::command_output("gnome-shell", &["--version"]);
    let line = output.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// What gnome-shell currently has loaded for this extension: `(state, version)`.
fn gnome_extension_info() -> (String, Option<u32>) {
    let output = install::command_output("gnome-extensions", &["info", install::extension_uuid()]);
    let field = |name: &str| {
        output
            .lines()
            .find_map(|line| line.trim().strip_prefix(name))
            .map(|value| value.trim().to_string())
    };
    let state = field("State:").unwrap_or_else(|| "unknown".to_string());
    let version = field("Version:").and_then(|value| value.parse().ok());
    (state, version)
}

/// A one-line summary of the settings, for the doctor report.
fn summarise_config(config: &config::Config) -> String {
    format!(
        "{} entries, {} image budget, poll {} ms",
        config.max_items,
        app::human_bytes(config.image_budget_bytes),
        config.poll_interval_ms
    )
}

/// The extension version compiled into this binary.
///
/// Found by name: the order of the files bundled inside the binary is not
/// something a caller should have to know about.
fn bundled_extension_version() -> Option<u32> {
    let (_, metadata) = install::extension_files()
        .into_iter()
        .find(|(name, _)| *name == "metadata.json")?;
    let after_key = metadata.split("\"version\"").nth(1)?;
    let after_colon = after_key.split(':').nth(1)?;
    after_colon
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

fn pass(message: &str) {
    println!("  \u{2705} {message}");
}

fn warn(message: &str) {
    println!("  \u{26a0}\u{fe0f}  {message}");
}

fn fail(message: &str) {
    println!("  \u{274c} {message}");
}

fn hint(command: &str) {
    println!("       \u{2192} {command}");
}

fn activation_token() -> String {
    // GNOME hands the activation token of the app that spawned us, which is what
    // lets the compositor give our panel keyboard focus instead of showing a
    // "window is ready" notification.
    std::env::var("XDG_ACTIVATION_TOKEN")
        .or_else(|_| std::env::var("DESKTOP_STARTUP_ID"))
        .unwrap_or_default()
}

/// Everything an option may be given to, and everything it may not.
///
/// `FLAGS` is the list a bare `--force` is checked against, so an option that
/// works but is missing here would print a warning that lies; `WITH_VALUE` is
/// what decides whether the next token is consumed as a value.
const WITH_VALUE: [&str; 4] = ["--limit", "--out", "--search", "--binding"];
const FLAGS: [&str; 3] = ["--json", "--raw", "--force"];

/// Tiny option parser: `--flag`, `--key value`, `--key=value`, and `--` to stop
/// reading options.
#[derive(Default)]
struct Args {
    positional: Vec<String>,
    values: Vec<(String, String)>,
    flags: Vec<String>,
    /// Things worth stopping for: an option this build does not know, or one
    /// that was left without the value it needs. A typo like `--limt 5` used to
    /// print a warning and then list the whole history anyway, which is the
    /// quiet sort of wrong.
    problems: Vec<String>,
}

impl Args {
    fn parse(tokens: Vec<String>) -> Self {
        let mut args = Args::default();
        let mut tokens = tokens.into_iter().peekable();
        let mut only_positional = false;
        while let Some(token) = tokens.next() {
            if only_positional {
                args.positional.push(token);
                continue;
            }
            if token == "--" {
                only_positional = true;
                continue;
            }
            if let Some(rest) = token.strip_prefix("--") {
                let (name, inline) = match rest.split_once('=') {
                    Some((name, value)) => (format!("--{name}"), Some(value.to_string())),
                    None => (token.clone(), None),
                };
                let inline = inline.filter(|value| !value.is_empty());
                if WITH_VALUE.contains(&name.as_str()) {
                    // A following token that is itself an option is not a value:
                    // `--limit --json` used to read `--json` as the number.
                    let consumed = match tokens.peek() {
                        None => None,
                        Some(next) if Self::looks_like_option(next) => None,
                        Some(next) => Some(next.clone()),
                    };
                    let from_next = inline.is_none();
                    match inline.or(consumed) {
                        Some(value) => {
                            if from_next {
                                tokens.next();
                            }
                            args.values.push((name, value));
                        }
                        None => args.problems.push(format!("{name} needs a value")),
                    }
                } else if FLAGS.contains(&name.as_str()) {
                    if let Some(value) = inline {
                        args.problems
                            .push(format!("{name} does not take a value (got '{value}')"));
                    }
                    args.flags.push(name);
                } else {
                    args.problems
                        .push(format!("{name} is not an option this build knows"));
                    args.flags.push(name);
                }
            } else if token.starts_with('-') && token.len() > 1 {
                args.problems
                    .push(format!("{token} is not an option this build knows"));
                args.flags.push(token);
            } else {
                args.positional.push(token);
            }
        }
        args
    }

    /// True for `--` and for anything spelled like a long option. A single-dash
    /// value (`--search -foo`) is still a value, so a search for a dash-leading
    /// string keeps working.
    fn looks_like_option(token: &str) -> bool {
        token == "--" || token.starts_with("--")
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    /// The first positional argument, parsed as an entry id or index.
    fn id(&self) -> Option<u32> {
        self.positional.first().and_then(|value| value.parse().ok())
    }

    /// `--limit` if it is a usable number.
    ///
    /// A missing option is the default; a bad one is an error rather than a
    /// silent fallback, because printing the whole history when five entries were
    /// asked for is worse than a complaint.
    fn limit(&self, default: i64) -> Result<i64, String> {
        match self.value("--limit") {
            None => Ok(default),
            Some(raw) => match raw.trim().parse::<i64>() {
                Ok(limit) if limit > 0 => Ok(limit),
                Ok(_) => Err(format!("--limit must be at least 1, got '{raw}'")),
                Err(_) => Err(format!("--limit wants a number, got '{raw}'")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(tokens: &[&str]) -> Args {
        Args::parse(tokens.iter().map(|token| token.to_string()).collect())
    }

    #[test]
    fn reads_options_in_both_spellings() {
        let args = parse(&["--limit", "5", "--json", "--out=shot.png", "12"]);
        assert_eq!(args.limit(50), Ok(5));
        assert!(args.has("--json"));
        assert_eq!(args.value("--out"), Some("shot.png"));
        assert_eq!(args.id(), Some(12));
        assert!(args.problems.is_empty());
    }

    #[test]
    fn knows_every_option_the_help_text_offers() {
        // The bug this pins down: `--force` worked but was missing from the list
        // of known flags, so every `clipnest export dir --force` printed a
        // warning about an option it had just honoured.
        for option in ["--json", "--raw", "--force"] {
            let args = parse(&[option]);
            assert!(args.has(option), "{option} did not register");
            assert!(args.problems.is_empty(), "{option} was called unknown");
        }
        for option in ["--limit", "--out", "--search", "--binding"] {
            let args = parse(&[option, "x"]);
            assert_eq!(args.value(option), Some("x"), "{option} did not register");
            assert!(args.problems.is_empty(), "{option} was called unknown");
        }
    }

    #[test]
    fn stops_on_an_option_it_does_not_know() {
        // A typo has to be visible: silently listing the whole history instead
        // of the five entries the user asked for is worse than a complaint.
        let args = parse(&["--limt", "5", "--search", "needle", "-x"]);
        assert_eq!(
            args.problems,
            vec![
                "--limt is not an option this build knows".to_string(),
                "-x is not an option this build knows".to_string(),
            ]
        );
        assert_eq!(args.value("--search"), Some("needle"));
    }

    #[test]
    fn stops_on_an_option_left_without_its_value() {
        assert_eq!(
            parse(&["--limit"]).problems,
            vec!["--limit needs a value".to_string()]
        );
        assert_eq!(
            parse(&["--out", "--json"]).problems,
            vec!["--out needs a value".to_string()]
        );
        assert_eq!(
            parse(&["--search="]).problems,
            vec!["--search needs a value".to_string()]
        );
        // A value that happens to start with a single dash is still a value.
        let args = parse(&["--search", "-foo"]);
        assert_eq!(args.value("--search"), Some("-foo"));
        assert!(args.problems.is_empty());
    }

    #[test]
    fn a_flag_refuses_a_value() {
        assert_eq!(
            parse(&["--json=1"]).problems,
            vec!["--json does not take a value (got '1')".to_string()]
        );
    }

    #[test]
    fn two_dashes_end_the_options() {
        // Without this, searching for a string that starts with a dash would be
        // impossible to write down.
        let args = parse(&["search", "--", "--limit", "5"]);
        assert_eq!(args.positional, vec!["search", "--limit", "5"]);
        assert!(args.problems.is_empty());
    }

    #[test]
    fn refuses_a_limit_that_makes_no_sense() {
        // `--limit 0` used to print nothing at all, without saying why.
        assert_eq!(parse(&["--limit", "0"]).limit(50).unwrap_err(), "--limit must be at least 1, got '0'");
        assert!(parse(&["--limit", "lots"]).limit(50).is_err());
        assert_eq!(parse(&[]).limit(50), Ok(50));
        // Export asks for everything by default, which is a negative limit.
        assert_eq!(parse(&[]).limit(-1), Ok(-1));
    }

    #[test]
    fn a_search_keeps_the_whole_phrase() {
        // `clipnest search hello world` used to drop the second word.
        let args = parse(&["hello", "world", "--limit", "3"]);
        assert_eq!(args.positional.join(" "), "hello world");
    }

    /// The rows `clipnest list --json` prints, as a machine reads them. The
    /// escaping used to be checked by comparing strings, which cannot tell
    /// "valid JSON" from "looks a bit like JSON".
    #[test]
    fn the_json_output_is_json() {
        let rows = vec![
            row(1, "text", "plain"),
            row(2, "text", "a \"quote\" and a \\ backslash"),
            row(3, "text", "two\nlines\tand a tab"),
            row(4, "text", "carriage\rreturn"),
            row(5, "text", "bell\u{7} and a nul\u{0} byte"),
            row(6, "text", "سلام دنیا \u{1f680} \u{2028} line separator"),
            row(7, "image", "\u{feff}\u{200d}\u{fffd}"),
            row(8, "text", &"x".repeat(50_000)),
        ];
        let text = json_array(&rows);
        let parsed: serde_json::Value =
            serde_json::from_str(&text).expect("the CLI printed something that is not JSON");
        let array = parsed.as_array().expect("a JSON array");
        assert_eq!(array.len(), rows.len());
        for (index, value) in array.iter().enumerate() {
            assert_eq!(value["id"], serde_json::json!(rows[index].id));
            assert_eq!(
                value["preview"].as_str(),
                Some(rows[index].preview.as_str()),
                "row {index} did not survive the round trip"
            );
            assert_eq!(value["pinned"], serde_json::json!(rows[index].pinned));
        }
        // An empty history is an empty array, not an empty string.
        assert_eq!(json_array(&[]), "[]");
    }

    fn row(id: u32, kind: &str, preview: &str) -> Row {
        Row {
            id,
            kind: kind.to_string(),
            preview: preview.to_string(),
            width: 0,
            height: 0,
            pinned: id.is_multiple_of(2),
        }
    }

    #[test]
    fn judges_the_session_it_is_running_in() {
        // The supported combination, and the one that looks identical until you
        // notice that nothing is being captured.
        assert_eq!(
            classify_session("ubuntu:GNOME", "wayland", true, true),
            SessionState::Supported
        );
        // The one that matters on this project's own target machine: Ubuntu
        // 26.04 calls its GNOME session "Unity".
        assert_eq!(
            classify_session("Unity", "wayland", true, true),
            SessionState::Supported
        );
        assert_eq!(
            classify_session("Unity", "wayland", false, true),
            SessionState::ExtensionsOff
        );
        // gnome-shell installed but a session that is not Wayland is not the
        // tested path, whatever the desktop calls itself.
        assert_eq!(
            classify_session("GNOME", "x11", true, true),
            SessionState::X11
        );
        assert_eq!(
            classify_session("KDE", "x11", false, false),
            SessionState::ForeignDesktop
        );
        // Wayland without gnome-shell: nothing can read the clipboard at all.
        assert_eq!(
            classify_session("sway", "wayland", false, false),
            SessionState::ForeignDesktop
        );
        // No desktop and no shell: a shell that exports nothing is not a desktop
        // we can judge (ssh, a container, a `sudo` shell).
        assert_eq!(
            classify_session("", "tty", false, false),
            SessionState::Unknown
        );
        assert_eq!(
            classify_session("", "", true, true),
            SessionState::Unknown
        );
    }

    #[test]
    fn judges_the_registered_shortcut() {
        assert_eq!(
            classify_shortcut(false, true, "<Super><Shift>v"),
            ShortcutState::Broken
        );
        assert_eq!(
            classify_shortcut(true, false, "<Super><Shift>v"),
            ShortcutState::Broken
        );
        assert_eq!(classify_shortcut(true, true, ""), ShortcutState::Broken);
        assert_eq!(
            classify_shortcut(true, true, "a path, not an accelerator"),
            ShortcutState::Broken
        );
        assert_eq!(
            classify_shortcut(true, true, "<Super><Shift>v"),
            ShortcutState::Default
        );
        // Picking another binding keeps it working; doctor must not cry wolf.
        assert_eq!(
            classify_shortcut(true, true, "<Super>v"),
            ShortcutState::Custom
        );
    }
}
