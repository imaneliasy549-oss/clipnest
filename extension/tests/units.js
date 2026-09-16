/**
 * Unit tests for the clipboard logic the extension runs on every poll.
 *
 * These are the decisions that cost a lost copy when they are wrong, and they
 * used to be impossible to test: they lived inside a class that imports
 * gnome-shell. `make test-extension` runs this file with `gjs -m` (what the
 * extension actually runs on) or with node, whichever is installed.
 *
 *   gjs -m extension/tests/units.js
 */

import {
    FINGERPRINT_BYTES,
    FULL_HASH_LIMIT,
    applyConfig,
    imageKey,
    pickImageMimetype,
    readStalled,
    textKey,
    toByteArray,
    withinLimit,
} from '../lib/clipboard.js';

// `print` is a gjs global and node has no such thing, so the same file can be
// run by either one without a build step in between.
const log = typeof print === 'function' ? print : console.log;

let failures = 0;
let checks = 0;

function ok(condition, what) {
    checks += 1;
    if (!condition) {
        failures += 1;
        log(`  FAIL ${what}`);
    }
}

function eq(actual, expected, what) {
    ok(actual === expected, `${what}: expected ${expected}, got ${actual}`);
}

function ne(actual, unexpected, what) {
    ok(actual !== unexpected, `${what}: both were ${unexpected}`);
}

/** A deterministic hash, small enough to write by hand. */
function checksum(value) {
    const bytes = typeof value === 'string' ? [...value].map(c => c.codePointAt(0) % 251) : value;
    let hash = 2166136261;
    for (const byte of bytes) {
        hash ^= byte;
        hash = Math.imul(hash, 16777619) >>> 0;
    }
    return hash.toString(16).padStart(8, '0');
}

function bytes(length, fill = 0) {
    return new Uint8Array(length).fill(fill);
}

function section(name) {
    log(`# ${name}`);
}

// ---------------------------------------------------------------------------
section('the same payload is not sent twice');

const shot = bytes(4096, 7);
eq(imageKey(shot, checksum), imageKey(shot.slice(), checksum), 'a repeated image has one key');
eq(textKey('hello', checksum), textKey('hello', checksum), 'a repeated text has one key');
ne(textKey('hello', checksum), textKey('hello ', checksum), 'a longer text is a new entry');
// Otherwise unicode would be fingerprinted from its UTF-16 length while the
// daemon counts bytes, and two different strings could share a key.
ne(textKey('👋', checksum), textKey('a', checksum), 'emoji and ascii are told apart');

// ---------------------------------------------------------------------------
section('two different images are never treated as one');

// The bug this pins down: a small image used to be fingerprinted from its length
// plus its first and last 64 KiB, so two screenshots that differ only in the
// middle - a cursor moved, a line of a terminal changed - collided, and the
// second copy was dropped in silence.
const smallA = bytes(4096, 1);
const smallB = bytes(4096, 1);
smallB[2048] = 9;
eq(smallA.length, smallB.length, 'the two payloads are the same size');
eq(smallA[0], smallB[0], 'and share their first byte');
ne(imageKey(smallA, checksum), imageKey(smallB, checksum), 'the middle is compared too');

// Large images keep a cheaper fingerprint - that is the point of the ceiling -
// but the middle slice means the same trick no longer works at any size.
const bigA = bytes(FULL_HASH_LIMIT + 1024, 3);
const bigB = bytes(FULL_HASH_LIMIT + 1024, 3);
// The middle of the payload, which the old length + head + tail fingerprint
// never looked at.
bigB[Math.floor(bigB.length / 2) - FINGERPRINT_BYTES / 2] = 42;
ne(imageKey(bigA, checksum), imageKey(bigB, checksum), 'a large image is compared past its ends');
ok(imageKey(bigA, checksum).startsWith('image:fast:'), 'a large image uses the cheap fingerprint');
ok(imageKey(smallA, checksum).startsWith('image:full:'), 'a small image is hashed whole');

