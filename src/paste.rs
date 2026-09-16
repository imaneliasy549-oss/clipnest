//! Auto-paste: putting the entry the user picked into the window they came from.
//!
//! Reading the clipboard on Wayland is impossible without the shell extension
//! (see `app.rs`), and *writing* to another application is the same story one
//! level up: no client may synthesise input for another window. The one door
//! that exists is `org.freedesktop.portal.RemoteDesktop`, the same API remote
//! desktop tools use. It asks the user once - "allow this application to control
//! the keyboard?" - hands back a session, and then accepts individual key
//! presses.
//!
//! Three things about this protocol shape the code below:
//!
//! * **Every call answers through a signal, not through its reply.** A request
//!   is sent as an object path built from a token *we* choose, and the result
//!   arrives as a `Response` signal on that path. Subscribing first and calling
//!   second is what makes the answer impossible to miss.
//! * **The permission dialog waits for a human.** It can stay open for minutes,
//!   which is why nothing here runs on the daemon's main loop: the protocol
//!   lives on its own thread, and the panel only ever hands it work to do.
//! * **A session belongs to the connection that created it.** If it is gone -
//!   the portal restarted, the daemon did - the session has to be replaced
//!   rather than reused, so the caller retries once on a fresh connection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use glib::translate::IntoGlib;
use glib::variant::{ObjectPath, ToVariant};
use gtk::gdk;

use crate::install;

pub const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE_REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";
const IFACE_REQUEST: &str = "org.freedesktop.portal.Request";

/// `AvailableDeviceTypes`: we ask for the keyboard and nothing else. A clipboard
/// popup that could move the pointer or read the screen would be asking for far
/// more than it needs, and the permission dialog says so.
const DEVICE_KEYBOARD: u32 = 1;
/// `NotifyKeyboardKeysym` states.
const PRESSED: u32 = 1;
const RELEASED: u32 = 0;
/// `persist_mode`: remember the grant, so this is a once-per-user question
/// instead of a once-per-session one.
const PERSIST_UNTIL_REVOKED: u32 = 2;

/// How long a single call may take.
const CALL_TIMEOUT_MS: i32 = 25_000;
/// How long the whole handshake may take. Generous on purpose: this covers a
/// permission dialog nobody is standing next to.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(300);
/// Between two synthetic key events. Applications read the keyboard from a
/// queue, and a modifier that arrives in the same millisecond as its key is not
/// always applied first.
const KEY_GAP: Duration = Duration::from_millis(4);

/// The order modifiers are pressed in; released in reverse.
const MODIFIER_ORDER: [&str; 6] = ["ctrl", "shift", "alt", "super", "hyper", "meta"];

/// The keysym of each modifier's left-hand key.
fn modifier_keysym(name: &str) -> u32 {
    match name {
        "ctrl" => 0xffe3,  // XK_Control_L
        "shift" => 0xffe1, // XK_Shift_L
        "alt" => 0xffe9,   // XK_Alt_L
        "super" => 0xffeb, // XK_Super_L
        "hyper" => 0xffed, // XK_Hyper_L
        _ => 0xffe7,       // XK_Meta_L
    }
}

/// The keysym GDK knows a key name by.
///
/// `gdk_keyval_from_name` is a pure table lookup with no display behind it, so
/// this works in a daemon that was started over SSH - the same reason
/// `install::split_binding` validates key names this way.
fn keysym_of(name: &str) -> Option<u32> {
    gdk::Key::from_name(name).map(|key| key.into_glib())
}

/// A shortcut to type: the modifiers, and the key they are held for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accelerator {
    /// Canonical modifier names, in the order they are pressed.
    modifiers: Vec<&'static str>,
    /// The keysym of the main key.
    keysym: u32,
    /// The key as it was written, for messages.
    key: String,
}

impl Accelerator {
    /// Parses an accelerator such as `<Control>v`.
    ///
    /// The grammar and the error messages are the ones `clipnest
    /// setup-shortcut` already uses, so a binding that GNOME accepts here is a
    /// binding GNOME would accept as a shortcut.
    pub fn parse(binding: &str) -> Result<Self, String> {
        let (written, key) = install::split_binding(binding)?;
        let keysym = keysym_of(&key)
            .ok_or_else(|| format!("'{key}' is not a key GDK can name"))?;
        // GDK does not care in which order the modifiers were written; the
        // sequence below does, so it is fixed here rather than wherever the
        // binding happened to come from.
        let modifiers = MODIFIER_ORDER
            .iter()
            .copied()
            .filter(|name| written.contains(name))
            .collect();
        Ok(Accelerator {
            modifiers,
            keysym,
            key,
        })
    }

    /// The key events to send, as `(keysym, state)` pairs: modifiers down, the
    /// key, the key up, modifiers up.
    pub fn sequence(&self) -> Vec<(u32, u32)> {
        let mut events = Vec::with_capacity(self.modifiers.len() * 2 + 2);
        for name in &self.modifiers {
            events.push((modifier_keysym(name), PRESSED));
        }
        events.push((self.keysym, PRESSED));
        events.push((self.keysym, RELEASED));
        for name in self.modifiers.iter().rev() {
            events.push((modifier_keysym(name), RELEASED));
        }
        events
    }

    /// A keystroke that types nothing: the left Control key, pressed and
    /// released on its own. It is the one thing worth sending to find out
    /// whether a session really delivers keys - no application does anything
    /// with a modifier that has no key after it, so a probe cannot end up as
    /// text in somebody's document.
    fn probe() -> Self {
        Accelerator {
            modifiers: Vec::new(),
            keysym: 0xffe3, // XK_Control_L
            key: "Control_L".to_string(),
        }
    }

    /// The binding as it is spelled in the settings file.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for name in &self.modifiers {
            out.push_str(&format!("<{name}>"));
        }
        out.push_str(&self.key);
        out
    }
}

