import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

// The decisions themselves live in `./lib/clipboard.js`, without a single GNOME
// import, so they can be tested outside the shell. What is left here is the part
// that genuinely needs gnome-shell: the clipboard object, the timer and D-Bus.
import {
    applyConfig,
    imageKey,
    pickImageMimetype,
    readStalled,
    textKey,
    toByteArray,
    withinLimit,
} from './lib/clipboard.js';

const BUS_NAME = 'dev.clipnest.Daemon';
const OBJECT_PATH = '/dev/clipnest/Daemon';
const INTERFACE = 'dev.clipnest.Daemon';
const POLL_INTERVAL_MS = 400;
/** A transfer that never answers must not stall the poller forever. */
const READ_TIMEOUT_MS = 3000;
const MAX_TEXT_BYTES = 1024 * 1024;
const MAX_IMAGE_BYTES = 8 * 1024 * 1024;
/**
 * The two sizes above are fallbacks: they are the daemon's own defaults, and
 * they are only used until the daemon answers. The real numbers come from the
 * settings file, through the `Config` call below - a setting that could not
 * change what this extension sends would be a lie, because an image the
 * extension drops never reaches the daemon that would have accepted it.
 */
/** How often to ask for the settings until the daemon answers. */
const CONFIG_RETRY_TICKS = 5;
/** How often to ask again afterwards, so an edit takes effect within a minute. */
const CONFIG_REFRESH_TICKS = 150;

/** Mimetypes St.Clipboard.get_text() can hand back. */
const TEXT_MIMETYPES = ['text/plain;charset=utf-8', 'UTF8_STRING', 'text/plain', 'STRING'];
/** Images we would rather store, most portable first. */
const IMAGE_MIMETYPES = ['image/png', 'image/jpeg', 'image/webp', 'image/tiff', 'image/bmp'];

/**
 * On Wayland the clipboard is owned by the compositor, and Mutter implements no
 * data-control protocol, so a background process cannot read it. This extension
 * lives inside gnome-shell, which does own the selection, and forwards every
 * change to the ClipNest daemon over D-Bus.
 *
 * Two things are easy to get wrong here:
 *
 * 1. `St.Clipboard` has no `changed` signal, so we poll. Its read methods are
 *    plain callback APIs — `get_text(type, callback)` and
 *    `get_content(type, mimetype, callback)` — and **there is no
 *    `get_text_finish`/`get_content_finish`** in GJS (only the C
 *    `StClipboardCallbackFunc` is exported). Calling a `_finish` method throws,
 *    so the callback arguments have to be used directly.
 * 2. A callback read may never answer (a hung owner, a paste that was cancelled).
 *    `_reading` therefore carries a timestamp: without it one stuck transfer
 *    would disable clipboard capture for the rest of the session.
 * 3. How big a payload may be, and how often to look, are the daemon's settings
 *    rather than this file's constants. `_loadConfig` asks for them over D-Bus
 *    and keeps asking until the daemon answers, because it is started lazily.
 */
export default class ClipNestExtension extends Extension {
    enable() {
        this._clipboard = St.Clipboard.get_default();
        this._bus = Gio.DBus.session;
        this._lastKey = null;
        this._reading = false;
        this._readStartedAt = 0;
        this._warnedDaemon = false;
        this._enabled = true;
        // The fallbacks are the daemon's own defaults; the real numbers arrive
        // through `_loadConfig`. They live in one object now so the pure
        // `applyConfig` can update them as a whole.
        this._settings = {
            maxTextBytes: MAX_TEXT_BYTES,
            maxImageBytes: MAX_IMAGE_BYTES,
            maxItems: 0,
            pollIntervalMs: POLL_INTERVAL_MS,
            loaded: false,
        };
        this._configTicks = 0;
        this._armTimer();
        // The daemon is started by the first copy, so at login it may well not be
        // there yet; `_poll` keeps asking until it answers.
        this._loadConfig();
    }

