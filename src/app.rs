use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use glib::variant::ToVariant;
use gtk::{gdk, glib};

use crate::db::{now_ms, Content, Item, Store};
use crate::paste::{self, Accelerator};

/// The name the CLI and the shell extension call the daemon on.
pub const BUS_NAME: &str = "dev.clipnest.Daemon";
pub const OBJECT_PATH: &str = "/dev/clipnest/Daemon";
pub const INTERFACE: &str = "dev.clipnest.Daemon";
/// GApplication's own identity, deliberately *not* `BUS_NAME`: it is taken while
/// the application registers, which would publish the API name a moment before
/// the interface behind it exists. See `register_dbus`.
const APP_ID: &str = "dev.clipnest.Panel";
/// `org.freedesktop.DBus.RequestName`: do not queue behind an existing owner.
const NAME_FLAG_DO_NOT_QUEUE: u32 = 4;
/// RequestName replies meaning "you own it now" / "you already did".
const NAME_REPLY_PRIMARY_OWNER: u32 = 1;
const NAME_REPLY_ALREADY_OWNER: u32 = 4;

/// How much memory the decoded thumbnails may take together.
///
/// Counting them was the wrong measure: a decoded 4K screenshot is roughly
/// 48 MiB, so allowing forty of them - which a history full of screenshots
/// reaches quickly - is nearly two gigabytes of RAM. The budget is in bytes, and
/// the decode that was touched longest ago goes first.
const TEXTURE_BUDGET_BYTES: i64 = 96 * 1024 * 1024;
/// Rows this far outside the viewport keep their thumbnail.
const VIEWPORT_MARGIN: f64 = 240.0;

/// The clipboard itself lives inside gnome-shell on Wayland (Mutter implements
/// no data-control protocol, so a plain background process cannot read it), so
/// the shell extension pushes changes to us through this interface.
const IFACE_XML: &str = r#"<node>
  <interface name="dev.clipnest.Daemon">
    <method name="PushText">
      <arg type="s" name="text" direction="in"/>
    </method>
    <method name="PushImage">
      <arg type="s" name="mime" direction="in"/>
      <arg type="ay" name="data" direction="in"/>
    </method>
    <method name="Toggle">
      <arg type="s" name="token" direction="in"/>
    </method>
    <method name="List">
      <arg type="u" name="limit" direction="in"/>
      <arg type="a(ussuub)" name="items" direction="out"/>
    </method>
    <method name="Search">
      <arg type="s" name="needle" direction="in"/>
      <arg type="u" name="limit" direction="in"/>
      <arg type="a(ussuub)" name="items" direction="out"/>
    </method>
    <method name="GetItem">
      <arg type="u" name="id" direction="in"/>
      <arg type="s" name="kind" direction="out"/>
      <arg type="s" name="text" direction="out"/>
      <arg type="u" name="width" direction="out"/>
      <arg type="u" name="height" direction="out"/>
      <arg type="s" name="mime" direction="out"/>
      <arg type="s" name="data" direction="out"/>
    </method>
    <method name="Pick">
      <arg type="u" name="id" direction="in"/>
    </method>
    <method name="Paste">
      <arg type="u" name="id" direction="in"/>
      <arg type="s" name="status" direction="out"/>
    </method>
    <method name="PreparePaste">
      <arg type="s" name="note" direction="out"/>
    </method>
    <method name="PasteStatus">
      <arg type="s" name="state" direction="out"/>
      <arg type="s" name="reason" direction="out"/>
      <arg type="s" name="note" direction="out"/>
    </method>
    <method name="SetPin">
      <arg type="u" name="id" direction="in"/>
      <arg type="b" name="pinned" direction="in"/>
    </method>
    <method name="Delete">
      <arg type="u" name="id" direction="in"/>
      <arg type="t" name="freed" direction="out"/>
    </method>
    <method name="Clear">
      <arg type="u" name="removed" direction="out"/>
      <arg type="t" name="freed" direction="out"/>
    </method>
    <method name="Vacuum">
      <arg type="t" name="freed" direction="out"/>
    </method>
    <method name="Config">
      <arg type="a(ss)" name="settings" direction="out"/>
    </method>
    <method name="Refresh"/>
    <method name="DiskUsage">
      <arg type="t" name="database" direction="out"/>
      <arg type="t" name="reclaimable" direction="out"/>
    </method>
    <method name="Status">
      <arg type="s" name="summary" direction="out"/>
      <arg type="s" name="version" direction="out"/>
      <arg type="u" name="total" direction="out"/>
      <arg type="u" name="images" direction="out"/>
      <arg type="u" name="pinned" direction="out"/>
      <arg type="t" name="db_bytes" direction="out"/>
    </method>
  </interface>
</node>"#;

const CSS: &str = "
window.clipnest-window { background: transparent; }
.clipnest-panel {
  background-color: @window_bg_color;
  border-radius: 14px;
  border: 1px solid alpha(currentColor, 0.15);
}
.clipnest-search { margin: 10px 10px 6px 10px; }
.clipnest-list { background: transparent; }
.clipnest-list > row { border-radius: 9px; margin: 0 6px 2px 6px; }
.clipnest-list > row:selected { background-color: alpha(@accent_bg_color, 0.85); }
.clipnest-thumb {
  border-radius: 8px;
  background-color: alpha(currentColor, 0.06);
  min-width: 88px;
  min-height: 56px;
}
.clipnest-meta { opacity: 0.75; }
.clipnest-time { opacity: 0.45; font-size: 0.8em; }
.clipnest-pin { opacity: 0.3; min-width: 28px; min-height: 28px; padding: 0; }
.clipnest-pin.pinned { opacity: 1; color: @accent_color; }
.clipnest-empty { opacity: 0.5; padding: 28px; }
.clipnest-hint { opacity: 0.5; font-size: 0.85em; margin: 6px 12px 10px 12px; }
";

