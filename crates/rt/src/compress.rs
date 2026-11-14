//! What `std.compress` does: DEFLATE (RFC 1951), and the zlib (RFC 1950) and gzip (RFC 1952)
//! formats around it, written here with nothing borrowed. Compressing finds repeats with hash
//! chains, and writes each block as whichever of stored, fixed codes or codes made for it is
//! shortest, those kept to 15 bits by package-merge. Reading takes all three kinds of block,
//! fails on anything else, never panics, and stops past the most it's given, 64 MiB unless
//! `most=` says otherwise, so a small input can't make it fill memory.

/// The most a read gives back unless it's told otherwise: 64 MiB.
pub(crate) const MOST_READ: usize = 64 << 20;

/// What a read fails with past the most it was given, which `std.compress` says in its own words.
pub(crate) const PAST_MOST: &str = "it holds more than the most it was given";

const WINDOW: usize = 32_768;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195,
    227, 258,
];
const LENGTH_EXTRA: [u8; 29] =
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073,
    4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
/// The order a dynamic block's code length code lengths come in.
const CODE_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
/// How many earlier places each level tries for a repeat.
const CHAINS: [usize; 10] = [0, 4, 8, 16, 32, 64, 128, 256, 1024, 4096];

/// What's at `at` in a table this code sized itself, so it's there; the default if it isn't.
fn item<T: Copy + Default>(items: &[T], at: usize) -> T {
    items.get(at).copied().unwrap_or_default()
}

/// Sets what's at `at`, in a table this code sized itself.
fn put_at<T>(items: &mut [T], at: usize, value: T) {
    if let Some(slot) = items.get_mut(at) {
        *slot = value;
    }
}

/// Adds one to the count at `at`.
fn count_at(counts: &mut [u32], at: usize) {
    if let Some(count) = counts.get_mut(at) {
        *count += 1;
    }
}

/// The fixed code's length for a literal or length symbol.
fn fixed_length(symbol: usize) -> u8 {
    match symbol {
        144..=255 => 9,
        256..=279 => 7,
        _ => 8,
    }
}

// ---- Reading -------------------------------------------------------------------------------

struct Bits<'d> {
    data: &'d [u8],
    at: usize,
    held: u64,
    count: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Result<(), String> {
        while self.count < n {
            let byte = *self.data.get(self.at).ok_or("the data ends too soon")?;
            self.at += 1;
            self.held |= u64::from(byte) << self.count;
            self.count += 8;
        }
        Ok(())
    }

    fn take(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let value = u32::try_from(self.held & ((1 << n) - 1)).unwrap_or(0);
        self.held >>= n;
        self.count -= n;
        Ok(value)
    }

    /// Drops what's left of the byte being read, for a stored block.
    fn skip_to_byte(&mut self) {
        let drop = self.count % 8;
        self.held >>= drop;
        self.count -= drop;
    }
}

/// A Huffman code to read, as counts of codes of each length and the symbols in code order,
/// read a bit at a time as Mark Adler's `puff` does.
struct Decoder {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Decoder {
    fn new(lengths: &[u8]) -> Result<Decoder, String> {
        let mut counts = [0_u16; 16];
        for &length in lengths {
            if let Some(count) = counts.get_mut(usize::from(length)) {
                *count += 1;
            }
        }
        // Too many codes of some length can't be a prefix code.
        let mut left: i32 = 1;
        for &count in counts.iter().skip(1) {
            left = left * 2 - i32::from(count);
            if left < 0 {
                return Err("a Huffman code has too many codes".to_owned());
            }
        }
        let mut offsets = [0_u16; 16];
        for length in 1..15 {
            let next = item(&offsets, length) + item(&counts, length);
            put_at(&mut offsets, length + 1, next);
        }
        let mut symbols = vec![0; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let slot = item(&offsets, usize::from(length));
                put_at(&mut symbols, usize::from(slot), u16::try_from(symbol).unwrap_or(0));
                put_at(&mut offsets, usize::from(length), slot + 1);
            }
        }
        put_at(&mut counts, 0, 0);
        Ok(Decoder { counts, symbols })
    }