/// Where the paste channel stands.
///
/// `Asking` is what separates "still waiting for the permission dialog" from "the
/// portal said no": both are not-ready, and a caller that cannot tell them apart
/// can only poll and hope - which is exactly what `clipnest paste-access` used to
/// do, for three minutes, after the daemon had already given up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing has been asked for yet.
    Idle,
    /// The portal is being asked; a permission dialog may be on screen.
    Asking,
    /// A session exists and delivered a keystroke.
    Ready,
    /// The last attempt failed, for the reason in the note.
    Failed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Asking => "asking",
            State::Ready => "ready",
            State::Failed => "failed",
        }
    }

    /// The other end of `as_str`, which is what travels over D-Bus. Anything
    /// unrecognised is `Idle`: a daemon from a different build should not be able
    /// to make a CLI claim that pasting works.
    pub fn parse(text: &str) -> Self {
        match text {
            "asking" => State::Asking,
            "ready" => State::Ready,
            "failed" => State::Failed,
            _ => State::Idle,
        }
    }
}

/// Why the channel is not ready, when it is not.
///
/// The cases are separated where the failure happens, not afterwards. A caller
/// that re-derived them later - by asking whether the screen happened to be
/// locked while the message was being *read* - would describe the state of the
/// world instead of the reason for the failure, and would name the wrong thing
/// the moment somebody unlocked the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Nothing to explain: nobody has asked yet, or the note says it all.
    Unknown,
    /// No portal on the bus, or no backend that offers a keyboard session.
    Unavailable,
    /// The dialog was cancelled or refused, so auto-paste stays off.
    Denied,
    /// The lock screen is up, and GNOME refuses synthetic input while it is.
    Locked,
    /// A session was granted, but typing into it failed.
    Broken,
}

impl Reason {
    /// The spelling that travels over D-Bus and into `doctor`'s output.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Unknown => "unknown",
            Reason::Unavailable => "unavailable",
            Reason::Denied => "denied",
            Reason::Locked => "locked",
            Reason::Broken => "broken",
        }
    }

    /// The other end of `as_str`. Anything unrecognised is `Unknown`, for the
    /// same reason `State::parse` falls back to `Idle`: a daemon from a different
    /// build must not be able to make the CLI claim something it cannot know.
    pub fn parse(text: &str) -> Self {
        match text {
            "unavailable" => Reason::Unavailable,
            "denied" => Reason::Denied,
            "locked" => Reason::Locked,
            "broken" => Reason::Broken,
            _ => Reason::Unknown,
        }
    }

    /// The next step for the user, in one line. Empty when there is nothing to
    /// do about it.
    pub fn advice(self) -> &'static str {
        match self {
            Reason::Unknown => "",
            Reason::Unavailable => {
                "make sure xdg-desktop-portal and a backend that supports RemoteDesktop are \
                 installed and running"
            }
            Reason::Denied => "run 'clipnest paste-access' to be asked again",
            Reason::Locked => "unlock the screen, then run 'clipnest paste-access'",
            Reason::Broken => {
                "try again; if it keeps failing: systemctl --user restart clipnest"
            }
        }
    }
}

/// A refusal, with the reason kept apart from the sentence that explains it, so
/// the plumbing cannot lose one while carrying the other.
#[derive(Debug)]
struct Failure {
    reason: Reason,
    note: String,
}

impl Failure {
    fn new(reason: Reason, note: impl Into<String>) -> Self {
        Failure {
            reason,
            note: note.into(),
        }
    }
}

/// Reporting a failure prints the sentence and not the classification: the
/// reason is for choosing what to do next, `note` is for reading.
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.note)
    }
}

/// What the paste channel can currently do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    /// Why it is not ready, when it is not.
    pub reason: Reason,
    /// One line for the panel, `doctor` and `clipnest paste-access`.
    pub note: String,
}

impl Status {
    pub fn ready(&self) -> bool {
        self.state == State::Ready
    }
}

impl Default for Status {
    fn default() -> Self {
        Status {
            state: State::Idle,
            reason: Reason::Unknown,
            note: "not set up yet; 'clipnest paste-access' asks for permission".to_string(),
        }
    }
}

// --------------------------------------------------------------------------
// What the rest of the program calls
// --------------------------------------------------------------------------

/// Types an accelerator into whatever window has the focus.
///
/// Returns immediately: the protocol runs on its own thread, and asking for the
/// permission the first time can take as long as the user needs to answer.
pub fn request(accel: Accelerator) {
    queue(Job::Paste(accel));
}

/// Starts (or reuses) the portal session without typing anything, so the
/// permission question is asked before the first click rather than during it.
pub fn request_access() {
    queue(Job::Prepare);
}

/// The last thing the paste thread learned.
pub fn status() -> Status {
    status_slot()
        .lock()
        .map(|status| status.clone())
        .unwrap_or_default()
}

enum Job {
    Prepare,
    Paste(Accelerator),
}

fn status_slot() -> &'static Mutex<Status> {
    static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();
    STATUS.get_or_init(|| Mutex::new(Status::default()))
}

fn set_status(status: Status) {
    if let Ok(mut slot) = status_slot().lock() {
        *slot = status;
    }
}

fn jobs() -> &'static Mutex<Option<Sender<Job>>> {
    static JOBS: OnceLock<Mutex<Option<Sender<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(None))
}

fn queue(job: Job) {
    let mut slot = match jobs().lock() {
        Ok(slot) => slot,
        Err(_) => return,
    };
    if slot.is_none() {
        let (sender, receiver) = channel();
        match std::thread::Builder::new()
            .name("clipnest-paste".to_string())
            .spawn(move || worker(receiver))
        {
            Ok(_) => *slot = Some(sender),
            Err(err) => {
                set_status(Status {
                    state: State::Failed,
                    reason: Reason::Broken,
                    note: format!("cannot start the paste thread: {err}"),
                });
                return;
            }
        }
    }
    let sent = slot.as_ref().map(|sender| sender.send(job).is_ok());
    // A thread that died takes its channel with it; the next call starts a new
    // one instead of failing for the rest of the session.
    if sent != Some(true) {
        *slot = None;
        set_status(Status {
            state: State::Failed,
            reason: Reason::Broken,
            note: "the paste thread stopped; try again".to_string(),
        });
    }
}