struct Ui {
    store: Rc<Store>,
    window: adw::ApplicationWindow,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    scroller: gtk::ScrolledWindow,
    popover: gtk::PopoverMenu,
    items: RefCell<Vec<Item>>,
    /// One entry per row in `items`, for images only.
    pictures: RefCell<Vec<Option<gtk::Picture>>>,
    /// Decoded thumbnails, keyed by item id and bounded by
    /// `TEXTURE_BUDGET_BYTES` rather than by a row count.
    textures: RefCell<Budgeted<gdk::Texture>>,
    /// Row the context menu was opened on.
    menu_target: RefCell<Option<i64>>,
    /// The line under the list, which changes when nothing can be pasted yet.
    hint: gtk::Label,
    /// Set when the history changed while the panel was hidden. The list is then
    /// rebuilt on the next `show` instead of on every single copy.
    dirty: Cell<bool>,
}

/// A small map with a memory budget, oldest entry dropped first.
///
/// Generic over the payload so the eviction policy can be tested without
/// decoding any image.
#[derive(Debug)]
struct Budgeted<T> {
    entries: HashMap<i64, (T, i64)>,
    order: VecDeque<i64>,
    bytes: i64,
    limit: i64,
}

impl<T> Budgeted<T> {
    fn new(limit: i64) -> Self {
        Budgeted {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    /// Looks an entry up and marks it as the most recently used, so scrolling
    /// back and forth over one screenful does not decode anything twice.
    fn get(&mut self, id: i64) -> Option<&T> {
        if !self.entries.contains_key(&id) {
            return None;
        }
        if let Some(position) = self.order.iter().position(|key| *key == id) {
            if position + 1 != self.order.len() {
                self.order.remove(position);
                self.order.push_back(id);
            }
        }
        self.entries.get(&id).map(|(value, _)| value)
    }

    /// Stores an entry, dropping the oldest ones until it fits. Something bigger
    /// than the whole budget is simply not cached - and a previous, smaller
    /// version of it is dropped rather than left behind counting against it.
    fn insert(&mut self, id: i64, value: T, size: i64) {
        self.forget(id);
        if size > self.limit {
            return;
        }
        while self.bytes + size > self.limit {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some((_, dropped)) = self.entries.remove(&oldest) {
                self.bytes -= dropped;
            }
        }
        self.entries.insert(id, (value, size));
        self.order.push_back(id);
        self.bytes += size;
    }

    fn forget(&mut self, id: i64) {
        if let Some((_, size)) = self.entries.remove(&id) {
            self.bytes -= size;
            if let Some(position) = self.order.iter().position(|key| *key == id) {
                self.order.remove(position);
            }
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }
}

/// Roughly what a decoded texture costs: four bytes per pixel.
fn decoded_bytes(texture: &gdk::Texture) -> i64 {
    texture.width() as i64 * texture.height() as i64 * 4
}

/// The daemon without its panel. A clipboard change has to be recorded even
/// when nobody has opened the panel yet, so the store lives here and the widgets
/// are built later, the first time they are really needed.
struct Daemon {
    app: adw::Application,
    store: Rc<Store>,
    panel: RefCell<Option<Rc<Ui>>>,
}

impl Daemon {
    /// The panel, built on first use. `None` only outside a graphical session,
    /// where there is no display to draw on.
    fn panel(&self) -> Option<Rc<Ui>> {
        if let Some(panel) = self.panel.borrow().as_ref() {
            return Some(panel.clone());
        }
        let panel = build_panel(self)?;
        *self.panel.borrow_mut() = Some(panel.clone());
        Some(panel)
    }

    /// The panel if it already exists. Never builds one: asking for the status
    /// must not drag a window into a headless process.
    fn existing_panel(&self) -> Option<Rc<Ui>> {
        self.panel.borrow().as_ref().cloned()
    }

    /// Marks a panel somebody is looking at as out of date.
    ///
    /// A panel that was never opened has nothing to update, and a hidden one has
    /// no reason to be rebuilt yet: copying something while the panel is closed
    /// happens all day long, and there is no point in rebuilding up to 300 rows
    /// every time. The list is rebuilt when the panel is next shown.
    fn refresh_panel(&self) {
        if let Some(panel) = self.existing_panel() {
            if panel.window.is_visible() {
                refresh(&panel);
            } else {
                panel.dirty.set(true);
            }
        }
    }

    /// Rebuilds whatever panel exists, visible or not. Used by the `Refresh` call
    /// the CLI makes after writing to the history behind the daemon's back.
    fn refresh_panel_now(&self) {
        if let Some(panel) = self.existing_panel() {
            refresh(&panel);
            panel.dirty.set(false);
        }
    }
}

pub fn run() {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .build();
    app.connect_startup(|app| {
        // systemd owns this process, so the daemon must not disappear just
        // because no panel was ever built. `hold` returns a guard that releases
        // the hold when it is dropped, and this hold is meant to last for the
        // whole process lifetime: leak that one guard here, on purpose.
        std::mem::forget(app.hold());
        setup_startup(app);
    });
    app.connect_activate(|_| {
        // Never triggered in normal use: the panel is opened through the
        // Toggle method. Keeps a bare `clipnest daemon` from showing an empty
        // window.
    });
    // Ignore our own CLI arguments so GTK does not try to open them as files.
    app.run_with_args::<&str>(&[]);
}

fn setup_startup(app: &adw::Application) {
    let store = match Store::open_default() {
        Ok(store) => store,
        Err(err) => {
            eprintln!("clipnest: cannot open the history database: {err}");
            std::process::exit(1);
        }
    };

    let daemon = Rc::new(Daemon {
        app: app.clone(),
        store: Rc::new(store),
        panel: RefCell::new(None),
    });
    register_dbus(&daemon);
}

/// Builds the panel: the only part of the daemon that needs a display.
fn build_panel(daemon: &Daemon) -> Option<Rc<Ui>> {
    let display = match gdk::Display::default() {
        Some(display) => display,
        None => {
            eprintln!("clipnest: no graphical display available");
            return None;
        }
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search clipboard history"));
    search.add_css_class("clipnest-search");

    let list = gtk::ListBox::new();
    list.add_css_class("clipnest-list");
    list.set_selection_mode(gtk::SelectionMode::Single);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list)
        .build();

    let hint = gtk::Label::new(None);
    hint.add_css_class("clipnest-hint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    hint.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    hint.set_lines(2);
    set_hint(&hint, &daemon.store);

    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("clipnest-panel");
    panel.append(&search);
    panel.append(&scroller);
    panel.append(&hint);

    let window = adw::ApplicationWindow::builder()
        .application(&daemon.app)
        .decorated(false)
        .resizable(false)
        .default_width(480)
        .default_height(480)
        .content(&panel)
        .build();
    window.add_css_class("clipnest-window");
    window.set_hide_on_close(true);

    let menu = gio::Menu::new();
    menu.append(Some("Copy"), Some("win.copy"));
    menu.append(Some("Pin / unpin"), Some("win.pin"));
    menu.append(Some("Delete"), Some("win.delete"));
    let popover = gtk::PopoverMenu::from_model(Some(&menu));

    let ui = Rc::new(Ui {
        store: daemon.store.clone(),
        window: window.clone(),
        search: search.clone(),
        list: list.clone(),
        scroller: scroller.clone(),
        popover,
        items: RefCell::new(Vec::new()),
        pictures: RefCell::new(Vec::new()),
        textures: RefCell::new(Budgeted::new(TEXTURE_BUDGET_BYTES)),
        menu_target: RefCell::new(None),
        hint,
        dirty: Cell::new(false),
    });

    {
        let ui = ui.clone();
        search.connect_search_changed(move |entry| {
            let query = entry.text().to_string();
            rebuild(&ui, &query);
        });
    }

    {
        let ui = ui.clone();
        list.connect_row_activated(move |_, row| activate_row(&ui, row.index()));
    }

    // Clicking away dismisses the panel, like the Windows clipboard popup.
    window.connect_notify_local(Some("is-active"), |window, _| {
        if window.is_visible() && !window.is_active() {
            window.set_visible(false);
        }
    });

    {
        let ui = ui.clone();
        let keys = gtk::EventControllerKey::new();
        // Capture phase so the arrow keys work while the search entry has focus.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, state| on_key(&ui, key, state));
        window.add_controller(keys);
    }

    // Thumbnails are decoded only where they are actually on screen.
    {
        let adjustment = scroller.vadjustment();
        let weak = Rc::downgrade(&ui);
        adjustment.connect_value_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                paint_visible_images(&ui);
            }
        });
        let weak = Rc::downgrade(&ui);
        adjustment.connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                paint_visible_images(&ui);
            }
        });
    }

    install_actions(&window, &ui);
    rebuild(&ui, "");
    Some(ui)
}