    fn read(&self, bits: &mut Bits<'_>) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0_i32, 0_i32, 0_i32);
        for length in 1..16 {
            code |= i32::try_from(bits.take(1)?).unwrap_or(0);
            let count = i32::from(item(&self.counts, length));
            if code - count < first {
                let at = usize::try_from(index + (code - first)).unwrap_or(usize::MAX);
                return self.symbols.get(at).copied().ok_or_else(|| "a Huffman code is broken".to_owned());
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("a code isn't any of its Huffman code's".to_owned())
    }
}

fn fixed_decoders() -> Result<(Decoder, Decoder), String> {
    let lengths: Vec<u8> = (0..288).map(fixed_length).collect();
    Ok((Decoder::new(&lengths)?, Decoder::new(&[5; 30])?))
}

fn dynamic_decoders(bits: &mut Bits<'_>) -> Result<(Decoder, Decoder), String> {
    let literals = usize::try_from(bits.take(5)?).unwrap_or(0) + 257;
    let distances = usize::try_from(bits.take(5)?).unwrap_or(0) + 1;
    let codes = usize::try_from(bits.take(4)?).unwrap_or(0) + 4;
    if literals > 286 || distances > 30 {
        return Err("a block has too many codes".to_owned());
    }
    let mut code_lengths = [0_u8; 19];
    for &at in CODE_ORDER.iter().take(codes) {
        put_at(&mut code_lengths, at, u8::try_from(bits.take(3)?).unwrap_or(0));
    }
    let lengths_code = Decoder::new(&code_lengths)?;
    let mut lengths = vec![0_u8; literals + distances];
    let mut at = 0;
    while at < lengths.len() {
        let symbol = lengths_code.read(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (u8::try_from(symbol).unwrap_or(0), 1),
            16 => {
                let previous =
                    *lengths.get(at.wrapping_sub(1)).ok_or("a length repeats before there is one")?;
                (previous, 3 + bits.take(2)?)
            }
            17 => (0, 3 + bits.take(3)?),
            _ => (0, 11 + bits.take(7)?),
        };
        for _ in 0..repeat {
            *lengths.get_mut(at).ok_or("a block's code lengths run over")? = value;
            at += 1;
        }
    }
    if lengths.get(256) == Some(&0) {
        return Err("a block has no code for its end".to_owned());
    }
    let (literal_lengths, distance_lengths) = lengths.split_at(literals);
    Ok((Decoder::new(literal_lengths)?, Decoder::new(distance_lengths)?))
}

/// Reads one block's codes into `out`, up to its end.
fn read_codes(
    bits: &mut Bits<'_>,
    codes: &(Decoder, Decoder),
    out: &mut Vec<u8>,
    most: usize,
) -> Result<(), String> {
    let (literals, distances) = codes;
    loop {
        let symbol = literals.read(bits)?;
        if let Ok(byte) = u8::try_from(symbol) {
            out.push(byte);
        } else if symbol == 256 {
            return Ok(());
        } else {
            let index = usize::from(symbol - 257);
            let base = *LENGTH_BASE.get(index).ok_or("a length code isn't one")?;
            let extra = u32::from(item(&LENGTH_EXTRA, index));
            let length = usize::from(base) + usize::try_from(bits.take(extra)?).unwrap_or(0);
            let code = usize::from(distances.read(bits)?);
            let base = *DISTANCE_BASE.get(code).ok_or("a distance code isn't one")?;
            let extra = u32::from(item(&DISTANCE_EXTRA, code));
            let distance = usize::from(base) + usize::try_from(bits.take(extra)?).unwrap_or(0);
            if distance > out.len() {
                return Err("a match reaches back before the start".to_owned());
            }
            let from = out.len() - distance;
            for offset in 0..length {
                let byte = item(out, from + offset);
                out.push(byte);
            }
        }
        if out.len() > most {
            return Err(PAST_MOST.to_owned());
        }
    }
}

/// The bytes raw DEFLATE data holds, and where in `data` it ends, failing past `most` bytes.
pub(crate) fn inflate(data: &[u8], most: usize) -> Result<(Vec<u8>, usize), String> {
    let mut bits = Bits { data, at: 0, held: 0, count: 0 };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => {
                bits.skip_to_byte();
                let size = bits.take(16)?;
                let check = bits.take(16)?;
                if size != !check & 0xFFFF {
                    return Err("a stored block's length doesn't match its check".to_owned());
                }
                if usize::try_from(size).unwrap_or(usize::MAX) > most.saturating_sub(out.len()) {
                    return Err(PAST_MOST.to_owned());
                }
                for _ in 0..size {
                    out.push(u8::try_from(bits.take(8)?).unwrap_or(0));
                }
            }
            1 => read_codes(&mut bits, &fixed_decoders()?, &mut out, most)?,
            2 => {
                let codes = dynamic_decoders(&mut bits)?;
                read_codes(&mut bits, &codes, &mut out, most)?;
            }
            _ => return Err("a block's kind isn't one".to_owned()),
        }
        if out.len() > most {
            return Err(PAST_MOST.to_owned());
        }
        if last {
            break;
        }
    }
    bits.skip_to_byte();
    let unread = usize::try_from(bits.count / 8).unwrap_or(0);
    Ok((out, bits.at - unread))
}

// ---- Writing -------------------------------------------------------------------------------

struct Writer {
    out: Vec<u8>,
    held: u64,
    count: u32,
}