    _armTimer() {
        if (this._timerId)
            GLib.source_remove(this._timerId);
        this._timerId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT,
            this._settings.pollIntervalMs,
            () => {
                this._poll();
                return GLib.SOURCE_CONTINUE;
            });
    }

    disable() {
        // A read already in flight will still call back; the nulls below are why
        // every callback checks `this._enabled` before touching anything.
        this._enabled = false;
        if (this._timerId) {
            GLib.source_remove(this._timerId);
            this._timerId = 0;
        }
        this._clipboard = null;
        this._bus = null;
        this._lastKey = null;
        this._reading = false;
        this._settings.loaded = false;
    }

    /**
     * Asks the daemon for the settings that decide what gets sent, and for how
     * often to look. Sizes arrive in bytes and the interval in milliseconds;
     * whatever is missing keeps its fallback.
     */
    _loadConfig() {
        if (!this._bus || !this._enabled)
            return;
        try {
            this._bus.call(
                BUS_NAME, OBJECT_PATH, INTERFACE, 'Config', null, null,
                Gio.DBusCallFlags.NONE, 2000, null,
                (connection, result) => {
                    if (!this._bus || !this._enabled)
                        return;
                    let pairs;
                    try {
                        // One out-argument: an array of [key, value] pairs.
                        [pairs] = connection.call_finish(result).deepUnpack();
                    } catch (e) {
                        // The daemon is not up yet; another attempt is scheduled.
                        return;
                    }
                    const values = {};
                    for (const [key, value] of pairs)
                        values[key] = Number.parseInt(value, 10);
                    this._applyConfig(values);
                });
        } catch (e) {
            logError(e, 'ClipNest: cannot ask the daemon for its settings');
        }
    }

    _applyConfig(values) {
        const before = this._settings;
        this._settings = applyConfig(before, values);
        if (this._settings.pollIntervalMs !== before.pollIntervalMs)
            this._armTimer();
    }

    _poll() {
        if (!this._enabled || !this._clipboard || !this._bus)
            return;

        this._configTicks += 1;
        const due = this._settings.loaded ? CONFIG_REFRESH_TICKS : CONFIG_RETRY_TICKS;
        if (this._configTicks >= due) {
            this._configTicks = 0;
            this._loadConfig();
        }

        if (readStalled(this._reading, this._readStartedAt,
            GLib.get_monotonic_time(), READ_TIMEOUT_MS)) {
            this._reading = false;
        } else if (this._reading) {
            return;
        }

        let mimetypes = [];
        try {
            mimetypes = this._clipboard.get_mimetypes(St.ClipboardType.CLIPBOARD) ?? [];
        } catch (e) {
            return;
        }
        if (mimetypes.length === 0)
            return;

        // Text wins when the owner offers both: copying a cell out of a
        // spreadsheet or a link out of a browser should land as text, not as a
        // rendered image of it.
        if (this._settings.maxTextBytes > 0 &&
            mimetypes.some(type => TEXT_MIMETYPES.includes(type))) {
            this._readText();
            return;
        }

        // A size of zero means "do not record this kind", so there is no point
        // in transferring the payload at all.
        if (this._settings.maxImageBytes > 0) {
            const image = pickImageMimetype(mimetypes, IMAGE_MIMETYPES);
            if (image)
                this._readImage(image);
        }
    }

    _beginRead() {
        this._reading = true;
        this._readStartedAt = GLib.get_monotonic_time();
    }

    _endRead() {
        this._reading = false;
    }

    _readText() {
        this._beginRead();
        try {
            this._clipboard.get_text(St.ClipboardType.CLIPBOARD, (_clipboard, text) => {
                this._endRead();
                if (!this._enabled)
                    return;
                if (!text || !withinLimit(text.length, this._settings.maxTextBytes))
                    return;

                const key = textKey(text, this._checksum.bind(this));
                if (key === this._lastKey)
                    return;
                this._lastKey = key;
                this._send('PushText', new GLib.Variant('(s)', [text]));
            });
        } catch (e) {
            this._endRead();
            logError(e, 'ClipNest: reading the clipboard text failed');
        }
    }

    _readImage(mimetype) {
        this._beginRead();
        try {
            this._clipboard.get_content(St.ClipboardType.CLIPBOARD, mimetype,
                (_clipboard, bytes) => {
                    this._endRead();
                    if (!this._enabled)
                        return;
                    const data = toByteArray(bytes);
                    if (!data || !withinLimit(data.length, this._settings.maxImageBytes))
                        return;

                    const key = imageKey(data, this._checksum.bind(this));
                    if (key === this._lastKey)
                        return;
                    this._lastKey = key;
                    this._send('PushImage', new GLib.Variant('(say)', [mimetype, data]));
                });
        } catch (e) {
            this._endRead();
            logError(e, 'ClipNest: reading the clipboard image failed');
        }
    }

    /**
     * The hash everything else is built from. GNOME's own implementation when it
     * is there, a length-based fallback when it is not - see `lib/clipboard.js`
     * for how the result is used, and why small images are hashed whole.
     */
    _checksum(value) {
        try {
            if (typeof value === 'string')
                return GLib.compute_checksum_for_string(GLib.ChecksumType.SHA256, value, -1);
            return GLib.compute_checksum_for_data(GLib.ChecksumType.SHA256, value);
        } catch (e) {
            // Cheap fallback: still good enough to detect a repeated change.
            return `len-${value.length}`;
        }
    }

    _send(method, params) {
        if (!this._enabled || !this._bus)
            return;
        try {
            this._bus.call(
                BUS_NAME, OBJECT_PATH, INTERFACE, method, params, null,
                Gio.DBusCallFlags.NONE, 2000, null,
                (connection, result) => {
                    if (!this._enabled)
                        return;
                    try {
                        connection.call_finish(result);
                        this._warnedDaemon = false;
                    } catch (e) {
                        // The daemon may simply not be up yet: forget the
                        // fingerprint so the next poll sends this payload again.
                        this._lastKey = null;
                        if (!this._warnedDaemon) {
                            this._warnedDaemon = true;
                            log(`ClipNest: cannot reach the daemon (${e.message}); ` +
                                'is clipnest.service running?');
                        }
                    }
                });
        } catch (e) {
            this._lastKey = null;
            logError(e, 'ClipNest: cannot talk to the daemon');
        }
    }
}