/// Copy / pin / delete for the row the context menu was opened on.
type ActionHandler = fn(&Rc<Ui>, i64);

fn install_actions(window: &adw::ApplicationWindow, ui: &Rc<Ui>) {
    let actions: [(&str, ActionHandler); 3] = [
        ("copy", |ui, id| {
            copy_item(ui, id, true);
        }),
        ("pin", toggle_pin),
        ("delete", delete_item),
    ];

    for (name, handler) in actions {
        let action = gio::SimpleAction::new(name, None);
        let ui = ui.clone();
        action.connect_activate(move |_, _| {
            let target = *ui.menu_target.borrow();
            let Some(id) = target else { return };
            ui.popover.popdown();
            handler(&ui, id);
        });
        window.add_action(&action);
    }
}

fn register_dbus(daemon: &Rc<Daemon>) {
    let node = match gio::DBusNodeInfo::for_xml(IFACE_XML) {
        Ok(node) => node,
        Err(err) => {
            eprintln!("clipnest: bad D-Bus interface description: {err}");
            return;
        }
    };
    let Some(interface) = node.lookup_interface(INTERFACE) else {
        eprintln!("clipnest: {INTERFACE} is missing from the interface description");
        return;
    };

    let connection = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("clipnest: session bus unavailable: {err}");
            return;
        }
    };

    let daemon = daemon.clone();
    let registered = connection
        .register_object(OBJECT_PATH, &interface)
        .method_call(
            move |_conn, _sender, _path, _iface, method, params, invocation| {
                match method {
                    "PushText" => {
                        if let Some(text) = params.child_value(0).str() {
                            store_text(&daemon, text);
                        }
                        invocation.return_value(None);
                    }
                    "PushImage" => {
                        let mime = params.child_value(0).str().unwrap_or("").to_string();
                        let data = params.child_value(1).data_as_bytes().to_vec();
                        store_image(&daemon, &mime, data);
                        invocation.return_value(None);
                    }
                    "Toggle" => {
                        let token = params.child_value(0).str().unwrap_or("").to_string();
                        // The panel is built here, on first use: this is also the
                        // first moment the daemon needs a display at all.
                        match daemon.panel() {
                            // Only a panel that currently holds keyboard focus is
                            // closed again; otherwise the shortcut re-summons it.
                            Some(ui) if ui.window.is_visible() && ui.window.is_active() => {
                                ui.window.set_visible(false);
                            }
                            Some(ui) => show(&ui, &token),
                            None => eprintln!("clipnest: no display to open the panel on"),
                        }
                        invocation.return_value(None);
                    }
                    // `List` stays for a CLI from an older build; `Search` is
                    // the same query with a needle in front of it.
                    "List" => {
                        let limit = panel_limit(&params.child_value(0));
                        let items = daemon.store.list("", limit).unwrap_or_default();
                        invocation.return_value(Some(&(rows_of(&items),).to_variant()));
                    }
                    "Search" => {
                        let needle = params.child_value(0).str().unwrap_or("").to_string();
                        let limit = panel_limit(&params.child_value(1));
                        let items = daemon.store.list(&needle, limit).unwrap_or_default();
                        invocation.return_value(Some(&(rows_of(&items),).to_variant()));
                    }
                    "GetItem" => {
                        let id = params.child_value(0).get::<u32>().unwrap_or(0) as i64;
                        let reply =
                            match daemon.store.content(id).ok().flatten() {
                                Some(Content::Text(text)) => (
                                    "text".to_string(),
                                    text,
                                    0u32,
                                    0u32,
                                    String::new(),
                                    String::new(),
                                ),
                                Some(Content::Image {
                                    mime,
                                    data,
                                    width,
                                    height,
                                }) => (
                                    "image".to_string(),
                                    String::new(),
                                    width,
                                    height,
                                    mime,
                                    glib::base64_encode(&data).to_string(),
                                ),
                                None => (
                                    "missing".to_string(),
                                    String::new(),
                                    0u32,
                                    0u32,
                                    String::new(),
                                    String::new(),
                                ),
                            };
                        invocation.return_value(Some(&reply.to_variant()));
                    }
                    "Pick" => {
                        let id = params.child_value(0).get::<u32>().unwrap_or(0) as i64;
                        // `clipnest pick` must work without ever building a
                        // panel, so the copying itself stays window-free - and it
                        // stays a plain copy: a command that also typed into
                        // whatever is focused would be a nasty surprise.
                        match daemon.existing_panel() {
                            Some(ui) => {
                                copy_item(&ui, id, false);
                            }
                            None => {
                                copy_to_clipboard_item(&daemon.store, id);
                            }
                        }
                        invocation.return_value(None);
                    }
                    // `clipnest paste <n>`: copy an entry *and* put it into the
                    // window the user is working in. This is the same thing a
                    // click on a row does, which is why it is asked for by name.
                    "Paste" => {
                        let id = params.child_value(0).get::<u32>().unwrap_or(0) as i64;
                        let note = paste_entry(&daemon, id);
                        invocation.return_value(Some(&(note,).to_variant()));
                    }
                    // Asking for the permission is a separate step because it
                    // waits for a human: `clipnest paste-access` triggers this,
                    // then reports what happened.
                    "PreparePaste" => {
                        paste::request_access();
                        invocation.return_value(Some(&(paste::status().note,).to_variant()));
                    }
                    "PasteStatus" => {
                        let status = paste::status();
                        invocation.return_value(Some(
                            &(
                                status.state.as_str(),
                                status.reason.as_str(),
                                status.note,
                            )
                                .to_variant(),
                        ));
                    }
                    "SetPin" => {
                        let id = params.child_value(0).get::<u32>().unwrap_or(0) as i64;
                        let pinned = params.child_value(1).get::<bool>().unwrap_or(false);
                        daemon.store.set_pinned(id, pinned).ok();
                        daemon.refresh_panel();
                        invocation.return_value(None);
                    }
                    "Delete" => {
                        let id = params.child_value(0).get::<u32>().unwrap_or(0) as i64;
                        // Deleting now hands the freed pages back, and the caller
                        // gets to say how much that was.
                        let freed = daemon.store.delete(id).unwrap_or(0) as u64;
                        daemon.refresh_panel();
                        invocation.return_value(Some(&(freed,).to_variant()));
                    }
                    "Clear" => {
                        let (removed, freed) = daemon.store.clear().unwrap_or((0, 0));
                        daemon.refresh_panel();
                        invocation.return_value(Some(&(removed as u32, freed as u64).to_variant()));
                    }
                    // The settings the shell extension needs: it has to know how
                    // often to look and how much it may send, and asking is the
                    // only way a settings file can change what it does.
                    "Config" => {
                        invocation.return_value(Some(
                            &(daemon.store.config().wire(),).to_variant(),
                        ));
                    }
                    // After a `clipnest import` the history changed under us; the
                    // panel has to be told, since nothing went through PushText.
                    "Refresh" => {
                        daemon.refresh_panel_now();
                        invocation.return_value(None);
                    }
                    "DiskUsage" => {
                        let database = daemon.store.disk_bytes().max(0) as u64;
                        let reclaimable = daemon.store.reclaimable_bytes().max(0) as u64;
                        invocation.return_value(Some(&(database, reclaimable).to_variant()));
                    }
                    "Vacuum" => {
                        let freed = daemon.store.vacuum().unwrap_or(0) as u64;
                        invocation.return_value(Some(&(freed,).to_variant()));
                    }
                    "Status" => {
                        let (total, images, pinned) = daemon.store.stats().unwrap_or((0, 0, 0));
                        let db_bytes = daemon.store.disk_bytes().max(0) as u64;
                        let version = env!("CARGO_PKG_VERSION").to_string();
                        // `is_mapped` only becomes true once the compositor has
                        // actually put the panel on screen.
                        let panel = match daemon.existing_panel() {
                            Some(ui) if ui.window.is_mapped() => "open",
                            _ => "closed",
                        };
                        let summary = format!(
                            "clipnest {version}: {total} entries ({images} images, {pinned} pinned), \
                             panel {panel}, db {}",
                            human_bytes(db_bytes as i64)
                        );
                        invocation.return_value(Some(
                            &(summary, version, total as u32, images as u32, pinned as u32, db_bytes)
                                .to_variant(),
                        ));
                    }
                    other => {
                        eprintln!("clipnest: ignoring unknown D-Bus method {other}");
                        invocation.return_value(None);
                    }
                }
            },
        )
        .build();

    match registered {
        Ok(_) => {
            // The interface exists now, so this is the earliest point at which the
            // name may be published: a caller woken up by it can never be answered
            // by a process that does not serve it yet. systemd's `Type=dbus`
            // watches for exactly this name to call the start successful.
            if let Err(err) = claim_bus_name(&connection) {
                eprintln!("clipnest: cannot take the name {BUS_NAME}: {err}");
                std::process::exit(1);
            }
        }
        Err(err) => {
            eprintln!("clipnest: cannot export {INTERFACE}: {err}");
            std::process::exit(1);
        }
    }
}