impl Writer {
    fn put(&mut self, value: u32, n: u32) {
        self.held |= u64::from(value) << self.count;
        self.count += n;
        while self.count >= 8 {
            self.out.push(u8::try_from(self.held & 0xFF).unwrap_or(0));
            self.held >>= 8;
            self.count -= 8;
        }
    }

    /// A Huffman code, which DEFLATE writes from its first bit.
    fn put_code(&mut self, code: u32, length: u8) {
        let mut reversed = 0;
        for bit in 0..length {
            reversed |= ((code >> bit) & 1) << (length - 1 - bit);
        }
        self.put(reversed, u32::from(length));
    }

    fn flush(&mut self) {
        if self.count > 0 {
            self.put(0, 8 - self.count % 8);
        }
    }
}

/// What a block says: a byte, or a match of a length at a distance.
#[derive(Clone, Copy)]
enum Token {
    Literal(u8),
    Match(u16, u16),
}

/// The code for a length, and its extra bits and their value.
fn length_code(length: u16) -> (usize, u8, u16) {
    let index = LENGTH_BASE.partition_point(|&base| base <= length).saturating_sub(1);
    let index = if length == 258 { 28 } else { index.min(27) };
    (257 + index, item(&LENGTH_EXTRA, index), length - item(&LENGTH_BASE, index))
}

fn distance_code(distance: u16) -> (usize, u8, u16) {
    let index = DISTANCE_BASE.partition_point(|&base| base <= distance).saturating_sub(1);
    (index, item(&DISTANCE_EXTRA, index), distance - item(&DISTANCE_BASE, index))
}

/// How many bytes from `earlier` and `at` are the same, up to `most`.
fn same_run(data: &[u8], earlier: usize, at: usize, most: usize) -> usize {
    match (data.get(earlier..earlier + most), data.get(at..at + most)) {
        (Some(first), Some(second)) => first.iter().zip(second).take_while(|(x, y)| x == y).count(),
        _ => 0,
    }
}

/// Repeats found by hash chains, trying `chain` earlier places for each, as `level` sets.
fn tokens(data: &[u8], chain: usize) -> Vec<Token> {
    const HASH_BITS: u32 = 15;
    let hash = |at: usize| -> usize {
        let word = (u32::from(item(data, at)) << 16)
            | (u32::from(item(data, at + 1)) << 8)
            | u32::from(item(data, at + 2));
        usize::try_from(word.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)).unwrap_or(0)
    };
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut previous = vec![usize::MAX; data.len()];
    let insert = |head: &mut [usize], previous: &mut [usize], at: usize| {
        if at + MIN_MATCH <= data.len() {
            let key = hash(at);
            put_at(previous, at, item(head, key));
            put_at(head, key, at);
        }
    };
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut at = 0;
    while at < data.len() {
        let mut best = (0, 0);
        if chain > 0 && at + MIN_MATCH <= data.len() {
            let mut candidate = item(&head, hash(at));
            let mut tries = chain;
            let most = MAX_MATCH.min(data.len() - at);
            while candidate != usize::MAX && at - candidate <= WINDOW && tries > 0 {
                let same = same_run(data, candidate, at, most);
                if same > best.0 {
                    best = (same, at - candidate);
                    if same == most {
                        break;
                    }
                }
                candidate = item(&previous, candidate);
                tries -= 1;
            }
        }
        if best.0 >= MIN_MATCH {
            out.push(Token::Match(u16::try_from(best.0).unwrap_or(3), u16::try_from(best.1).unwrap_or(1)));
            for place in at..at + best.0 {
                insert(&mut head, &mut previous, place);
            }
            at += best.0;
        } else {
            out.push(Token::Literal(item(data, at)));
            insert(&mut head, &mut previous, at);
            at += 1;
        }
    }
    out
}

/// Code lengths of at most `limit` bits for symbols used `counts` times, by package-merge,
/// which finds the shortest such code. A symbol never used gets no code.
fn code_lengths(counts: &[u32], limit: u8) -> Vec<u8> {
    let mut leaves: Vec<(u64, usize)> = counts
        .iter()
        .enumerate()
        .filter(|&(_, &count)| count > 0)
        .map(|(symbol, &count)| (u64::from(count), symbol))
        .collect();
    let mut lengths = vec![0_u8; counts.len()];
    if leaves.len() < 2 {
        for &(_, symbol) in &leaves {
            put_at(&mut lengths, symbol, 1);
        }
        return lengths;
    }
    leaves.sort_unstable();
    // Each item: its weight, and the leaves in it, each as often as it's in it.
    let leaf_items: Vec<(u64, Vec<usize>)> =
        leaves.iter().map(|&(weight, symbol)| (weight, vec![symbol])).collect();
    let mut row = leaf_items.clone();
    for _ in 1..limit {
        let mut packaged: Vec<(u64, Vec<usize>)> = row
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[first, second]| (first.0 + second.0, first.1.iter().chain(&second.1).copied().collect()))
            .collect();
        packaged.extend(leaf_items.iter().cloned());
        packaged.sort_by_key(|entry| entry.0);
        row = packaged;
    }
    for entry in row.iter().take(2 * leaves.len() - 2) {
        for &symbol in &entry.1 {
            let grown = item(&lengths, symbol) + 1;
            put_at(&mut lengths, symbol, grown);
        }
    }
    lengths
}

