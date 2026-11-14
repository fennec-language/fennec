//! `Argon2id`, as RFC 9106 describes it, for `crypto.hash_password` and `crypto.check_password`: a
//! password stretched through memory, which a guess has to pay for in memory too. A hash is kept as
//! a PHC string, `$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`, so its settings and salt travel
//! with it and can be raised later.
//!
//! The other two kinds, `Argon2d` and `Argon2i`, are here only so RFC 9106's vectors for them check
//! the parts all three share.

use super::blake2b;

/// Words in a block of Argon2's memory: 1 KiB.
const WORDS: usize = 128;
type Block = [u64; WORDS];
/// Each pass over a lane is filled in four slices, the points lanes meet at.
const SLICES: usize = 4;
const VERSION: u32 = 0x13;

/// Which of Argon2's kinds: how a block picks the earlier block it's made with.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), expect(dead_code, reason = "Argon2d and Argon2i are only for RFC 9106's vectors"))]
pub(super) enum Kind {
    D,
    I,
    Id,
}

impl Kind {
    fn number(self) -> u32 {
        match self {
            Kind::D => 0,
            Kind::I => 1,
            Kind::Id => 2,
        }
    }
}

/// The settings a hash is made with: memory in KiB, passes, lanes, and the tag's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Settings {
    pub(super) memory: u32,
    pub(super) passes: u32,
    pub(super) lanes: u32,
    pub(super) tag: usize,
}

/// What `crypto.hash_password` uses: OWASP's floor for `Argon2id`, 19 MiB and two passes on one
/// lane, with a 32-byte tag.
pub(super) const DEFAULT: Settings = Settings { memory: 19_456, passes: 2, lanes: 1, tag: 32 };

/// The least a stored hash may have been made with to be checked: OWASP's floor. A weaker hash
/// is refused rather than quietly accepted.
const LEAST_MEMORY: u32 = 19_456;
const LEAST_PASSES: u32 = 2;
/// The most, so a stored hash can't make checking it take all the memory or time there is.
const MOST_MEMORY: u32 = 1 << 20;
const MOST_PASSES: u32 = 16;
const MOST_LANES: u32 = 16;
/// A salt's and a tag's lengths in bytes, as a check takes them.
const SALT_LENGTHS: std::ops::RangeInclusive<usize> = 8..=64;
const TAG_LENGTHS: std::ops::RangeInclusive<usize> = 16..=64;

fn le(number: u32) -> [u8; 4] {
    number.to_le_bytes()
}

fn le_size(size: usize) -> [u8; 4] {
    le(u32::try_from(size).unwrap_or(u32::MAX))
}

/// `H'`, the hash of any length Argon2 builds from `BLAKE2b`.
fn long_hash(size: usize, parts: &[&[u8]]) -> Vec<u8> {
    let mut first = blake2b::Hasher::new(size.min(64));
    first.update(&le_size(size));
    for part in parts {
        first.update(part);
    }
    if size <= 64 {
        return first.finish();
    }
    let rounds = size.div_ceil(32) - 2;
    let mut out = Vec::with_capacity(size);
    let mut last = first.finish();
    out.extend_from_slice(last.get(..32).unwrap_or_default());
    for _ in 1..rounds {
        last = blake2b::digest(64, &last);
        out.extend_from_slice(last.get(..32).unwrap_or_default());
    }
    out.extend_from_slice(&blake2b::digest(size - 32 * rounds, &last));
    out
}

/// `BlaMka`'s mixing: `BLAKE2b`'s, with a product of the low halves added in.
fn mix(work: &mut Block, places: [usize; 4]) {
    let multiplied = |x: u64, y: u64| {
        x.wrapping_add(y).wrapping_add(2u64.wrapping_mul(x & 0xffff_ffff).wrapping_mul(y & 0xffff_ffff))
    };
    let [Some(&one), Some(&two), Some(&three), Some(&four)] = places.map(|place| work.get(place)) else {
        return;
    };
    let one = multiplied(one, two);
    let four = (four ^ one).rotate_right(32);
    let three = multiplied(three, four);
    let two = (two ^ three).rotate_right(24);
    let one = multiplied(one, two);
    let four = (four ^ one).rotate_right(16);
    let three = multiplied(three, four);
    let two = (two ^ three).rotate_right(63);
    for (place, value) in places.into_iter().zip([one, two, three, four]) {
        if let Some(slot) = work.get_mut(place) {
            *slot = value;
        }
    }
}