/// Takes the well-known name on the session bus by hand.
fn claim_bus_name(connection: &gio::DBusConnection) -> Result<(), String> {
    let reply = connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(BUS_NAME, NAME_FLAG_DO_NOT_QUEUE).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            gio::Cancellable::NONE,
        )
        .map_err(|err| err.message().to_string())?;

    let code = reply.child_value(0).get::<u32>().unwrap_or(0);
    if code == NAME_REPLY_PRIMARY_OWNER || code == NAME_REPLY_ALREADY_OWNER {
        Ok(())
    } else {
        Err(format!("the name is already taken (reply {code})"))
    }
}

/// The rows `List` and `Search` marshal back, as `a(ussuub)`. A `Vec` of tuples
/// converts straight into that signature.
type Rows = Vec<(u32, String, String, u32, u32, bool)>;

fn rows_of(items: &[Item]) -> Rows {
    items
        .iter()
        .map(|item| {
            (
                item.id as u32,
                item.kind.as_str().to_string(),
                item.preview(),
                item.width,
                item.height,
                item.pinned,
            )
        })
        .collect()
}

/// How many entries a `List`/`Search` call may ask for.
fn panel_limit(value: &glib::Variant) -> i64 {
    value.get::<u32>().unwrap_or(50).clamp(1, 1000) as i64
}