/// The canonical codes for code lengths.
fn codes_of(lengths: &[u8]) -> Vec<u32> {
    let mut counts = [0_u32; 16];
    for &length in lengths {
        count_at(&mut counts, usize::from(length));
    }
    put_at(&mut counts, 0, 0);
    let mut next = [0_u32; 16];
    let mut code = 0;
    for bits in 1..16 {
        code = (code + item(&counts, bits - 1)) << 1;
        put_at(&mut next, bits, code);
    }
    lengths
        .iter()
        .map(|&length| {
            if length == 0 {
                return 0;
            }
            let code = item(&next, usize::from(length));
            put_at(&mut next, usize::from(length), code + 1);
            code
        })
        .collect()
}

/// The code lengths of a dynamic block's two codes, written with runs as RFC 1951's code
/// length code does: each a symbol from 0 to 18 and its extra bits.
fn length_runs(lengths: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < lengths.len() {
        let length = item(lengths, at);
        let mut run = 1;
        while at + run < lengths.len() && item(lengths, at + run) == length {
            run += 1;
        }
        let mut left = run;
        if length == 0 {
            while left >= 11 {
                let n = left.min(138);
                out.push((18, u8::try_from(n - 11).unwrap_or(0)));
                left -= n;
            }
            if left >= 3 {
                out.push((17, u8::try_from(left - 3).unwrap_or(0)));
                left = 0;
            }
        } else if left >= 4 {
            out.push((length, 0));
            left -= 1;
            while left >= 3 {
                let n = left.min(6);
                out.push((16, u8::try_from(n - 3).unwrap_or(0)));
                left -= n;
            }
        }
        for _ in 0..left {
            out.push((length, 0));
        }
        at += run;
    }
    out
}

/// How many bits `tokens` take written with these codes, extra bits too, and the end of block.
fn cost(tokens: &[Token], literal_lengths: &[u8], distance_lengths: &[u8]) -> u64 {
    let mut bits = u64::from(item(literal_lengths, 256));
    for token in tokens {
        bits += match *token {
            Token::Literal(byte) => u64::from(item(literal_lengths, usize::from(byte))),
            Token::Match(length, distance) => {
                let (code, extra, _) = length_code(length);
                let (far, far_extra, _) = distance_code(distance);
                u64::from(item(literal_lengths, code))
                    + u64::from(extra)
                    + u64::from(item(distance_lengths, far))
                    + u64::from(far_extra)
            }
        };
    }
    bits
}

/// A dynamic block's codes: its two codes' lengths, how many of each it writes, its code
/// length code, and the runs it writes them in.
struct Dynamic {
    literal_lengths: Vec<u8>,
    distance_lengths: Vec<u8>,
    literals: usize,
    distances: usize,
    run_lengths: Vec<u8>,
    used_runs: usize,
    runs: Vec<(u8, u8)>,
}

impl Dynamic {
    fn new(tokens: &[Token]) -> Dynamic {
        let mut literal_counts = [0_u32; 286];
        let mut distance_counts = [0_u32; 30];
        for token in tokens {
            match *token {
                Token::Literal(byte) => count_at(&mut literal_counts, usize::from(byte)),
                Token::Match(length, distance) => {
                    count_at(&mut literal_counts, length_code(length).0);
                    count_at(&mut distance_counts, distance_code(distance).0);
                }
            }
        }
        put_at(&mut literal_counts, 256, 1);
        let literal_lengths = code_lengths(&literal_counts, 15);
        let mut distance_lengths = code_lengths(&distance_counts, 15);
        // A block with no matches still gives one distance code.
        if distance_lengths.iter().all(|&length| length == 0) {
            put_at(&mut distance_lengths, 0, 1);
        }
        let literals =
            257.max(literal_lengths.iter().rposition(|&length| length != 0).map_or(0, |at| at + 1));
        let distances =
            1.max(distance_lengths.iter().rposition(|&length| length != 0).map_or(0, |at| at + 1));
        let mut all: Vec<u8> = literal_lengths.iter().take(literals).copied().collect();
        all.extend(distance_lengths.iter().take(distances));
        let runs = length_runs(&all);
        let mut run_counts = [0_u32; 19];
        for &(symbol, _) in &runs {
            count_at(&mut run_counts, usize::from(symbol));
        }
        let run_lengths = code_lengths(&run_counts, 7);
        let used_runs = 4.max(
            CODE_ORDER.iter().rposition(|&symbol| item(&run_lengths, symbol) != 0).map_or(0, |at| at + 1),
        );
        Dynamic { literal_lengths, distance_lengths, literals, distances, run_lengths, used_runs, runs }
    }