fn worker(jobs: Receiver<Job>) {
    // The `Response` signals this protocol answers with are delivered to the
    // thread-default main context of whoever subscribed. Giving this thread one
    // of its own is what lets `Client::request` wait for the permission dialog
    // while the daemon's own loop keeps serving the clipboard.
    let context = glib::MainContext::new();
    let _ = context.with_thread_default(|| {
        let mut client: Option<Client> = None;
        while let Ok(job) = jobs.recv() {
            // Published before the work starts, because the handshake may sit in
            // front of a permission dialog for minutes and a caller deserves to
            // know that is what it is waiting for.
            set_status(Status {
                state: State::Asking,
                reason: Reason::Unknown,
                note: "asking the portal for a keyboard session".to_string(),
            });
            let status = match run(job, &mut client) {
                Ok(note) => Status {
                    state: State::Ready,
                    reason: Reason::Unknown,
                    note,
                },
                Err(failure) => Status {
                    state: State::Failed,
                    reason: failure.reason,
                    note: failure.note,
                },
            };
            set_status(status);
        }
    });
}

/// What a portal call answers with: the response code, and the results of the
/// call. Shared with the signal callback that carries them.
type Answer = Arc<Mutex<Option<(u32, HashMap<String, glib::Variant>)>>>;
/// The call's own failure, if it had one before any answer arrived.
type CallFailure = Arc<Mutex<Option<String>>>;

/// One job: make sure there is a session, then (for a paste) type into it.
fn run(job: Job, client: &mut Option<Client>) -> Result<String, Failure> {
    let note = access(client)?;
    let Job::Paste(accel) = job else {
        return Ok(note);
    };
    let active = client.as_ref().expect("access() leaves a client behind");
    match active.send(&accel) {
        Ok(()) => Ok(format!("pasted with {}", accel.describe())),
        Err(first) => {
            // The session outlived its usefulness - the portal was restarted,
            // or the daemon was. A stale session cannot be repaired, only
            // replaced, and it is worth one retry before giving up.
            *client = None;
            access(client)?;
            let fresh = client.as_ref().expect("access() leaves a client behind");
            fresh.send(&accel).map_err(|second| {
                Failure::new(
                    second.reason,
                    format!("{}; after a new session: {}", first.note, second.note),
                )
            })?;
            Ok(format!("pasted with {} (new session)", accel.describe()))
        }
    }
}

fn access(client: &mut Option<Client>) -> Result<String, Failure> {
    if client.is_none() {
        *client = Some(Client::connect()?);
    }
    client
        .as_mut()
        .expect("a client was just installed")
        .ensure()
}

// --------------------------------------------------------------------------
// The portal session
// --------------------------------------------------------------------------

struct Client {
    connection: gio::DBusConnection,
    /// The session object path, once `Start` has been granted.
    session: Option<String>,
}

impl Client {
    fn connect() -> Result<Self, Failure> {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .map_err(|err| {
                Failure::new(
                    Reason::Unavailable,
                    format!("cannot reach the session bus: {err}"),
                )
            })?;
        Ok(Client {
            connection,
            session: None,
        })
    }

    /// Whether the session is usable, asking for it if it is not.
    fn ensure(&mut self) -> Result<String, Failure> {
        if self.session.is_some() {
            return Ok("the portal session is ready".to_string());
        }
        let session = self.create_session()?;
        self.select_devices(&session)?;
        let started = self.start(&session)?;
        if let Some(token) = started {
            save_token(&token);
        }
        self.session = Some(session);

        // A session that was granted and a session that works are two different
        // claims, and only one of them can be checked by looking at a reply. The
        // probe below makes the second one: it costs two D-Bus calls and one
        // harmless keystroke, and it turns "ready" from a hope into a fact.
        let granted = "the portal granted keyboard access";
        match self.send(&Accelerator::probe()) {
            Ok(()) => Ok(format!("{granted}, and a test key was delivered")),
            // The reason is the one the typing failure itself carried: a probe
            // refused because the screen is locked should still say so.
            Err(err) => Err(Failure::new(
                err.reason,
                format!("{granted}, but it refused a keystroke: {}", err.note),
            )),
        }
    }

    /// Types an accelerator into the session.
    fn send(&self, accel: &Accelerator) -> Result<(), Failure> {
        let session = self.session.as_deref().ok_or_else(|| {
            Failure::new(Reason::Broken, "there is no portal session to type into")
        })?;
        let session = object_path(session)?;
        let events = accel.sequence();
        for (index, (keysym, state)) in events.iter().enumerate() {
            if index > 0 {
                std::thread::sleep(KEY_GAP);
            }
            let params = (
                session.clone(),
                HashMap::<String, glib::Variant>::new(),
                *keysym as i32,
                *state,
            )
                .to_variant();
            self.connection
                .call_sync(
                    Some(PORTAL_BUS_NAME),
                    PORTAL_OBJECT_PATH,
                    IFACE_REMOTE_DESKTOP,
                    "NotifyKeyboardKeysym",
                    Some(&params),
                    None,
                    gio::DBusCallFlags::NONE,
                    CALL_TIMEOUT_MS,
                    gio::Cancellable::NONE,
                )
                .map_err(|err| {
                    Failure::new(
                        Reason::Broken,
                        format!("cannot type {}: {}", accel.describe(), err.message()),
                    )
                })?;
        }
        Ok(())
    }

    fn create_session(&self) -> Result<String, Failure> {
        let handle_token = self.new_token();
        let session_token = format!("{handle_token}_session");
        let options = self.options(
            &handle_token,
            &[("session_handle_token", session_token.to_variant())],
        );
        let results = self.request("CreateSession", &handle_token, (options,).to_variant())?;
        // The handle is in the reply, but it is also derived from the token we
        // just sent. Falling back to the derived path costs nothing and covers a
        // backend that answers without it.
        Ok(dict_string(&results, "session_handle")
            .unwrap_or_else(|| format!("{PORTAL_OBJECT_PATH}/session/{}/{session_token}", self.sender())))
    }