fn store_text(daemon: &Daemon, text: &str) {
    let config = daemon.store.config().clone();
    if text.is_empty() || !config.takes_text() {
        return;
    }
    if text.len() as i64 > config.max_text_bytes {
        eprintln!(
            "clipnest: ignoring a {} text (limit {})",
            human_bytes(text.len() as i64),
            human_bytes(config.max_text_bytes)
        );
        return;
    }
    daemon.store.push(&Content::Text(text.to_string())).ok();
    daemon.refresh_panel();
}

fn store_image(daemon: &Daemon, mime: &str, data: Vec<u8>) {
    let config = daemon.store.config().clone();
    if data.is_empty() || !config.takes_images() {
        return;
    }
    if data.len() as i64 > config.max_image_bytes {
        eprintln!(
            "clipnest: ignoring a {} image (limit {})",
            human_bytes(data.len() as i64),
            human_bytes(config.max_image_bytes)
        );
        return;
    }
    // Decoding once here both validates the payload and gives us the size the
    // panel needs, without re-decoding it on every rebuild.
    let Some((width, height)) = image_size(&data) else {
        eprintln!("clipnest: cannot decode the {mime} image, ignoring it");
        return;
    };
    let mime = if mime.is_empty() {
        "image/png".to_string()
    } else {
        mime.to_string()
    };
    let image = Content::Image {
        mime,
        data,
        width,
        height,
    };
    daemon.store.push(&image).ok();
    daemon.refresh_panel();
}

/// The size of an encoded image, or `None` when it cannot be decoded.
///
/// Exposed to the crate because `clipnest import` validates backups with the
/// same check the daemon uses on a freshly copied image.
pub(crate) fn image_size(data: &[u8]) -> Option<(u32, u32)> {
    let texture = texture_from_bytes(data)?;
    Some((texture.width() as u32, texture.height() as u32))
}

fn texture_from_bytes(data: &[u8]) -> Option<gdk::Texture> {
    let bytes = glib::Bytes::from(data);
    gdk::Texture::from_bytes(&bytes).ok()
}

fn on_key(ui: &Rc<Ui>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);

    // Checked up front so no key-variant guessing is needed for plain letters.
    if ctrl {
        if let Some(letter) = key.to_unicode().map(|ch| ch.to_ascii_lowercase()) {
            if letter == 'p' {
                pin_selected(ui);
                return glib::Propagation::Stop;
            }
        }
    }

    match key {
        gdk::Key::Escape => {
            ui.window.set_visible(false);
            glib::Propagation::Stop
        }
        gdk::Key::Return | gdk::Key::KP_Enter => {
            if let Some(row) = ui.list.selected_row() {
                activate_row(ui, row.index());
            }
            glib::Propagation::Stop
        }
        gdk::Key::Delete => {
            delete_selected(ui);
            glib::Propagation::Stop
        }
        // Backspace belongs to whatever the user is typing in the search box.
        // The only way a row can be under it is a mouse click that focused the
        // row; the keyboard cannot get there, see `Tab` below.
        gdk::Key::BackSpace if !ui.search.has_focus() => {
            delete_selected(ui);
            glib::Propagation::Stop
        }
        // `Tab` moves the selection instead of handing the focus to a row. The
        // panel is built around the search box keeping the focus, and a row that
        // held it would silently swallow everything typed next; Shift+Tab walks
        // back. Arriving as `ISO_Left_Tab` is how the keyboard reports Shift+Tab.
        gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
            let backwards =
                key == gdk::Key::ISO_Left_Tab || state.contains(gdk::ModifierType::SHIFT_MASK);
            step_selection(ui, if backwards { -1 } else { 1 });
            glib::Propagation::Stop
        }
        gdk::Key::Down => {
            step_selection(ui, 1);
            glib::Propagation::Stop
        }
        gdk::Key::Up => {
            step_selection(ui, -1);
            glib::Propagation::Stop
        }
        gdk::Key::Page_Down => {
            step_selection(ui, 5);
            glib::Propagation::Stop
        }
        gdk::Key::Page_Up => {
            step_selection(ui, -5);
            glib::Propagation::Stop
        }
        gdk::Key::Home => {
            select_index(ui, 0);
            glib::Propagation::Stop
        }
        gdk::Key::End => {
            let last = ui.items.borrow().len() as i32 - 1;
            select_index(ui, last);
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    }
}

fn selected_id(ui: &Rc<Ui>) -> Option<i64> {
    let index = ui.list.selected_row()?.index();
    ui.items.borrow().get(index as usize).map(|item| item.id)
}

