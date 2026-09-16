/**
 * The decisions the extension makes, with no GNOME in them.
 *
 * Everything here is a plain function over plain values: no `St`, no `GLib`,
 * no shell objects. That is on purpose - these are the parts where a mistake
 * costs a lost clipboard entry or a busy loop, and outside `extension.js` they
 * can be tested directly (see `extension/tests/units.js`, run by `make
 * test-extension`).
 *
 * The one thing the caller has to supply is the hash: gnome-shell gets
 * `GLib.compute_checksum_for_data`, the tests pass something small.
 */

/** How much of a large image is hashed to notice a repeat cheaply. */
export const FINGERPRINT_BYTES = 64 * 1024;

/** The lowest poll interval the extension will use, whatever the file says. */
export const MIN_POLL_INTERVAL_MS = 100;

/**
 * Up to this size an image is hashed as a whole, which cannot collide: two
 * different payloads of the same length can share their first and last 64 KiB,
 * and treating those as one entry would silently drop a copy.
 *
 * Above it the fingerprint keeps three slices (head, middle, tail) instead of
 * two. A transfer only happens at most once per poll, so the cost of hashing
 * 192 KiB per poll is dwarfed by the transfer itself; the ceiling exists to
 * keep the shell's per-poll work bounded, not because hashing is slow.
 */
export const FULL_HASH_LIMIT = 1024 * 1024;

/** The key for a text payload: the length plus a hash of the whole thing. */
export function textKey(text, checksum) {
    return `text:${text.length}:${checksum(text)}`;
}

/**
 * The key for an image: identical payloads must produce the same key (so a
 * repeat is not sent twice), and different payloads must not (so a copy is
 * never lost).
 */
export function imageKey(data, checksum, limits = {}) {
    const fullHashLimit = limits.fullHashLimit ?? FULL_HASH_LIMIT;
    const sliceBytes = limits.fingerprintBytes ?? FINGERPRINT_BYTES;
    if (data.length <= fullHashLimit) {
        return `image:full:${data.length}:${checksum(data)}`;
    }
    const last = data.length - sliceBytes;
    const middle = (last >> 1) - (sliceBytes >> 1);
    const head = data.subarray(0, sliceBytes);
    const middleSlice = data.subarray(middle, middle + sliceBytes);
    const tail = data.subarray(last);
    return `image:fast:${data.length}` +
        `:${checksum(head)}:${checksum(middleSlice)}:${checksum(tail)}`;
}

/**
 * Which image type to ask the clipboard for, most portable first.
 *
 * The order matters: the daemon stores whatever it is handed and pastes it back
 * byte for byte, so asking for PNG when the owner has one keeps a screenshot
 * lossless.
 */
export function pickImageMimetype(mimetypes, preferred) {
    for (const type of preferred) {
        if (mimetypes.includes(type))
            return type;
    }
    return mimetypes.find(type => type.startsWith('image/')) ?? null;
}

/**
 * The settings the daemon reports, applied over the ones in force.
 *
 * A missing or unusable value keeps what was there: the daemon started lazily,
 * so the first answers may well be incomplete, and a half-read answer must not
 * quietly disable clipboard capture. A poll interval below the floor is refused
 * for the same reason the daemon enforces one - this timer runs inside
 * gnome-shell.
 */
export function applyConfig(current, values, floor = MIN_POLL_INTERVAL_MS) {
    const next = {...current};
    if (Number.isFinite(values.max_text_bytes))
        next.maxTextBytes = values.max_text_bytes;
    if (Number.isFinite(values.max_image_bytes))
        next.maxImageBytes = values.max_image_bytes;
    if (Number.isFinite(values.max_items))
        next.maxItems = values.max_items;
    const interval = values.poll_interval_ms;
    if (Number.isFinite(interval) && interval >= floor)
        next.pollIntervalMs = interval;
    next.loaded = true;
    return next;
}

/**
 * Whether a transfer that was started is still worth waiting for.
 *
 * `St.Clipboard` has no changed signal, so this is a poll loop; a callback that
 * never arrives (a hung owner, a cancelled paste) would otherwise stop capture
 * for the rest of the session.
 */
export function readStalled(reading, startedAtMicros, nowMicros, timeoutMs) {
    if (!reading)
        return false;
    return (nowMicros - startedAtMicros) / 1000 >= timeoutMs;
}

/**
 * `St.Clipboard.get_content` hands back a `GLib.Bytes` in some shell versions
 * and an already unwrapped `Uint8Array` in others.
 */
export function toByteArray(value) {
    if (!value)
        return null;
    if (value instanceof Uint8Array)
        return value;
    if (typeof value.get_data === 'function')
        return value.get_data();
    if (typeof value.toArray === 'function')
        return value.toArray();
    return null;
}

/** Whether a payload of this size is worth sending at all. */
export function withinLimit(length, limit) {
    return limit > 0 && length > 0 && length <= limit;
}