/// One `BLAKE2b` round, without a message, on the sixteen words at `places`.
fn round(work: &mut Block, places: [usize; 16]) {
    let [p0, p1, p2, p3, p4, p5, p6, p7, p8, p9, p10, p11, p12, p13, p14, p15] = places;
    mix(work, [p0, p4, p8, p12]);
    mix(work, [p1, p5, p9, p13]);
    mix(work, [p2, p6, p10, p14]);
    mix(work, [p3, p7, p11, p15]);
    mix(work, [p0, p5, p10, p15]);
    mix(work, [p1, p6, p11, p12]);
    mix(work, [p2, p7, p8, p13]);
    mix(work, [p3, p4, p9, p14]);
}

/// The compression `G`: a block made from two into `onto`, or, with `keep`, joined by exclusive
/// or to what's there.
fn compress(previous: &Block, reference: &Block, onto: &mut Block, keep: bool) {
    let mut work = [0u64; WORDS];
    for ((slot, one), two) in work.iter_mut().zip(previous).zip(reference) {
        *slot = one ^ two;
    }
    let mut out = work;
    if keep {
        for (slot, held) in out.iter_mut().zip(onto.iter()) {
            *slot ^= held;
        }
    }
    for row in 0..8 {
        round(&mut work, std::array::from_fn(|at| row * 16 + at));
    }
    for column in 0..8 {
        round(&mut work, std::array::from_fn(|at| column * 2 + (at / 2) * 16 + at % 2));
    }
    for ((slot, kept), mixed) in onto.iter_mut().zip(out).zip(work) {
        *slot = kept ^ mixed;
    }
}

fn block_of(bytes: &[u8]) -> Block {
    let (eights, _) = bytes.as_chunks::<8>();
    let mut block = [0u64; WORDS];
    for (word, eight) in block.iter_mut().zip(eights) {
        *word = u64::from_le_bytes(*eight);
    }
    block
}

/// Where a pass, slice and index in a segment are, and the memory's shape.
#[derive(Clone, Copy)]
struct Place {
    pass: u32,
    slice: usize,
    index: usize,
    segment: usize,
    lane_length: usize,
}

/// Where in its lane the block made at `place` takes its other block from.
fn reference_index(place: Place, pseudo: u64, same_lane: bool) -> usize {
    let Place { pass, slice, index, segment, lane_length } = place;
    let area = if pass == 0 {
        if slice == 0 {
            index - 1
        } else if same_lane {
            slice * segment + index - 1
        } else {
            slice * segment - usize::from(index == 0)
        }
    } else if same_lane {
        lane_length - segment + index - 1
    } else {
        lane_length - segment - usize::from(index == 0)
    };
    let wide = |number: usize| u64::try_from(number).unwrap_or(u64::MAX);
    let low = pseudo & 0xffff_ffff;
    let relative = (low * low) >> 32;
    let relative = wide(area) - 1 - ((wide(area) * relative) >> 32);
    let start = if pass != 0 && slice != SLICES - 1 { (slice + 1) * segment } else { 0 };
    usize::try_from((wide(start) + relative) % wide(lane_length)).unwrap_or(0)
}

/// `H0`: `BLAKE2b`'s 64 bytes of the settings and inputs, with no length before them, where
/// `H'` has one.
fn first_hash(
    kind: Kind,
    settings: Settings,
    (password, salt): (&[u8], &[u8]),
    (key, data): (&[u8], &[u8]),
) -> Vec<u8> {
    let Settings { memory, passes, lanes, tag } = settings;
    let mut first = blake2b::Hasher::new(64);
    let parts: [&[u8]; 14] = [
        &le(lanes),
        &le_size(tag),
        &le(memory),
        &le(passes),
        &le(VERSION),
        &le(kind.number()),
        &le_size(password.len()),
        password,
        &le_size(salt.len()),
        salt,
        &le_size(key.len()),
        key,
        &le_size(data.len()),
        data,
    ];
    for part in parts {
        first.update(part);
    }
    first.finish()
}