    /// Asks for the keyboard, and for the grant to be remembered.
    ///
    /// `persist_mode` is optional in the protocol and a backend may refuse it;
    /// a refused option is not a refused session, so it is retried without.
    fn select_devices(&self, session: &str) -> Result<(), Failure> {
        let restore = load_token();
        let mut extra: Vec<(&str, glib::Variant)> =
            vec![("types", DEVICE_KEYBOARD.to_variant())];
        if let Some(token) = restore.as_deref() {
            extra.push(("restore_token", token.to_variant()));
        }
        extra.push(("persist_mode", PERSIST_UNTIL_REVOKED.to_variant()));

        let handle_token = self.new_token();
        let options = self.options(&handle_token, &extra);
        let params = (object_path(session)?, options).to_variant();
        match self.request("SelectDevices", &handle_token, params) {
            Ok(_) => Ok(()),
            Err(first) => {
                // Nothing to restore and nothing to remember: just the keyboard.
                let handle_token = self.new_token();
                let options = self.options(
                    &handle_token,
                    &[("types", DEVICE_KEYBOARD.to_variant())],
                );
                let params = (object_path(session)?, options).to_variant();
                self.request("SelectDevices", &handle_token, params)
                    .map(|_| ())
                    .map_err(|second| {
                        Failure::new(
                            second.reason,
                            format!(
                                "{}; without remembering the grant: {}",
                                first.note, second.note
                            ),
                        )
                    })
            }
        }
    }

    /// The call that shows the permission dialog. Returns the restore token, when
    /// the backend offered one.
    fn start(&self, session: &str) -> Result<Option<String>, Failure> {
        let handle_token = self.new_token();
        let options = self.options(&handle_token, &[]);
        let params = (
            object_path(session)?,
            // No parent window: the daemon has no window of the caller's to hang
            // the dialog off, and an empty string is how the protocol says so.
            String::new(),
            options,
        )
            .to_variant();
        let results = self.request("Start", &handle_token, params)?;
        Ok(dict_string(&results, "restore_token"))
    }

    /// The option dictionary every call in this protocol shares: a handle token,
    /// so the answer arrives on a path that can be predicted, plus whatever the
    /// call itself needs.
    fn options(
        &self,
        handle_token: &str,
        extra: &[(&str, glib::Variant)],
    ) -> HashMap<String, glib::Variant> {
        let mut options: HashMap<String, glib::Variant> = HashMap::new();
        options.insert("handle_token".to_string(), handle_token.to_variant());
        for (key, value) in extra {
            options.insert((*key).to_string(), value.clone());
        }
        options
    }

    /// Issues one call and waits for its `Response` signal.
    ///
    /// The subscription is created **before** the call is made: the portal can
    /// answer at any moment after that, and a signal that arrives before anyone
    /// is listening is gone for good.
    fn request(
        &self,
        method: &str,
        handle_token: &str,
        params: glib::Variant,
    ) -> Result<HashMap<String, glib::Variant>, Failure> {
        let path = format!(
            "{PORTAL_OBJECT_PATH}/request/{}/{handle_token}",
            self.sender()
        );
        // Shared rather than `Rc`: a signal subscription may be delivered from
        // any thread, so the closure has to own something thread-safe.
        let response: Answer = Arc::new(Mutex::new(None));
        let failure: CallFailure = Arc::new(Mutex::new(None));

        let subscription = {
            let response = response.clone();
            self.connection.subscribe_to_signal(
                Some(PORTAL_BUS_NAME),
                Some(IFACE_REQUEST),
                Some("Response"),
                Some(&path),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    let parameters = signal.parameters;
                    let code = parameters.child_value(0).get::<u32>().unwrap_or(2);
                    let results = dict_entries(&parameters.child_value(1));
                    if let Ok(mut slot) = response.lock() {
                        *slot = Some((code, results));
                    }
                },
            )
        };

        {
            let failure = failure.clone();
            self.connection.call(
                Some(PORTAL_BUS_NAME),
                PORTAL_OBJECT_PATH,
                IFACE_REMOTE_DESKTOP,
                method,
                Some(&params),
                None,
                gio::DBusCallFlags::NONE,
                CALL_TIMEOUT_MS,
                gio::Cancellable::NONE,
                move |result| {
                    if let Err(err) = result {
                        if let Ok(mut slot) = failure.lock() {
                            *slot = Some(err.message().to_string());
                        }
                    }
                },
            );
        }