fn pin_selected(ui: &Rc<Ui>) {
    if let Some(id) = selected_id(ui) {
        toggle_pin(ui, id);
    }
}

fn delete_selected(ui: &Rc<Ui>) {
    if let Some(id) = selected_id(ui) {
        delete_item(ui, id);
    }
}

fn step_selection(ui: &Rc<Ui>, delta: i32) {
    let count = ui.items.borrow().len() as i32;
    if count == 0 {
        return;
    }
    let current = ui.list.selected_row().map(|row| row.index()).unwrap_or(0);
    select_index(ui, (current + delta).rem_euclid(count));
}

/// Selects a row without stealing the focus from the search entry.
fn select_index(ui: &Rc<Ui>, index: i32) {
    let Some(row) = ui.list.row_at_index(index) else {
        return;
    };
    ui.list.select_row(Some(&row));
    row.grab_focus();
    scroll_row_into_view(ui, &row);
    ui.search.grab_focus();
}

fn scroll_row_into_view(ui: &Rc<Ui>, row: &gtk::ListBoxRow) {
    let Some(bounds) = row.compute_bounds(&ui.list) else {
        return;
    };
    let adjustment = ui.scroller.vadjustment();
    let top = bounds.y() as f64;
    let bottom = top + bounds.height() as f64;
    let page = adjustment.page_size();
    if top < adjustment.value() {
        adjustment.set_value(top);
    } else if bottom > adjustment.value() + page {
        adjustment.set_value(bottom - page);
    }
}

fn activate_row(ui: &Rc<Ui>, index: i32) {
    let id = ui.items.borrow().get(index as usize).map(|item| item.id);
    if let Some(id) = id {
        copy_item(ui, id, true);
    }
}

/// Puts an entry back on the clipboard and marks it as used. Needs no window,
/// so `clipnest pick` and the D-Bus `Pick` call work with the panel closed.
fn copy_to_clipboard_item(store: &Store, id: i64) -> bool {
    let Some(content) = store.content(id).ok().flatten() else {
        return false;
    };
    copy_to_clipboard(&content);
    store.touch(id).ok();
    true
}

/// The panel's version of the same thing: it gets out of the way, and when asked
/// for it types the paste shortcut into the window that gets the focus back.
fn copy_item(ui: &Rc<Ui>, id: i64, paste: bool) -> bool {
    if !copy_to_clipboard_item(&ui.store, id) {
        return false;
    }
    ui.window.set_visible(false);
    if paste {
        if let Some(accel) = ui.store.config().paste_key.clone() {
            paste_when_focused(ui, accel);
        }
    }
    true
}

/// `Paste` over D-Bus: the same thing a click does, with an answer for the CLI.
fn paste_entry(daemon: &Daemon, id: i64) -> String {
    let accel = daemon.store.config().paste_key.clone();
    let copied = match daemon.existing_panel() {
        Some(ui) => copy_item(&ui, id, true),
        None => {
            // No panel to get out of the way, so there is nothing to wait for and
            // the keys can go straight out.
            let copied = copy_to_clipboard_item(&daemon.store, id);
            if copied {
                if let Some(accel) = accel.clone() {
                    paste::request(accel);
                }
            }
            copied
        }
    };
    if !copied {
        return format!("there is no entry with id {id}");
    }
    match accel {
        // Claiming a paste that is still sitting behind a permission dialog
        // would be a lie the user only discovers in their editor, so "ready" is
        // the only state in which the paste is reported as done.
        Some(accel) => {
            let status = paste::status();
            if status.ready() {
                format!("pasting with {}", accel.describe())
            } else {
                format!("copied; {}", status.note)
            }
        }
        None => "copied, but auto-paste is switched off in the settings".to_string(),
    }
}

/// Types the paste shortcut once the panel has really let go of the keyboard.
///
/// Hiding the window is not the same as handing the focus back: the compositor
/// does that a moment later, and typing into that gap would send the keys to the
/// panel on its way out - or to nothing at all. So the window is polled until it
/// is no longer active, plus a short grace period, with a ceiling so a panel that
/// somehow stays focused cannot hold the paste hostage.
fn paste_when_focused(ui: &Rc<Ui>, accel: Accelerator) {
    /// Ticks of 30 ms: 750 ms to lose the focus, 120 ms for the compositor to
    /// give it to the next window.
    const PATIENCE: u32 = 25;
    const GRACE: u32 = 4;

    let waited = Rc::new(Cell::new(0u32));
    let grace = Rc::new(Cell::new(0u32));
    let weak = Rc::downgrade(ui);
    glib::timeout_add_local(Duration::from_millis(30), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if ui.window.is_active() && waited.get() < PATIENCE {
            waited.set(waited.get() + 1);
            return glib::ControlFlow::Continue;
        }
        grace.set(grace.get() + 1);
        if grace.get() < GRACE {
            return glib::ControlFlow::Continue;
        }
        paste::request(accel.clone());
        glib::ControlFlow::Break
    });
}

/// The line under the list: the keys, and - when it matters - why picking an
/// entry will only copy it.
fn set_hint(hint: &gtk::Label, store: &Store) {
    let keys = "del remove    ctrl+p pin    esc close";
    let label = match store.config().paste_key.as_ref() {
        None => format!("\u{21b5} copy    {keys}"),
        Some(_) if paste::status().ready() => format!("\u{21b5} copy & paste    {keys}"),
        Some(_) => format!(
            "\u{21b5} copy \u{00b7} paste needs permission: clipnest paste-access    {keys}"
        ),
    };
    hint.set_text(&label);
    // The short line is a summary; the full answer is what the paste thread last
    // said, and that can be long enough not to belong under a 480px list. The
    // reason's own advice is appended so a tooltip says what to do, not only
    // what went wrong.
    let status = paste::status();
    let advice = status.reason.advice();
    let tooltip = match (status.note.is_empty(), advice.is_empty()) {
        (true, true) => None,
        (false, true) => Some(status.note.clone()),
        (true, false) => Some(advice.to_string()),
        (false, false) => Some(format!("{}\n{}", status.note, advice)),
    };
    hint.set_tooltip_text(tooltip.as_deref());
}