    /// The bits its header takes, before the tokens.
    fn header_bits(&self) -> u64 {
        let mut bits = 5 + 5 + 4 + 3 * u64::try_from(self.used_runs).unwrap_or(0);
        for &(symbol, _) in &self.runs {
            bits += u64::from(item(&self.run_lengths, usize::from(symbol))) + u64::from(run_extra(symbol));
        }
        bits
    }

    fn write_header(&self, writer: &mut Writer) {
        writer.put(u32::try_from(self.literals - 257).unwrap_or(0), 5);
        writer.put(u32::try_from(self.distances - 1).unwrap_or(0), 5);
        writer.put(u32::try_from(self.used_runs - 4).unwrap_or(0), 4);
        for &symbol in CODE_ORDER.iter().take(self.used_runs) {
            writer.put(u32::from(item(&self.run_lengths, symbol)), 3);
        }
        let run_codes = codes_of(&self.run_lengths);
        for &(symbol, extra) in &self.runs {
            let at = usize::from(symbol);
            writer.put_code(item(&run_codes, at), item(&self.run_lengths, at));
            writer.put(u32::from(extra), u32::from(run_extra(symbol)));
        }
    }
}

/// How many extra bits a code length code symbol has.
fn run_extra(symbol: u8) -> u8 {
    match symbol {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    }
}

fn write_tokens(writer: &mut Writer, tokens: &[Token], literal_lengths: &[u8], distance_lengths: &[u8]) {
    let literal_codes = codes_of(literal_lengths);
    let distance_codes = codes_of(distance_lengths);
    for token in tokens {
        match *token {
            Token::Literal(byte) => {
                let at = usize::from(byte);
                writer.put_code(item(&literal_codes, at), item(literal_lengths, at));
            }
            Token::Match(length, distance) => {
                let (code, extra, value) = length_code(length);
                writer.put_code(item(&literal_codes, code), item(literal_lengths, code));
                writer.put(u32::from(value), u32::from(extra));
                let (code, extra, value) = distance_code(distance);
                writer.put_code(item(&distance_codes, code), item(distance_lengths, code));
                writer.put(u32::from(value), u32::from(extra));
            }
        }
    }
    writer.put_code(item(&literal_codes, 256), item(literal_lengths, 256));
}

/// Writes `data` as stored blocks, which hold it as it is.
fn write_stored(writer: &mut Writer, data: &[u8], last: bool) {
    let mut chunks = data.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        writer.put(u32::from(last), 1);
        writer.put(0, 2);
        writer.flush();
        writer.out.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
        return;
    }
    while let Some(chunk) = chunks.next() {
        writer.put(u32::from(last && chunks.peek().is_none()), 1);
        writer.put(0, 2);
        writer.flush();
        let size = u16::try_from(chunk.len()).unwrap_or(0);
        writer.out.extend_from_slice(&size.to_le_bytes());
        writer.out.extend_from_slice(&(!size).to_le_bytes());
        writer.out.extend_from_slice(chunk);
    }
}

/// Writes one block of `tokens`, which say `data`, as whichever of a stored block, a block
/// with the fixed codes, or one with codes made for it, takes fewest bits.
fn write_block(writer: &mut Writer, tokens: &[Token], data: &[u8], last: bool) {
    let dynamic = Dynamic::new(tokens);
    let dynamic_bits =
        dynamic.header_bits() + cost(tokens, &dynamic.literal_lengths, &dynamic.distance_lengths);
    let fixed_literals: Vec<u8> = (0..288).map(fixed_length).collect();
    let fixed_distances = [5_u8; 30];
    let fixed_bits = cost(tokens, &fixed_literals, &fixed_distances);
    let stored_bits = (u64::try_from(data.len()).unwrap_or(u64::MAX) + 5) * 8;
    if stored_bits < dynamic_bits.min(fixed_bits) {
        write_stored(writer, data, last);
    } else if fixed_bits <= dynamic_bits {
        writer.put(u32::from(last), 1);
        writer.put(1, 2);
        write_tokens(writer, tokens, &fixed_literals, &fixed_distances);
    } else {
        writer.put(u32::from(last), 1);
        writer.put(2, 2);
        dynamic.write_header(writer);
        write_tokens(writer, tokens, &dynamic.literal_lengths, &dynamic.distance_lengths);
    }
}