/// Argon2's memory: its blocks, lanes one after another, and their shape.
struct Memory {
    blocks: Vec<Block>,
    lanes: usize,
    segment: usize,
    lane_length: usize,
}

impl Memory {
    fn block(&self, at: usize) -> Block {
        self.blocks.get(at).copied().unwrap_or([0; WORDS])
    }
}

/// Fills one lane's segment of one slice in one pass.
fn fill_segment(memory: &mut Memory, kind: Kind, (pass, passes): (u32, u32), (lane, slice): (usize, usize)) {
    let Memory { lanes, segment, lane_length, .. } = *memory;
    let independent = match kind {
        Kind::I => true,
        Kind::D => false,
        Kind::Id => pass == 0 && slice < SLICES / 2,
    };
    let wide = |number: usize| u64::try_from(number).unwrap_or(u64::MAX);
    let zero = [0u64; WORDS];
    let mut input = [0u64; WORDS];
    let made_from = [
        u64::from(pass),
        wide(lane),
        wide(slice),
        wide(lane_length * lanes),
        u64::from(passes),
        u64::from(kind.number()),
    ];
    for (slot, value) in input.iter_mut().zip(made_from) {
        *slot = value;
    }
    let mut addresses = [0u64; WORDS];
    let next_addresses = |input: &mut Block, addresses: &mut Block| {
        if let Some(counter) = input.get_mut(6) {
            *counter += 1;
        }
        let mut once = [0u64; WORDS];
        compress(&zero, input, &mut once, false);
        compress(&zero, &once, addresses, false);
    };
    let first_index = if pass == 0 && slice == 0 { 2 } else { 0 };
    if independent && first_index == 2 {
        next_addresses(&mut input, &mut addresses);
    }
    for index in first_index..segment {
        let here = lane * lane_length + slice * segment + index;
        let previous = if here.is_multiple_of(lane_length) { here + lane_length - 1 } else { here - 1 };
        let before = memory.block(previous);
        let pseudo = if independent {
            if index.is_multiple_of(WORDS) {
                next_addresses(&mut input, &mut addresses);
            }
            addresses.get(index % WORDS).copied().unwrap_or(0)
        } else {
            before.first().copied().unwrap_or(0)
        };
        let other_lane = if pass == 0 && slice == 0 {
            lane
        } else {
            usize::try_from((pseudo >> 32) % wide(lanes)).unwrap_or(0)
        };
        let at =
            reference_index(Place { pass, slice, index, segment, lane_length }, pseudo, other_lane == lane);
        let reference = memory.block(other_lane * lane_length + at);
        if let Some(slot) = memory.blocks.get_mut(here) {
            compress(&before, &reference, slot, pass > 0);
        }
    }
}

/// The tag of `password` under `salt`, with an optional secret key and data bound in, as RFC 9106
/// has them.
pub(super) fn tag(
    kind: Kind,
    settings: Settings,
    (password, salt): (&[u8], &[u8]),
    (key, data): (&[u8], &[u8]),
) -> Vec<u8> {
    let first = first_hash(kind, settings, (password, salt), (key, data));
    let lanes = usize::try_from(settings.lanes).unwrap_or(1).max(1);
    let wanted = usize::try_from(settings.memory).unwrap_or(0).max(2 * SLICES * lanes);
    let segment = wanted / (lanes * SLICES);
    let lane_length = segment * SLICES;
    let mut memory = Memory { blocks: vec![[0u64; WORDS]; lane_length * lanes], lanes, segment, lane_length };
    for lane in 0..lanes {
        for column in 0..2 {
            let made = long_hash(1024, &[&first, &le_size(column), &le_size(lane)]);
            if let Some(slot) = memory.blocks.get_mut(lane * lane_length + column) {
                *slot = block_of(&made);
            }
        }
    }
    for pass in 0..settings.passes {
        for slice in 0..SLICES {
            for lane in 0..lanes {
                fill_segment(&mut memory, kind, (pass, settings.passes), (lane, slice));
            }
        }
    }
    let mut last = memory.block(lane_length - 1);
    for lane in 1..lanes {
        let other = memory.block(lane * lane_length + lane_length - 1);
        for (slot, word) in last.iter_mut().zip(other) {
            *slot ^= word;
        }
    }
    let bytes: Vec<u8> = last.iter().flat_map(|word| word.to_le_bytes()).collect();
    long_hash(settings.tag, &[&bytes])
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn letter(value: u32) -> char {
    let at = usize::try_from(value & 63).unwrap_or(0);
    char::from(ALPHABET.get(at).copied().unwrap_or(b'A'))
}

/// Base64 without padding, as PHC strings write salts and hashes.
fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let joined = chunk.iter().fold(0u32, |held, byte| (held << 8) | u32::from(*byte));
        let shifted = joined << (8 * (3 - chunk.len()));
        for at in 0..=chunk.len() {
            let shift = u32::try_from(18 - 6 * at).unwrap_or(0);
            out.push(letter(shifted >> shift));
        }
    }
    out
}

fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut held, mut bits) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = ALPHABET.iter().position(|letter| *letter == byte)?;
        held = (held << 6) | u32::try_from(value).ok()?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((held >> bits) & 0xff).ok()?);
            held &= (1 << bits) - 1;
        }
    }
    // What's left over has to be nothing, or bits that encode nothing and are zero.
    if bits >= 6 || held != 0 {
        return None;
    }
    Some(out)
}

/// A password's hash as a PHC string, with `salt`, at the default settings.
pub(super) fn hash_password(password: &[u8], salt: &[u8]) -> String {
    let made = tag(Kind::Id, DEFAULT, (password, salt), (&[], &[]));
    let Settings { memory, passes, lanes, .. } = DEFAULT;
    format!("$argon2id$v={VERSION}$m={memory},t={passes},p={lanes}${}${}", base64(salt), base64(&made))
}

/// A stored hash, read from its PHC string.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Stored {
    pub(super) settings: Settings,
    pub(super) salt: Vec<u8>,
    pub(super) hash: Vec<u8>,
}

/// One of a stored hash's settings, `name` followed by digits with no leading zero, so a setting
/// has one way to be written.
fn setting(part: Option<&str>, name: &str, settings: &str) -> Result<u32, String> {
    let text = part
        .and_then(|part| part.strip_prefix(name))
        .ok_or_else(|| format!("a stored password's settings are `m=...,t=...,p=...`, not `{settings}`"))?;
    if text.is_empty()
        || !text.bytes().all(|byte| byte.is_ascii_digit())
        || (text.len() > 1 && text.starts_with('0'))
    {
        return Err(format!("`{name}{text}` isn't a setting a stored password can have"));
    }
    text.parse().map_err(|_| format!("`{name}{text}` is too big to be a stored password's setting"))
}

/// A stored hash read from its PHC string, or why it can't be checked: it's not one, its settings
/// are below OWASP's floor, or above what's worth checking.
pub(super) fn read(text: &str) -> Result<Stored, String> {
    let parts: Vec<&str> = text.split('$').collect();
    let ["", kind, version, settings, salt, hash] = parts[..] else {
        return Err("a stored password is written `$argon2id$v=19$m=...,t=...,p=...$salt$hash`".to_owned());
    };
    if kind != "argon2id" {
        return Err(format!("a stored password is an `argon2id` hash, not `{kind}`"));
    }
    if version != "v=19" {
        return Err(format!("a stored password is Argon2's version 19, not `{version}`"));
    }
    let mut named = settings.split(',');
    let memory = setting(named.next(), "m=", settings)?;
    let passes = setting(named.next(), "t=", settings)?;
    let lanes = setting(named.next(), "p=", settings)?;
    if named.next().is_some() {
        return Err(format!("a stored password's settings are `m=...,t=...,p=...`, not `{settings}`"));
    }
    if memory < LEAST_MEMORY || passes < LEAST_PASSES {
        return Err(format!(
            "a stored password made with m={memory},t={passes} is below the least that's checked, \
             m={LEAST_MEMORY},t={LEAST_PASSES}: hash it again"
        ));
    }
    if memory > MOST_MEMORY || passes > MOST_PASSES || lanes == 0 || lanes > MOST_LANES || memory < 8 * lanes
    {
        return Err(format!(
            "a stored password made with m={memory},t={passes},p={lanes} is past what's checked: m up to \
             {MOST_MEMORY}, t up to {MOST_PASSES}, p from 1 to {MOST_LANES}"
        ));
    }
    let salt = unbase64(salt).ok_or("a stored password's salt isn't base64")?;
    let hash = unbase64(hash).ok_or("a stored password's hash isn't base64")?;
    if !SALT_LENGTHS.contains(&salt.len()) {
        return Err(format!("a stored password's salt is {} bytes, where it's 8 to 64", salt.len()));
    }
    if !TAG_LENGTHS.contains(&hash.len()) {
        return Err(format!("a stored password's hash is {} bytes, where it's 16 to 64", hash.len()));
    }
    let tag = hash.len();
    Ok(Stored { settings: Settings { memory, passes, lanes, tag }, salt, hash })
}