fn toggle_pin(ui: &Rc<Ui>, id: i64) {
    ui.store.toggle_pinned(id).ok();
    refresh(ui);
    // Keep the selection on the entry we just pinned.
    if let Some(index) = ui.items.borrow().iter().position(|item| item.id == id) {
        if let Some(row) = ui.list.row_at_index(index as i32) {
            ui.list.select_row(Some(&row));
            scroll_row_into_view(ui, &row);
        }
    }
}

fn delete_item(ui: &Rc<Ui>, id: i64) {
    let index = ui
        .items
        .borrow()
        .iter()
        .position(|item| item.id == id)
        .unwrap_or(0) as i32;
    ui.store.delete(id).ok();
    refresh(ui);
    let count = ui.items.borrow().len() as i32;
    if count > 0 {
        select_index(ui, index.min(count - 1).max(0));
    }
}

fn copy_to_clipboard(content: &Content) {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let clipboard = display.clipboard();
    // Middle-click paste uses the PRIMARY selection on X11 and Wayland alike.
    // Filling it too keeps both ways of pasting pointing at the entry just
    // picked, instead of at whatever was selected before.
    let primary = display.primary_clipboard();
    match content {
        Content::Text(text) => {
            clipboard.set_text(text);
            primary.set_text(text);
        }
        Content::Image { data, .. } => match texture_from_bytes(data) {
            Some(texture) => {
                clipboard.set_texture(&texture);
                primary.set_texture(&texture);
            }
            None => eprintln!("clipnest: the stored image can no longer be decoded"),
        },
    }
}

fn refresh(ui: &Rc<Ui>) {
    let query = ui.search.text().to_string();
    rebuild(ui, &query);
}

fn rebuild(ui: &Rc<Ui>, query: &str) {
    while let Some(child) = ui.list.first_child() {
        ui.list.remove(&child);
    }
    ui.textures.borrow_mut().clear();

    let limit = ui.store.config().panel_limit;
    let items = ui.store.list(query, limit).unwrap_or_default();
    let mut pictures = Vec::new();

    if items.is_empty() {
        let message = if query.trim().is_empty() {
            "Clipboard history is empty"
        } else {
            "No matches"
        };
        let label = gtk::Label::new(Some(message));
        label.add_css_class("clipnest-empty");
        let row = gtk::ListBoxRow::new();
        row.set_selectable(false);
        row.set_activatable(false);
        row.set_child(Some(&label));
        ui.list.append(&row);
    } else {
        for item in &items {
            let (row, picture) = build_row(ui, item);
            ui.list.append(&row);
            pictures.push(picture);
        }
    }

    *ui.items.borrow_mut() = items;
    *ui.pictures.borrow_mut() = pictures;

    if let Some(row) = ui.list.row_at_index(0) {
        ui.list.select_row(Some(&row));
    }
    paint_visible_images(ui);
}

fn build_row(ui: &Rc<Ui>, item: &Item) -> (gtk::ListBoxRow, Option<gtk::Picture>) {
    let row = gtk::ListBoxRow::new();
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.set_margin_top(7);
    content.set_margin_bottom(7);
    content.set_margin_start(10);
    content.set_margin_end(10);

    let mut picture = None;
    if item.is_image() {
        let thumb = gtk::Picture::new();
        thumb.set_size_request(88, 56);
        thumb.set_content_fit(gtk::ContentFit::Contain);
        thumb.add_css_class("clipnest-thumb");
        content.append(&thumb);
        picture = Some(thumb);

        let format = item.mime.as_deref().map(mime_label).unwrap_or("image");
        let meta = gtk::Label::new(Some(&format!("{}  \u{00b7}  {format}", item.preview())));
        meta.set_xalign(0.0);
        meta.set_hexpand(true);
        meta.add_css_class("clipnest-meta");
        content.append(&meta);
    } else {
        let label = gtk::Label::new(Some(&item.preview()));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_lines(2);
        label.set_wrap(true);
        label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        content.append(&label);
    }

    let time = gtk::Label::new(Some(&relative_time(item.last_used_at)));
    time.add_css_class("clipnest-time");
    content.append(&time);

    let pin = gtk::Button::new();
    pin.set_icon_name("view-pin-symbolic");
    pin.add_css_class("flat");
    pin.add_css_class("clipnest-pin");
    pin.set_valign(gtk::Align::Center);
    if item.pinned {
        pin.add_css_class("pinned");
        pin.set_tooltip_text(Some("Unpin this entry (Ctrl+P)"));
    } else {
        pin.set_tooltip_text(Some("Pin this entry (Ctrl+P)"));
    }
    {
        let ui = ui.clone();
        let id = item.id;
        pin.connect_clicked(move |_| toggle_pin(&ui, id));
    }
    content.append(&pin);

    // Right click: copy, pin or delete this row.
    let click = gtk::GestureClick::new();
    click.set_button(3);
    {
        let ui = ui.clone();
        let id = item.id;
        click.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            open_menu(&ui, id, x, y);
        });
    }
    row.add_controller(click);

    row.set_tooltip_text(Some(&format!(
        "copied {}  \u{00b7}  last used {}",
        relative_time(item.created_at),
        relative_time(item.last_used_at)
    )));
    row.set_child(Some(&content));
    (row, picture)
}

fn mime_label(mime: &str) -> &str {
    match mime {
        "image/png" => "PNG",
        "image/jpeg" => "JPEG",
        "image/webp" => "WEBP",
        "image/tiff" => "TIFF",
        "image/bmp" => "BMP",
        other => other,
    }
}