/// `data` as raw DEFLATE at `level`, 0 for stored blocks only and 1 to 9 for more effort
/// spent finding repeats.
pub(crate) fn deflate(data: &[u8], level: u8) -> Vec<u8> {
    let mut writer = Writer { out: Vec::with_capacity(data.len() / 2 + 16), held: 0, count: 0 };
    if level == 0 {
        write_stored(&mut writer, data, true);
        return writer.out;
    }
    let found = tokens(data, item(&CHAINS, usize::from(level.min(9))));
    let mut start = 0;
    let mut blocks = found.chunks(16_384).peekable();
    if blocks.peek().is_none() {
        write_block(&mut writer, &[], &[], true);
    }
    while let Some(block) = blocks.next() {
        let size: usize = block
            .iter()
            .map(|token| match *token {
                Token::Literal(_) => 1,
                Token::Match(length, _) => usize::from(length),
            })
            .sum();
        write_block(
            &mut writer,
            block,
            data.get(start..start + size).unwrap_or_default(),
            blocks.peek().is_none(),
        );
        start += size;
    }
    writer.flush();
    writer.out
}

// ---- zlib and gzip -------------------------------------------------------------------------

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1_u32, 0_u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0_u32; 256];
        for (n, slot) in table.iter_mut().enumerate() {
            let mut c = u32::try_from(n).unwrap_or(0);
            for _ in 0..8 {
                c = if c & 1 == 1 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        table
    });
    let mut crc = !0_u32;
    for &byte in data {
        crc = item(table, usize::try_from((crc ^ u32::from(byte)) & 0xFF).unwrap_or(0)) ^ (crc >> 8);
    }
    !crc
}

pub(crate) fn zlib(data: &[u8], level: u8) -> Vec<u8> {
    let flag = match level {
        0 | 1 => 0x01,
        2..=5 => 0x5E,
        6 => 0x9C,
        _ => 0xDA,
    };
    let mut out = vec![0x78, flag];
    out.extend(deflate(data, level));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Four bytes of `data` from `at`, as a word.
fn word_at(data: &[u8], at: usize) -> Option<[u8; 4]> {
    data.get(at..at + 4).and_then(|four| four.try_into().ok())
}

pub(crate) fn unzlib(data: &[u8], most: usize) -> Result<Vec<u8>, String> {
    let (&method, &flags) = (data.first().ok_or("it's empty")?, data.get(1).ok_or("it ends in its header")?);
    if method & 0x0F != 8 || (u16::from(method) << 8 | u16::from(flags)) % 31 != 0 {
        return Err("it doesn't start as zlib data does".to_owned());
    }
    // A window past 32 KiB isn't one DEFLATE has, as RFC 1950 says.
    if method >> 4 > 7 {
        return Err("it says its window is bigger than DEFLATE's".to_owned());
    }
    if flags & 0x20 != 0 {
        return Err("it needs a preset dictionary, which this doesn't take".to_owned());
    }
    let (out, used) = inflate(data.get(2..).unwrap_or_default(), most)?;
    let check = word_at(data, 2 + used).ok_or("it ends before its check")?;
    if u32::from_be_bytes(check) != adler32(&out) {
        return Err("its check doesn't match what it holds".to_owned());
    }
    // As `inflate` refuses: what follows would be read by nothing, or by another reader as more.
    if data.len() > 2 + used + 4 {
        return Err("there's more after its end".to_owned());
    }
    Ok(out)
}

/// The low 32 bits of a length, which is all gzip keeps of it. It goes through `u64`, so it's
/// right whether a `usize` is 32 bits or 64.
fn length_word(size: usize) -> u32 {
    u32::try_from(u64::try_from(size).unwrap_or(u64::MAX) & 0xFFFF_FFFF).unwrap_or(0)
}

pub(crate) fn gzip(data: &[u8], level: u8) -> Vec<u8> {
    let extra = match level {
        0 | 1 => 4,
        9 => 2,
        _ => 0,
    };
    // No name or time, so the same data always gives the same bytes; the system is "unknown".
    let mut out = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, extra, 255];
    out.extend(deflate(data, level));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&length_word(data.len()).to_le_bytes());
    out
}