/// Whether `password` is the one `stored` was made from. The tags are compared in time that
/// doesn't depend on where they differ.
pub(super) fn matches(password: &[u8], stored: &Stored) -> bool {
    let made = tag(Kind::Id, stored.settings, (password, &stored.salt), (&[], &[]));
    super::equal(&made, &stored.hash)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT, Kind, Settings, base64, hash_password, matches, read, tag, unbase64};

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
    }

    /// RFC 9106, section 5: the same password, salt, secret and data under each kind.
    #[test]
    fn tags_match_rfc_9106() {
        let settings = Settings { memory: 32, passes: 3, lanes: 4, tag: 32 };
        let made = |kind| hex(&tag(kind, settings, (&[1; 32], &[2; 16]), (&[3; 8], &[4; 12])));
        assert_eq!(made(Kind::D), "512b391b6f1162975371d30919734294f868e3be3984f3c1a13a4db9fabe4acb");
        assert_eq!(made(Kind::I), "c814d9d1dc7f37aa13f0d77f2494bda1c8de6b016dd388d29952a4c4672b6ce8");
        assert_eq!(made(Kind::Id), "0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659");
    }

    /// The same tags as `RustCrypto`'s `argon2`, a dev-dependency only, over settings and inputs
    /// from a fixed generator: memory from 8 to 263 KiB, 1 to 4 passes and lanes, tags of 4 to 99
    /// bytes, and passwords and salts of every length up to 63 bytes.
    #[test]
    fn tags_match_rustcrypto_argon2() {
        let mut state = 2026u64;
        let mut next = |top: u64| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            u32::try_from((state >> 33) % top).unwrap_or(0)
        };
        for _ in 0..60 {
            let lanes = 1 + next(4);
            let memory = (8 * lanes).max(8 + next(256));
            let passes = 1 + next(4);
            let tag_length = 4 + usize::try_from(next(96)).unwrap_or(0);
            let password: Vec<u8> = (0..next(64)).map(|_| u8::try_from(next(256)).unwrap_or(0)).collect();
            let salt: Vec<u8> = (0..8 + next(56)).map(|_| u8::try_from(next(256)).unwrap_or(0)).collect();
            let params = argon2::Params::new(memory, passes, lanes, Some(tag_length)).expect("settings");
            let theirs = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
            let mut expected = vec![0u8; tag_length];
            theirs.hash_password_into(&password, &salt, &mut expected).expect("hashed");
            let settings = Settings { memory, passes, lanes, tag: tag_length };
            assert_eq!(
                tag(Kind::Id, settings, (&password, &salt), (&[], &[])),
                expected,
                "m={memory} t={passes} p={lanes} tag={tag_length} password={} salt={}",
                password.len(),
                salt.len()
            );
        }
    }

    #[test]
    fn a_hash_checks_its_own_password_and_no_other() {
        let stored = hash_password(b"correct horse battery staple", &[7; 16]);
        assert!(stored.starts_with("$argon2id$v=19$m=19456,t=2,p=1$BwcHBwcHBwcHBwcHBwcHBw$"), "{stored}");
        let read_back = read(&stored).expect("it reads");
        assert_eq!(read_back.settings, DEFAULT);
        assert!(matches(b"correct horse battery staple", &read_back));
        assert!(!matches(b"correct horse battery stapler", &read_back));
        assert!(!matches(b"", &read_back));
    }

    #[test]
    fn base64_goes_there_and_back_and_refuses_what_isnt() {
        for length in 0..40u8 {
            let bytes: Vec<u8> = (0..length).map(|at| at.wrapping_mul(37).wrapping_add(11)).collect();
            assert_eq!(unbase64(&base64(&bytes)), Some(bytes), "{length} bytes");
        }
        assert_eq!(base64(b"hello"), "aGVsbG8");
        assert_eq!(unbase64("aGVsbG8="), None, "padding isn't written");
        assert_eq!(unbase64("aGVsbG9"), None, "leftover bits must be zero");
        assert_eq!(unbase64("a"), None, "one letter can't be a byte");
        assert_eq!(unbase64("aGV-bG8"), None, "`-` isn't in the alphabet");
    }

    /// A malformed or weak stored hash fails to be read, saying why, before anything's hashed.
    #[test]
    fn a_stored_hash_is_read_strictly() {
        let good = hash_password(b"pw", &[1; 16]);
        let salt_and_hash = good.splitn(5, '$').nth(4).expect("its salt and hash");
        let with = |head: &str| format!("{head}${salt_and_hash}");
        let refused = |text: &str| read(text).expect_err(text);
        assert!(refused("").contains("is written"));
        assert!(refused("plain text").contains("is written"));
        assert!(refused(&with("$argon2i$v=19$m=19456,t=2,p=1")).contains("not `argon2i`"));
        assert!(refused(&with("$argon2id$v=16$m=19456,t=2,p=1")).contains("version 19"));
        assert!(refused(&with("$argon2id$v=19$t=2,m=19456,p=1")).contains("settings are"));
        assert!(refused(&with("$argon2id$v=19$m=19456,t=2")).contains("settings are"));
        assert!(refused(&with("$argon2id$v=19$m=19456,t=2,p=1,x=1")).contains("settings are"));
        assert!(refused(&with("$argon2id$v=19$m=019456,t=2,p=1")).contains("isn't a setting"));
        assert!(refused(&with("$argon2id$v=19$m=-1,t=2,p=1")).contains("isn't a setting"));
        assert!(refused(&with("$argon2id$v=19$m=99999999999,t=2,p=1")).contains("too big"));
        // Below OWASP's floor, and past what's worth the memory or the time.
        assert!(refused(&with("$argon2id$v=19$m=4096,t=3,p=1")).contains("below the least"));
        assert!(refused(&with("$argon2id$v=19$m=19456,t=1,p=1")).contains("below the least"));
        assert!(refused(&with("$argon2id$v=19$m=2097152,t=2,p=1")).contains("past what's checked"));
        assert!(refused(&with("$argon2id$v=19$m=19456,t=64,p=1")).contains("past what's checked"));
        assert!(refused(&with("$argon2id$v=19$m=19456,t=2,p=0")).contains("past what's checked"));
        assert!(refused(&with("$argon2id$v=19$m=19456,t=2,p=99")).contains("past what's checked"));
        // A salt or hash that isn't base64, or is too short or too long.
        assert!(refused("$argon2id$v=19$m=19456,t=2,p=1$!!$AAAA").contains("salt isn't base64"));
        let short_salt = "$argon2id$v=19$m=19456,t=2,p=1$AAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        assert!(refused(short_salt).contains("salt is 3 bytes"));
        assert!(refused("$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAA$AAAA").contains("hash is 3 bytes"));
    }

    /// Nothing that ships takes `RustCrypto`'s `argon2`: it's here only to hold this one to.
    #[test]
    fn rustcrypto_argon2_is_only_ever_a_dev_dependency() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut taking = Vec::new();
        for entry in std::fs::read_dir(&crates).into_iter().flatten().flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path().join("Cargo.toml")) else { continue };
            let mut section = String::new();
            for line in text.lines().map(str::trim) {
                if let Some(header) = line.strip_prefix('[') {
                    header.trim_end_matches(']').clone_into(&mut section);
                    continue;
                }
                let normal =
                    matches!(section.rsplit('.').next(), Some("dependencies" | "build-dependencies"));
                let key = line.split(['=', '.']).next().unwrap_or_default().trim().trim_matches('"');
                if normal && key == "argon2" {
                    taking.push(entry.path().display().to_string());
                }
            }
        }
        let workspace = std::fs::read_to_string(crates.join("../Cargo.toml")).unwrap_or_default();
        assert!(
            !workspace.lines().any(|line| line.trim_start().starts_with("argon2")),
            "the workspace names argon2"
        );
        assert!(taking.is_empty(), "these take RustCrypto's argon2 as a normal dependency: {taking:?}");
    }
}