// The three slices are probed, so a change between them is still missed. Saying
// so out loud is the difference between a documented limit and a surprise; the
// daemon hashes every byte it is handed, so nothing is lost once sent.
const edgeA = bytes(FULL_HASH_LIMIT + 1024, 5);
const edgeB = bytes(FULL_HASH_LIMIT + 1024, 5);
const unwatched = FINGERPRINT_BYTES + 512;
edgeB[unwatched] = 77;
eq(
    imageKey(edgeA, checksum),
    imageKey(edgeB, checksum),
    'the fingerprint only promises three slices of a large payload'
);

// ---------------------------------------------------------------------------
section('which type to ask for');

eq(
    pickImageMimetype(['text/plain', 'image/png'], ['image/png', 'image/jpeg']),
    'image/png',
    'png wins when the owner has it (it is lossless)'
);
eq(
    pickImageMimetype(['image/jpeg', 'image/png'], ['image/png', 'image/jpeg']),
    'image/png',
    'the preferred order decides, not the owner answer order'
);
eq(
    pickImageMimetype(['image/x-something'], ['image/png']),
    'image/x-something',
    'an unknown image type is still better than nothing'
);
eq(pickImageMimetype(['text/plain'], ['image/png']), null, 'no image, nothing to read');

// ---------------------------------------------------------------------------
section('the settings the daemon reports');

const base = {
    maxTextBytes: 1024,
    maxImageBytes: 2048,
    maxItems: 0,
    pollIntervalMs: 400,
    loaded: false,
};

const partial = applyConfig(base, {max_text_bytes: 512});
eq(partial.maxTextBytes, 512, 'a reported size is applied');
eq(partial.maxImageBytes, 2048, 'a missing size keeps its fallback');
eq(partial.loaded, true, 'the first answer marks the settings as loaded');

const nonsense = applyConfig(base, {
    max_text_bytes: Number.NaN,
    max_image_bytes: undefined,
    poll_interval_ms: 10,
});
eq(nonsense.maxTextBytes, 1024, 'a value that is not a number changes nothing');
eq(nonsense.pollIntervalMs, 400, 'an interval below the floor is refused');

eq(
    applyConfig(base, {poll_interval_ms: 250}).pollIntervalMs,
    250,
    'a sane interval is taken'
);

// ---------------------------------------------------------------------------
section('a transfer that never answers');

ok(readStalled(false, 0, 10_000_000, 3000) === false, 'nothing in flight, nothing to wait for');
ok(readStalled(true, 0, 1_000_000, 3000) === false, 'still inside the timeout');
ok(readStalled(true, 0, 3_000_000, 3000) === true, 'past the timeout the poller resumes');

// ---------------------------------------------------------------------------
section('unwrapping what the clipboard hands back');

const raw = bytes(8, 1);
eq(toByteArray(raw), raw, 'a Uint8Array is passed through');
eq(toByteArray({get_data: () => raw}), raw, 'GLib.Bytes is unwrapped');
eq(toByteArray({toArray: () => raw}), raw, 'other wrappers are unwrapped');
eq(toByteArray(null), null, 'nothing stays nothing');

// ---------------------------------------------------------------------------
section('sizes that are not worth sending');

ok(withinLimit(10, 100), 'a normal payload is sent');
ok(!withinLimit(10, 0), 'a limit of zero means the kind is off');
ok(!withinLimit(0, 100), 'an empty payload is not an entry');
ok(!withinLimit(101, 100), 'one byte over is over');

log('');
if (failures === 0) {
    log(`All ${checks} extension checks passed.`);
} else {
    log(`${failures} of ${checks} extension checks failed.`);
    // `import.meta` is not available in a plain gjs script, so the exit code is
    // set the portable way: an uncaught throw.
    throw new Error(`${failures} extension checks failed`);
}