        // Both answers - the call's own reply and the signal carrying the result
        // - are delivered on this thread's context, so pumping it is what waits.
        let context = glib::MainContext::ref_thread_default();
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let outcome = loop {
            context.iteration(false);
            let answered = response.lock().ok().and_then(|mut slot| slot.take());
            let broken = failure.lock().ok().and_then(|mut slot| slot.take());
            if let Some((code, results)) = answered {
                break match code {
                    // 0 is "the user said yes".
                    0 => Ok(results),
                    // 1 is the user saying no - the one answer only a person can
                    // give, and the one that is remembered.
                    1 => Err(Failure::new(
                        Reason::Denied,
                        format!(
                            "'{method}' was cancelled: auto-paste stays off until you allow it, \
                             run 'clipnest paste-access'"
                        ),
                    )),
                    other => Err(self.refusal(method, other)),
                };
            }
            if let Some(err) = broken {
                break Err(classify_call_error(method, &err, || {
                    screen_locked(&self.connection)
                }));
            }
            if Instant::now() >= deadline {
                break Err(Failure::new(
                    Reason::Broken,
                    format!("'{method}' timed out"),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        // The subscription dies with this binding, so it must not be dropped
        // before the answer is in.
        drop(subscription);
        outcome
    }

    /// Why the portal said no.
    ///
    /// The protocol carries a response code and nothing else, and the most
    /// common reason is one the user can fix in a second - if it is named.
    /// GNOME refuses to hand out a remote-desktop session while the lock screen
    /// is up, which is also the state in which nobody could have clicked an
    /// entry anyway.
    fn refusal(&self, method: &str, code: u32) -> Failure {
        classify_refusal(method, code, || screen_locked(&self.connection))
    }

    /// The caller's unique bus name in the spelling the portal uses inside
    /// object paths: `:1.42` becomes `1_42`.
    fn sender(&self) -> String {
        self.connection
            .unique_name()
            .map(|name| name.trim_start_matches(':').replace('.', "_"))
            .unwrap_or_else(|| "anonymous".to_string())
    }

    /// A token for one call. Unique within the process, which is what the
    /// protocol asks for.
    fn new_token(&self) -> String {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        format!(
            "clipnest{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }
}

/// Why the portal refused a call with a response code.
///
/// `locked` is a closure rather than a value because asking the shell whether the
/// screen is locked costs a bus round trip, and it only ever needs asking when the
/// answer would change the classification.
fn classify_refusal(method: &str, code: u32, locked: impl FnOnce() -> Option<bool>) -> Failure {
    if locked() == Some(true) {
        return Failure::new(
            Reason::Locked,
            format!(
                "'{method}' was refused ({code}): the screen is locked, and GNOME refuses \
                 synthetic input while it is. Unlock and try again - nothing is lost, the \
                 next attempt asks again."
            ),
        );
    }
    // Every other response code is the protocol's "something else went wrong",
    // which on this desktop is most often a backend that does not offer a
    // keyboard session at all.
    Failure::new(
        Reason::Broken,
        format!(
            "'{method}' was refused (portal response {code}); \
             'clipnest doctor' reports what is available"
        ),
    )
}

/// Turns a D-Bus failure into something worth reading, with the reason that can
/// be acted on.
fn classify_call_error(
    method: &str,
    message: &str,
    locked: impl FnOnce() -> Option<bool>,
) -> Failure {
    let missing = message.contains("not provided by any .service files")
        || message.contains("NameHasNoOwner")
        || message.contains("ServiceUnknown");
    if missing {
        return Failure::new(
            Reason::Unavailable,
            format!(
                "{method}: the desktop portal is not running. Auto-paste needs \
                 xdg-desktop-portal with a backend that supports RemoteDesktop (installed \
                 with GNOME by default)"
            ),
        );
    }
    // A portal that is running but whose backend never implemented RemoteDesktop
    // answers `UnknownMethod` for the whole interface. There is nothing to
    // repair, the fix is a package, and naming it is the whole value of this
    // branch - so it must not be reported as a broken session that a restart
    // would fix. (Seen for real on a session whose only backend was portal-gtk.)
    if message.contains("RemoteDesktop") && message.contains("UnknownMethod")
        || message.contains("No such interface")
    {
        return Failure::new(
            Reason::Unavailable,
            format!(
                "{method}: this portal has no backend that can type into other windows. \
                 Install xdg-desktop-portal-gnome (or the portal backend for your desktop) \
                 ({message})"
            ),
        );
    }
    // GNOME answers "Session creation inhibited" while the lock screen is up, and
    // says it as a call error rather than a response code. Calling that "locked"
    // is not a guess: the shell is asked, and only when the wording fits.
    if looks_like_a_lock_refusal(message) && locked() == Some(true) {
        return Failure::new(
            Reason::Locked,
            format!(
                "{method} was refused while the screen is locked: unlock it and try again \
                 ({message})"
            ),
        );
    }
    Failure::new(Reason::Broken, format!("{method} failed: {message}"))
}

/// Whether a portal error reads like the lock screen refusing synthetic input.
fn looks_like_a_lock_refusal(message: &str) -> bool {
    let lowered = message.to_lowercase();
    lowered.contains("inhibit")
        || lowered.contains("not allowed")
        || lowered.contains("denied")
        || lowered.contains("locked")
}

/// `org.gnome.ScreenSaver.GetActive`, the one honest way to ask whether the
/// session is locked without reading its logs.
///
/// `None` means the question could not be put - no shell on the bus, or an
/// answer that is not a boolean - which is different from "not locked".
fn screen_locked(connection: &gio::DBusConnection) -> Option<bool> {
    let reply = connection
        .call_sync(
            Some("org.gnome.ScreenSaver"),
            "/org/gnome/ScreenSaver",
            "org.gnome.ScreenSaver",
            "GetActive",
            None,
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
        .ok()?;
    reply.child_value(0).get::<bool>()
}

fn object_path(value: &str) -> Result<ObjectPath, Failure> {
    ObjectPath::try_from(value).map_err(|_| {
        Failure::new(Reason::Broken, format!("'{value}' is not an object path"))
    })
}

/// The entries of a D-Bus dictionary, with each value unwrapped from its variant.
fn dict_entries(dict: &glib::Variant) -> HashMap<String, glib::Variant> {
    let mut entries = HashMap::new();
    for index in 0..dict.n_children() {
        let entry = dict.child_value(index);
        let key = entry.child_value(0);
        let Some(key) = key.str() else {
            continue;
        };
        // `a{sv}` wraps every value in a variant, and the unwrapping is what
        // makes `results["session_handle"]` an object path rather than a box.
        entries.insert(key.to_string(), entry.child_value(1).child_value(0));
    }
    entries
}

fn dict_string(dict: &HashMap<String, glib::Variant>, key: &str) -> Option<String> {
    dict.get(key).and_then(|value| value.str()).map(str::to_string)
}

// --------------------------------------------------------------------------
// The restore token
// --------------------------------------------------------------------------

/// Where the portal's "you already allowed this" token is kept.
///
/// `XDG_DATA_HOME` is honoured, so a session that keeps its data elsewhere keeps
/// this with it.
pub fn token_path() -> PathBuf {
    install::data_home().join("clipnest/portal-restore-token")
}

fn load_token() -> Option<String> {
    load_token_from(&token_path())
}

/// The mode a secret file has to have. Anything a group or another user can read
/// is one `cat` away from being used to type into this session.
pub const SECRET_MODE: u32 = 0o600;
/// The mode the directory holding it has to have.
pub const SECRET_DIR_MODE: u32 = 0o700;

/// What the token file looks like right now. Used by `doctor`, which asks the
/// question a user cannot: is the file the portal's answer is kept in readable by
/// anybody else?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenState {
    /// No token yet: auto-paste has simply not been allowed.
    Missing,
    /// Present, and readable only by its owner.
    Private,
    /// Present, and readable by somebody else.
    Exposed(u32),
    /// There, but not readable: a broken file, or a directory we cannot enter.
    Unreadable(String),
}

pub fn token_state() -> TokenState {
    token_state_at(&token_path())
}

fn token_state_at(path: &Path) -> TokenState {
    let Ok(metadata) = std::fs::metadata(path) else {
        return TokenState::Missing;
    };
    if !metadata.is_file() {
        return TokenState::Unreadable("not a regular file".to_string());
    }
    if std::fs::read_to_string(path).is_err() {
        return TokenState::Unreadable("cannot be read".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return TokenState::Exposed(mode);
        }
    }
    TokenState::Private
}

/// Deletes the token, which is what "forget that I allowed pasting" means.
///
/// Only the token: the history is the user's data and lives in another file, and
/// a permission that can be revoked should never be revocable *by losing the
/// clipboard history*.
/// Returns whether there was anything to remove.
pub fn forget_token() -> Result<bool, String> {
    forget_token_at(&token_path())
}

/// The path-taking half, so the tests can work in a sandbox instead of on the
/// real token.
fn forget_token_at(path: &Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("cannot remove {}: {err}", path.display())),
    }
}

/// Tightens the permissions of the token and its directory, for the case where
/// an earlier version (or a careless `cp`) left them open.
pub fn tighten_token() -> Result<TokenState, String> {
    let path = token_path();
    let state = token_state_at(&path);
    if let Some(parent) = path.parent() {
        set_dir_mode(parent).map_err(|err| format!("cannot secure {}: {err}", parent.display()))?;
    }
    if matches!(state, TokenState::Exposed(_)) {
        // Rewriting is what fixes it: the file is ours and its contents are
        // exactly what a fresh write would produce.
        match load_token_from(&path) {
            Some(token) => save_token_to(&path, &token)
                .map_err(|err| format!("cannot rewrite {}: {err}", path.display()))?,
            None => {
                std::fs::remove_file(&path)
                    .map_err(|err| format!("cannot remove {}: {err}", path.display()))?;
            }
        }
    }
    Ok(token_state_at(&path))
}

/// Makes the directory that holds secrets readable only by its owner. A
/// no-op on anything that is not Unix.
fn set_dir_mode(dir: &Path) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(dir)?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(SECRET_DIR_MODE))?;
        }
    }
    Ok(())
}