fn open_menu(ui: &Rc<Ui>, id: i64, x: f64, y: f64) {
    *ui.menu_target.borrow_mut() = Some(id);
    let row = ui
        .items
        .borrow()
        .iter()
        .position(|item| item.id == id)
        .and_then(|index| ui.list.row_at_index(index as i32));
    let Some(row) = row else { return };

    ui.list.select_row(Some(&row));
    if ui.popover.parent().is_some() {
        ui.popover.unparent();
    }
    ui.popover.set_parent(&row);
    ui.popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    ui.popover.popup();
}

/// Decodes (and caches) the thumbnail of an image entry.
fn item_texture(ui: &Rc<Ui>, id: i64) -> Option<gdk::Texture> {
    if let Some(texture) = ui.textures.borrow_mut().get(id) {
        return Some(texture.clone());
    }
    let Some(Content::Image { data, .. }) = ui.store.content(id).ok().flatten() else {
        return None;
    };
    let texture = texture_from_bytes(&data)?;
    let size = decoded_bytes(&texture);
    ui.textures.borrow_mut().insert(id, texture.clone(), size);
    Some(texture)
}

/// Decodes thumbnails for the rows near the viewport and releases the rest, so
/// a long history of screenshots cannot grow the panel without bound.
fn paint_visible_images(ui: &Rc<Ui>) {
    if ui.pictures.borrow().is_empty() {
        return;
    }
    let adjustment = ui.scroller.vadjustment();
    let top = adjustment.value() - VIEWPORT_MARGIN;
    let bottom = adjustment.value() + adjustment.page_size() + VIEWPORT_MARGIN;

    let items = ui.items.borrow();
    let pictures = ui.pictures.borrow();
    for (index, item) in items.iter().enumerate() {
        let Some(picture) = pictures.get(index).and_then(Option::as_ref) else {
            continue;
        };
        let Some(row) = ui.list.row_at_index(index as i32) else {
            continue;
        };
        let visible = match row.compute_bounds(&ui.list) {
            Some(bounds) => {
                let y = bounds.y() as f64;
                y + bounds.height() as f64 >= top && y <= bottom
            }
            // Not allocated yet: paint rather than show an empty frame.
            None => true,
        };
        if visible {
            if picture.paintable().is_none() {
                if let Some(texture) = item_texture(ui, item.id) {
                    picture.set_paintable(Some(&texture));
                }
            }
        } else if picture.paintable().is_some() {
            picture.set_paintable(None::<&gdk::Texture>);
        }
    }
}

fn relative_time(then_ms: i64) -> String {
    let delta = (now_ms() - then_ms).max(0) / 1000;
    match delta {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", delta / 60),
        3600..=86399 => format!("{}h", delta / 3600),
        86400..=2_591_999 => format!("{}d", delta / 86400),
        _ => format!("{}mo", delta / 2_592_000),
    }
}

pub fn human_bytes(bytes: i64) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KIB {
        format!("{bytes} B")
    } else if value < KIB * KIB {
        format!("{:.0} KiB", value / KIB)
    } else if value < KIB * KIB * KIB {
        format!("{:.1} MiB", value / (KIB * KIB))
    } else {
        format!("{:.2} GiB", value / (KIB * KIB * KIB))
    }
}

fn show(ui: &Rc<Ui>, token: &str) {
    // The permission may have been granted since the panel was last opened.
    set_hint(&ui.hint, &ui.store);
    // Clearing the search box rebuilds the list by itself when there was a query
    // in it; the extra build is only there for copies that arrived while the
    // panel was hidden (see `Daemon::refresh_panel`).
    let had_query = !ui.search.text().is_empty();
    ui.search.set_text("");
    if had_query || ui.dirty.get() {
        refresh(ui);
    }
    ui.dirty.set(false);
    // `realize` exists on both WidgetExt and NativeExt, so spell it out.
    gtk::prelude::WidgetExt::realize(&ui.window);
    if let Some(surface) = ui.window.surface() {
        if let Ok(toplevel) = surface.downcast::<gdk::Toplevel>() {
            if !token.is_empty() {
                // Wayland's activation token: without it the compositor would
                // refuse to focus our panel.
                toplevel.set_startup_id(token);
            }
        }
    }
    ui.window.present();
    ui.search.grab_focus();
    // The rows only know where they are once the compositor laid them out.
    let ui = ui.clone();
    glib::idle_add_local_once(move || paint_visible_images(&ui));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumbnail_cache_stays_within_its_budget() {
        let mut cache: Budgeted<i64> = Budgeted::new(100);
        cache.insert(1, 1, 40);
        cache.insert(2, 2, 40);
        assert_eq!(cache.bytes, 80);

        // Looking at 1 made it the most recent, so 2 is the one that goes.
        assert!(cache.get(1).is_some());
        cache.insert(3, 3, 40);
        assert_eq!(cache.bytes, 80);
        assert_eq!(cache.get(1), Some(&1));
        assert!(cache.get(2).is_none());
        assert!(cache.get(3).is_some());

        // Re-inserting an id replaces it instead of counting it twice.
        cache.insert(1, 11, 30);
        assert_eq!(cache.bytes, 70);
        assert_eq!(cache.get(1), Some(&11));

        // Something bigger than the whole budget is not cached at all - better a
        // re-decode than a panel that cannot be used.
        cache.insert(4, 4, 500);
        assert!(cache.get(4).is_none());
        assert_eq!(cache.bytes, 70);

        cache.clear();
        assert_eq!(cache.bytes, 0);
        assert!(cache.get(1).is_none());
    }

    #[test]
    fn forgetting_an_entry_gives_its_budget_back() {
        let mut cache: Budgeted<i64> = Budgeted::new(100);
        cache.insert(1, 1, 60);
        cache.insert(2, 2, 30);
        assert_eq!(cache.bytes, 90);
        cache.forget(1);
        assert_eq!(cache.bytes, 30);
        assert!(cache.get(1).is_none());
        // Forgetting something that was never there is harmless.
        cache.forget(99);
        assert_eq!(cache.bytes, 30);
    }
}