/// Where a gzip member's DEFLATE data starts, past its header, which is checked as RFC 1952
/// says: no flag it doesn't define, and its own check, when it has one, right.
fn gzip_start(member: &[u8]) -> Result<usize, String> {
    let flags = item(member, 3);
    if flags & 0xE0 != 0 {
        return Err("its header has a flag gzip doesn't define".to_owned());
    }
    let mut place = 10;
    if flags & 4 != 0 {
        let low = *member.get(place).ok_or("it ends in its header")?;
        let high = *member.get(place + 1).ok_or("it ends in its header")?;
        place += 2 + usize::from(u16::from_le_bytes([low, high]));
    }
    for flag in [8, 16] {
        if flags & flag != 0 {
            while *member.get(place).ok_or("it ends in its header")? != 0 {
                place += 1;
            }
            place += 1;
        }
    }
    if flags & 2 != 0 {
        let header = member.get(..place).ok_or("it ends in its header")?;
        let check = member.get(place..place + 2).ok_or("it ends in its header")?;
        if u32::from(u16::from_le_bytes([item(check, 0), item(check, 1)])) != crc32(header) & 0xFFFF {
            return Err("its header's check doesn't match the header".to_owned());
        }
        place += 2;
    }
    Ok(place)
}

/// The bytes gzip data holds, every member one after another, as `gunzip` gives them, failing
/// past `most` bytes in all.
pub(crate) fn gunzip(data: &[u8], most: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let rest = data.get(at..).unwrap_or_default();
        if rest.len() < 10 || !rest.starts_with(&[0x1F, 0x8B, 8]) {
            return Err(if at == 0 {
                "it doesn't start as gzip data does".to_owned()
            } else {
                "something after its end isn't gzip".to_owned()
            });
        }
        let place = gzip_start(rest)?;
        let (held, used) = inflate(rest.get(place..).ok_or("it ends in its header")?, most - out.len())?;
        let sum = word_at(rest, place + used).ok_or("it ends before its check")?;
        let size = word_at(rest, place + used + 4).ok_or("it ends before its check")?;
        if u32::from_le_bytes(sum) != crc32(&held) {
            return Err("its check doesn't match what it holds".to_owned());
        }
        if u32::from_le_bytes(size) != length_word(held.len()) {
            return Err("its length doesn't match what it holds".to_owned());
        }
        out.extend(held);
        if out.len() > most {
            return Err(PAST_MOST.to_owned());
        }
        at += place + used + 8;
        if at >= data.len() {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MOST_READ, PAST_MOST, crc32, deflate, gunzip, gzip, inflate, unzlib, zlib};

    fn samples() -> Vec<Vec<u8>> {
        let mut seed = 0x2545_f491_u64;
        let mut random = Vec::new();
        for _ in 0..70_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            random.push(u8::try_from(seed & 0xFF).unwrap_or(0));
        }
        vec![
            Vec::new(),
            b"a".to_vec(),
            b"abcabcabcabcabcabcabcabc".to_vec(),
            "the quick brown fox jumps over the lazy dog. ".repeat(500).into_bytes(),
            vec![0; 100_000],
            random,
            (0..=255).cycle().take(300_000).collect(),
        ]
    }

    #[test]
    fn what_goes_in_comes_back_at_every_level() {
        for data in samples() {
            for level in 0..=9 {
                let packed = deflate(&data, level);
                assert_eq!(
                    inflate(&packed, MOST_READ).map(|(out, _)| out).as_deref(),
                    Ok(&data[..]),
                    "level {level}, {} bytes",
                    data.len()
                );
                assert_eq!(unzlib(&zlib(&data, level), MOST_READ).as_deref(), Ok(&data[..]));
                assert_eq!(gunzip(&gzip(&data, level), MOST_READ).as_deref(), Ok(&data[..]));
            }
        }
    }

    #[test]
    fn broken_data_fails_and_never_panics() {
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let good = gzip(&"fennec fox den ".repeat(300).into_bytes(), 6);
        for round in 0..20_000_usize {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let size = usize::try_from(seed % 300).unwrap_or(0);
            let mut data: Vec<u8> =
                (0..size).map(|at| u8::try_from((seed >> (at % 56)) & 0xFF).unwrap_or(0)).collect();
            // Half are real data with one byte changed, or cut short.
            if round % 2 == 0 {
                data.clone_from(&good);
                let at = usize::try_from(seed % u64::try_from(data.len()).unwrap_or(1)).unwrap_or(0);
                if round % 4 == 0 {
                    data.truncate(at);
                } else if let Some(byte) = data.get_mut(at) {
                    *byte ^= u8::try_from((seed >> 8) & 0xFF).unwrap_or(1) | 1;
                }
            }
            let _ = inflate(&data, MOST_READ);
            let _ = gunzip(&data, MOST_READ);
            let _ = unzlib(&data, MOST_READ);
        }
    }

    /// A gzip bomb: a megabyte of zeros is a kilobyte or so, and `gunzip` reads members one after
    /// another, so 65 of them give back 65 MiB from about 65 KiB, past the most unless told
    /// otherwise.
    #[test]
    fn a_bomb_stops_at_the_most_it_was_given() {
        let zeros = vec![0; 1 << 20];
        let member = gzip(&zeros, 9);
        assert!(member.len() < 4096, "{}", member.len());
        assert_eq!(gunzip(&member.repeat(65), MOST_READ), Err(PAST_MOST.to_owned()));
        assert_eq!(gunzip(&member.repeat(64), MOST_READ).map(|out| out.len()), Ok(64 << 20));
        // One stream past the most it's given, however it's wrapped.
        let most = 64 * 1024;
        assert_eq!(inflate(&deflate(&zeros, 9), most).err(), Some(PAST_MOST.to_owned()));
        assert_eq!(unzlib(&zlib(&zeros, 9), most), Err(PAST_MOST.to_owned()));
        assert_eq!(gunzip(&member, most), Err(PAST_MOST.to_owned()));
        // A stored block that would pass the most is refused before it's read.
        assert_eq!(inflate(&deflate(&vec![7; 70_000], 0), most).err(), Some(PAST_MOST.to_owned()));
        assert_eq!(inflate(&deflate(&vec![7; 60_000], 0), most).map(|(out, _)| out.len()), Ok(60_000));
    }

    #[test]
    fn repeats_get_smaller() {
        let text = "the quick brown fox jumps over the lazy dog. ".repeat(500);
        assert!(deflate(text.as_bytes(), 6).len() < 200);
        assert!(deflate(&vec![0; 100_000], 6).len() < 200);
    }

    #[test]
    fn known_answers() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // zlib.compress(b"hello") from Python.
        assert_eq!(
            unzlib(
                &[0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0x00, 0x06, 0x2c, 0x02, 0x15],
                MOST_READ
            )
            .as_deref(),
            Ok(&b"hello"[..])
        );
        assert!(gunzip(b"not gzip at all", MOST_READ).is_err());
        let mut broken = gzip(b"fennec", 6);
        let last = broken.len() - 6;
        broken[last] ^= 1;
        assert_eq!(gunzip(&broken, MOST_READ), Err("its check doesn't match what it holds".to_owned()));
    }

    /// zlib and gzip are read as their RFCs say: nothing after zlib's end, no window past
    /// DEFLATE's, no gzip flag that isn't defined, and a gzip header's own check right.
    #[test]
    fn framing_is_read_as_the_rfcs_say() {
        let fine = zlib(b"fennec", 6);
        let mut longer = fine.clone();
        longer.push(0);
        assert_eq!(unzlib(&longer, MOST_READ), Err("there's more after its end".to_owned()));
        // A window of 64 KiB, with the header's check made right for it.
        let mut wide = fine.clone();
        wide[0] = 0x88;
        wide[1] = 0x1C;
        assert_eq!(unzlib(&wide, MOST_READ), Err("it says its window is bigger than DEFLATE's".to_owned()));
        let packed = gzip(b"fennec", 6);
        for flag in [0x20, 0x40, 0x80] {
            let mut odd = packed.clone();
            odd[3] = flag;
            assert_eq!(gunzip(&odd, MOST_READ), Err("its header has a flag gzip doesn't define".to_owned()));
        }
        // A header check: right reads, wrong fails.
        let mut checked = packed[..10].to_vec();
        checked[3] = 2;
        let sum = u16::try_from(crc32(&checked) & 0xFFFF).unwrap_or(0).to_le_bytes();
        checked.extend_from_slice(&sum);
        checked.extend_from_slice(&packed[10..]);
        assert_eq!(gunzip(&checked, MOST_READ).as_deref(), Ok(&b"fennec"[..]));
        checked[10] ^= 1;
        assert_eq!(
            gunzip(&checked, MOST_READ),
            Err("its header's check doesn't match the header".to_owned())
        );
    }

    /// Every cut of zlib or gzip data fails, and every byte of it changed fails or reads as what
    /// was packed: the checks catch the rest, and nothing panics or runs away.
    #[test]
    fn a_cut_or_changed_byte_fails_or_reads_the_same() {
        let data = b"the quick brown fox jumps over the lazy dog, again and again and again".repeat(20);
        for (packed, read) in [
            (zlib(&data, 6), unzlib as fn(&[u8], usize) -> Result<Vec<u8>, String>),
            (gzip(&data, 6), gunzip),
            (zlib(&data, 0), unzlib),
            (gzip(&data, 9), gunzip),
        ] {
            for end in 0..packed.len() {
                assert!(read(&packed[..end], MOST_READ).is_err(), "a cut at {end} read");
            }
            for at in 0..packed.len() {
                for byte in [0, 1, 0x7F, 0x80, 0xFF] {
                    let mut changed = packed.clone();
                    changed[at] = byte;
                    if let Ok(out) = read(&changed, MOST_READ) {
                        assert_eq!(out, data, "byte {at} as {byte} read as something else");
                    }
                }
            }
        }
    }
}