/// Saves the token, complaining out loud when it cannot.
///
/// The failure mode to avoid is silence: a token that was not saved means the
/// permission dialog comes back at the next login, and without a word in the log
/// that looks like the tool forgetting a grant it was given.
fn save_token(token: &str) {
    let path = token_path();
    if let Err(err) = save_token_to(&path, token) {
        eprintln!(
            "clipnest: cannot remember the paste permission in {}: {err}",
            path.display()
        );
        eprintln!("         the permission dialog will appear again next time");
    }
}

fn load_token_from(path: &Path) -> Option<String> {
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// Writes the token so a crash cannot leave a half-written one behind, and so
/// that the file it lands in is readable only by its owner.
///
/// The permissions are set on the temporary file *before* the rename: a mode set
/// afterwards would leave a window in which the secret exists with the umask's
/// permissions, and the rename is what makes the swap atomic either way.
fn save_token_to(path: &Path, token: &str) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the token path has no directory",
        ));
    };
    std::fs::create_dir_all(parent)?;
    set_dir_mode(parent)?;
    // Same directory, so the rename below is atomic rather than a copy.
    let temporary = parent.join(".clipnest-token.new");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(SECRET_MODE);
        }
        let mut file = options.open(&temporary)?;
        std::io::Write::write_all(&mut file, token.as_bytes())?;
        file.sync_all()?;
    }
    if let Err(err) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_and_the_reason_survive_the_wire() {
        // The CLI reads both back out of a D-Bus reply, so a spelling that
        // changed on one side only would silently downgrade a `denied` to an
        // `unknown` - the difference between "ask again" and "no idea".
        for reason in [
            Reason::Unknown,
            Reason::Unavailable,
            Reason::Denied,
            Reason::Locked,
            Reason::Broken,
        ] {
            assert_eq!(Reason::parse(reason.as_str()), reason);
        }
        for state in [State::Idle, State::Asking, State::Ready, State::Failed] {
            assert_eq!(State::parse(state.as_str()), state);
        }
        // A daemon from another build must not be able to put words in the
        // CLI's mouth: an unknown spelling is `Unknown`, not something worse.
        assert_eq!(Reason::parse("nonsense"), Reason::Unknown);
        assert_eq!(Reason::parse(""), Reason::Unknown);
    }

    #[test]
    fn every_reason_worth_reporting_says_what_to_do() {
        // A reported failure with nothing to do about it is a dead end, so the
        // four actionable reasons all carry a next step. `Unknown` is the one
        // exception: it is what an unrecognised word parses to.
        for reason in [
            Reason::Unavailable,
            Reason::Denied,
            Reason::Locked,
            Reason::Broken,
        ] {
            assert!(!reason.advice().is_empty(), "{reason:?} needs advice");
        }
        assert!(Reason::Unknown.advice().is_empty());
    }

    #[test]
    fn a_refusal_while_the_screen_is_locked_says_so() {
        let locked = classify_refusal("CreateSession", 2, || Some(true));
        assert_eq!(locked.reason, Reason::Locked);
        assert!(locked.note.contains("locked"));

        let unlocked = classify_refusal("CreateSession", 2, || Some(false));
        assert_eq!(unlocked.reason, Reason::Broken);

        // Not being able to ask is not the same answer as "not locked", so an
        // unavailable shell must not turn into a confident claim either way.
        let unknown = classify_refusal("CreateSession", 2, || None);
        assert_eq!(unknown.reason, Reason::Broken);
    }

    #[test]
    fn names_the_portal_that_cannot_type() {
        // Verbatim from a real session: portal-gtk was the only backend, so the
        // RemoteDesktop interface simply was not there. Reported as a broken
        // session it would send the user off to restart a daemon that is fine.
        let no_backend = classify_call_error(
            "CreateSession",
            "GDBus.Error:org.freedesktop.DBus.Error.UnknownMethod: No such interface \
             “org.freedesktop.portal.RemoteDesktop” on object at path \
             /org/freedesktop/portal/desktop",
            || None,
        );
        assert_eq!(no_backend.reason, Reason::Unavailable);
        assert!(no_backend.note.contains("xdg-desktop-portal-gnome"));

        let no_portal = classify_call_error(
            "CreateSession",
            "The name org.freedesktop.portal.Desktop was not provided by any .service files",
            || None,
        );
        assert_eq!(no_portal.reason, Reason::Unavailable);

        // The locked case arrives as a call error too, and only here does the
        // shell get asked at all.
        let inhibited = classify_call_error(
            "CreateSession",
            "GDBus.Error:org.freedesktop.DBus.Error.Failed: Session creation inhibited",
            || Some(true),
        );
        assert_eq!(inhibited.reason, Reason::Locked);

        // The same words with an unlocked screen are a genuine failure, not a
        // lock refusal in disguise.
        let other = classify_call_error("Start", "something else went wrong", || Some(true));
        assert_eq!(other.reason, Reason::Broken);
    }

    #[test]
    fn builds_the_option_dictionary_the_portal_expects() {
        // `a{sv}` is the signature of every options argument in this protocol,
        // and it is easy to get wrong: a mistake here is a call the portal never
        // accepts, with no hint about which field was wrong.
        let mut options: HashMap<String, glib::Variant> = HashMap::new();
        options.insert("handle_token".to_string(), "clipnest1_0".to_variant());
        options.insert("types".to_string(), DEVICE_KEYBOARD.to_variant());
        let built = options.to_variant();
        assert_eq!(built.type_().as_str(), "a{sv}");
        // The values keep their own types inside it.
        assert_eq!(
            dict_entries(&built).get("types").and_then(|v| v.get::<u32>()),
            Some(DEVICE_KEYBOARD)
        );
    }

    #[test]
    fn marshals_each_call_the_way_the_protocol_documents_it() {
        let session = object_path("/org/freedesktop/portal/desktop/session/1_2/s").unwrap();
        let options: HashMap<String, glib::Variant> = HashMap::new();
        let signature = |value: glib::Variant| value.type_().as_str().to_string();
        assert_eq!(
            signature((options.clone(),).to_variant()),
            "(a{sv})",
            "CreateSession takes just the options"
        );
        assert_eq!(
            signature((session.clone(), options.clone()).to_variant()),
            "(oa{sv})",
            "SelectDevices takes the session and the options"
        );
        assert_eq!(
            signature((session.clone(), String::new(), options).to_variant()),
            "(osa{sv})",
            "Start adds the parent window"
        );
        assert_eq!(
            signature(
                (
                    session,
                    HashMap::<String, glib::Variant>::new(),
                    0x76i32,
                    PRESSED
                )
                    .to_variant()
            ),
            "(oa{sv}iu)",
            "NotifyKeyboardKeysym takes a keysym and a state"
        );
    }

    #[test]
    fn reads_a_result_back_out_of_a_response() {
        // The portal answers with `a{sv}`, and the value is wrapped in a variant
        // that has to be unwrapped again - the one place where a wrong guess
        // would silently produce an empty string instead of a session path.
        let mut results: HashMap<String, glib::Variant> = HashMap::new();
        results.insert(
            "session_handle".to_string(),
            "/org/freedesktop/portal/desktop/session/1_42/clipnest1_0".to_variant(),
        );
        results.insert("restore_token".to_string(), "abc123".to_variant());
        results.insert("devices".to_string(), 1u32.to_variant());

        let built = results.to_variant();
        let read = dict_entries(&built);
        assert_eq!(
            dict_string(&read, "session_handle").as_deref(),
            Some("/org/freedesktop/portal/desktop/session/1_42/clipnest1_0")
        );
        assert_eq!(dict_string(&read, "restore_token").as_deref(), Some("abc123"));
        assert_eq!(dict_string(&read, "missing"), None);
        assert_eq!(read.get("devices").and_then(|value| value.get::<u32>()), Some(1));
    }

    #[test]
    fn parses_the_configured_shortcut() {
        let accel = Accelerator::parse("<Control>v").unwrap();
        assert_eq!(accel.describe(), "<ctrl>v");
        assert_eq!(
            accel.sequence(),
            vec![
                (0xffe3, PRESSED),
                (0x76, PRESSED),
                (0x76, RELEASED),
                (0xffe3, RELEASED),
            ]
        );
    }

    #[test]
    fn presses_modifiers_first_and_releases_them_last() {
        // The order matters to the application that receives this: a modifier
        // released before its key turns the shortcut into plain text.
        let accel = Accelerator::parse("<Shift><Super>v").unwrap();
        assert_eq!(accel.describe(), "<shift><super>v");
        let sequence = accel.sequence();
        assert_eq!(
            sequence,
            vec![
                (0xffe1, PRESSED),  // Shift
                (0xffeb, PRESSED),  // Super
                (0x76, PRESSED),    // v
                (0x76, RELEASED),   // v
                (0xffeb, RELEASED), // Super
                (0xffe1, RELEASED), // Shift
            ]
        );
        // Two modifiers written in the other order are the same shortcut.
        assert_eq!(
            Accelerator::parse("<Super><Shift>v").unwrap().sequence(),
            sequence
        );
    }

    #[test]
    fn refuses_a_shortcut_that_could_never_work() {
        // Same rules as the keybinding command: a binding GNOME would reject is
        // not one this daemon should pretend to be able to type.
        for binding in ["", "Control+v", "<Bogus>v", "<Control>", "<Control>vg"] {
            assert!(
                Accelerator::parse(binding).is_err(),
                "'{binding}' should be refused"
            );
        }
        assert!(Accelerator::parse("F9").unwrap().sequence().len() == 2);
        assert!(Accelerator::parse("<Control>Return").is_ok());
    }

    /// The first half of the handshake against the desktop this is running on.
    ///
    /// `CreateSession` and `SelectDevices` are the calls a backend can answer
    /// without asking anybody anything, which makes them the part worth checking
    /// automatically: the object paths, the option dictionaries and the response
    /// signal. `Start` is deliberately *not* called - it opens a permission
    /// dialog, and a test suite is no place for one.
    ///
    /// Run it by hand where a session bus and a portal exist:
    ///
    /// ```text
    /// cargo test --offline -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the real session bus and a portal backend"]
    fn the_portal_answers_without_a_dialog() {
        let context = glib::MainContext::new();
        let _ = context.with_thread_default(|| {
            let client = match Client::connect() {
                Ok(client) => client,
                Err(err) => panic!("no session bus: {err}"),
            };
            let session = match client.create_session() {
                Ok(session) => session,
                Err(err) => panic!("CreateSession failed: {err}"),
            };
            println!("session: {session}");
            assert!(session.starts_with("/org/freedesktop/portal/desktop/session/"));
            match client.select_devices(&session) {
                Ok(()) => println!("SelectDevices: the keyboard was accepted"),
                Err(err) => panic!("SelectDevices failed: {err}"),
            }
            // Leave nothing behind for the next run to trip over.
            let _ = client.connection.call_sync(
                Some(PORTAL_BUS_NAME),
                &session,
                "org.freedesktop.portal.Session",
                "Close",
                None,
                None,
                gio::DBusCallFlags::NONE,
                CALL_TIMEOUT_MS,
                gio::Cancellable::NONE,
            );
        });
    }

    #[test]
    fn a_token_round_trips_through_its_file() {
        // The path is passed in on purpose: the environment is shared by every
        // test in the process, so pointing the real one at a sandbox would be a
        // race against whatever else is running.
        let base = std::env::temp_dir().join(format!(
            "clipnest-token-{}-{}",
            std::process::id(),
            crate::db::now_ms()
        ));
        let path = base.join("clipnest/portal-restore-token");

        // Nothing to restore yet is not an error.
        assert_eq!(load_token_from(&path), None);
        save_token_to(&path, "token-1").unwrap();
        assert_eq!(load_token_from(&path).as_deref(), Some("token-1"));
        // Whitespace from the file is not part of the token.
        std::fs::write(&path, "  token-2 \n").unwrap();
        assert_eq!(load_token_from(&path).as_deref(), Some("token-2"));
        // An empty file means "no token", not an empty token.
        std::fs::write(&path, "\n").unwrap();
        assert_eq!(load_token_from(&path), None);
        // A later grant replaces the earlier one.
        save_token_to(&path, "token-3").unwrap();
        assert_eq!(load_token_from(&path).as_deref(), Some("token-3"));
        // Nothing but the token itself is left behind by the atomic write.
        let leftovers: Vec<_> = std::fs::read_dir(base.join("clipnest"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(leftovers, vec!["portal-restore-token".to_string()]);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The token is a permission to type into this session. A file any user on
    /// the machine can read is a permission anybody can borrow.
    #[test]
    fn the_token_is_private() {
        let base = std::env::temp_dir().join(format!(
            "clipnest-token-mode-{}-{}",
            std::process::id(),
            crate::db::now_ms()
        ));
        let path = base.join("clipnest/portal-restore-token");

        save_token_to(&path, "token").unwrap();
        assert_eq!(token_state_at(&path), TokenState::Private);

        // An older version, or a careless `cp`, can leave it wide open; there is
        // no silent write to fix that, so the repair has to be asked for.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(token_state_at(&path), TokenState::Exposed(0o644));
            // Rewriting is the repair: the temporary file is created with the
            // right mode before the rename, so there is no open window.
            save_token_to(&path, "token").unwrap();
            assert_eq!(token_state_at(&path), TokenState::Private);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, SECRET_MODE, "token mode should be 0600, was {mode:o}");
            let dir_mode = std::fs::metadata(base.join("clipnest"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, SECRET_DIR_MODE);
        }

        // A file that is not there is a state of its own, not an error: it just
        // means auto-paste has not been allowed yet.
        assert_eq!(token_state_at(&base.join("clipnest/nothing")), TokenState::Missing);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn forgetting_the_permission_leaves_the_history_alone() {
        // The one thing a reset must never do: take the clipboard history with
        // it. They are different files, and this test is what keeps them apart.
        let base = std::env::temp_dir().join(format!(
            "clipnest-token-forget-{}-{}",
            std::process::id(),
            crate::db::now_ms()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let token = base.join("clipnest/portal-restore-token");
        let history = base.join("clipnest/history.db");
        save_token_to(&token, "token").unwrap();
        std::fs::write(&history, "not really a database").unwrap();

        forget_token_at(&token).unwrap();
        assert_eq!(token_state_at(&token), TokenState::Missing);
        assert!(history.is_file(), "the history went with the token");
        // Forgetting twice is not an error, it is just nothing left to do.
        assert!(!forget_token_at(&token).unwrap());

        let _ = std::fs::remove_dir_all(&base);
    }
}
