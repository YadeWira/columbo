//! Deflate block parsing and the 5-candidate re-encoding engine.
//!
//! columbo does not perform a fresh LZ77 search: it parses the token
//! stream (literals, length/distance matches) already present in the
//! source Deflate data, then re-encodes each block choosing the smallest
//! of 5 candidates. See `research/design-v1.md` for the full rationale
//! and provenance of each candidate.

use crate::bit::{BitReader, BitWriter};
use crate::error::Error;

/// Number of literal/length alphabet symbols (0..=285 used, 286/287 reserved).
pub const LITLEN_SYMBOLS: usize = 288;
/// Number of distance alphabet symbols (0..=29 used, 30/31 reserved).
pub const DIST_SYMBOLS: usize = 32;
/// Token-count gate: blocks at or below this size skip candidate search
/// and are emitted directly as fixed Huffman (defluff `0x4071db`).
pub const SMALL_BLOCK_GATE: usize = 25;
/// Maximum decoded byte length of a single stored block (RFC 1951 LEN field).
pub const MAX_STORED_LEN: usize = 65535;

// --- RFC 1951 tables -------------------------------------------------------

/// (extra_bits, base_length) for length symbols 257..=285.
const LENGTH_TABLE: [(u8, u16); 29] = [
    (0, 3), (0, 4), (0, 5), (0, 6), (0, 7), (0, 8), (0, 9), (0, 10),
    (1, 11), (1, 13), (1, 15), (1, 17),
    (2, 19), (2, 23), (2, 27), (2, 31),
    (3, 35), (3, 43), (3, 51), (3, 59),
    (4, 67), (4, 83), (4, 99), (4, 115),
    (5, 131), (5, 163), (5, 195), (5, 227),
    (0, 258),
];

/// (extra_bits, base_distance) for distance symbols 0..=29.
const DIST_TABLE: [(u8, u16); 30] = [
    (0, 1), (0, 2), (0, 3), (0, 4),
    (1, 5), (1, 7),
    (2, 9), (2, 13),
    (3, 17), (3, 25),
    (4, 33), (4, 49),
    (5, 65), (5, 97),
    (6, 129), (6, 193),
    (7, 257), (7, 385),
    (8, 513), (8, 769),
    (9, 1025), (9, 1537),
    (10, 2049), (10, 3073),
    (11, 4097), (11, 6145),
    (12, 8193), (12, 12289),
    (13, 16385), (13, 24577),
];

/// Order in which code-length code lengths (HCLEN) are transmitted.
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Symbol 284 with extra value 0x1f (31) encodes length 258 — the same
/// length symbol 285 encodes directly. This is the "edge case" that
/// gates defluff's single iterative-refinement pass.
const SYM284_LEN258_EXTRA: u32 = 0x1f;

fn fixed_litlen_lengths() -> [u8; LITLEN_SYMBOLS] {
    let mut l = [0u8; LITLEN_SYMBOLS];
    for (i, v) in l.iter_mut().enumerate() {
        *v = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            280..=287 => 8,
            _ => unreachable!(),
        };
    }
    l
}

fn fixed_dist_lengths() -> [u8; DIST_SYMBOLS] {
    let mut l = [0u8; DIST_SYMBOLS];
    for v in l.iter_mut().take(30) {
        *v = 5;
    }
    l
}

// --- Canonical Huffman codes -------------------------------------------------
//
// Encoding uses `crate::huffman::canonical_codes`, which returns codes
// already bit-reversed for a single `write_bits(len, code)` call against
// the LSB-first bit writer (the standard trick — see `huffman/mod.rs`).
// Decoding here needs the *non*-reversed, MSB-first convention instead
// (codes built up one bit at a time as `code = (code << 1) | bit`), so
// [`decode_codes`] stays local and is not shared with the encode path.

/// Build non-reversed canonical codes from per-symbol code lengths
/// (RFC 1951 §3.2.2), for [`HuffDecoder`]'s bit-at-a-time matching.
/// Returns `(code, length)` per symbol; unused symbols get `(0, 0)`.
fn decode_codes(lengths: &[u8]) -> Vec<(u16, u8)> {
    let max_bits = lengths.iter().copied().max().unwrap_or(0) as usize;
    let mut bl_count = vec![0u32; max_bits + 1];
    for &l in lengths {
        if l > 0 {
            bl_count[l as usize] += 1;
        }
    }
    let mut code = 0u32;
    let mut next_code = vec![0u32; max_bits + 2];
    for bits in 1..=max_bits {
        code = (code + bl_count[bits - 1]) << 1;
        next_code[bits] = code;
    }
    let mut out = vec![(0u16, 0u8); lengths.len()];
    for (sym, &len) in lengths.iter().enumerate() {
        if len > 0 {
            let l = len as usize;
            out[sym] = (next_code[l] as u16, len);
            next_code[l] += 1;
        }
    }
    out
}

/// Decoder for a canonical Huffman code table, built from per-symbol
/// code lengths. Decodes one bit at a time (codes are packed MSB-first
/// within the LSB-first bitstream, per RFC 1951 §3.1.1).
struct HuffDecoder {
    /// `table[(length, code)] = symbol`, only for lengths 1..=15.
    table: std::collections::HashMap<(u8, u16), u16>,
    max_len: u8,
}

impl HuffDecoder {
    fn from_lengths(lengths: &[u8]) -> Self {
        let codes = decode_codes(lengths);
        let mut table = std::collections::HashMap::new();
        let mut max_len = 0u8;
        for (sym, &(code, len)) in codes.iter().enumerate() {
            if len > 0 {
                table.insert((len, code), sym as u16);
                max_len = max_len.max(len);
            }
        }
        HuffDecoder { table, max_len }
    }

    fn decode(&self, r: &mut BitReader) -> Result<u16, Error> {
        let mut code: u16 = 0;
        for len in 1..=self.max_len {
            code = (code << 1) | (r.read_bit()? as u16);
            if let Some(&sym) = self.table.get(&(len, code)) {
                return Ok(sym);
            }
        }
        Err(Error::MissingCode(code))
    }
}

/// Write a symbol using a canonical code table built by
/// `crate::huffman::canonical_codes` (bit-reversed codes, one `write_bits`
/// call per symbol).
fn write_huffman_symbol(w: &mut BitWriter, codes: &[(u32, u8)], symbol: usize) -> Result<(), Error> {
    let (code, len) = codes.get(symbol).copied().unwrap_or((0, 0));
    if len == 0 {
        return Err(Error::MissingCode(symbol as u16));
    }
    w.write_bits(len as u32, code);
    Ok(())
}

// --- Tokens ------------------------------------------------------------------

/// A single decoded Deflate token.
#[derive(Debug, Clone)]
pub enum Token {
    Literal(u8),
    Match {
        length: u16,
        distance: u16,
        /// True if this match's length (always 258) was coded as symbol
        /// 284 (extra=0x1f) rather than the canonical symbol 285.
        uses_sym284: bool,
        /// The `length` literal bytes this match resolves to (via the
        /// standard LZ77 back-reference copy, byte-by-byte so `distance <
        /// length` overlap/RLE repeats resolve correctly). Needed to
        /// expand a match into literals for the dynamic-pruned candidate,
        /// which columbo does not otherwise reconstruct decompressed
        /// output for.
        resolved: Vec<u8>,
    },
}

/// One parsed Deflate block: its tokens, frequency tables (for rebuilding
/// Huffman trees), and enough bookkeeping to reproduce or replace it.
pub struct ParsedBlock {
    pub tokens: Vec<Token>,
    pub is_final: bool,
    pub litlen_freq: [u32; LITLEN_SYMBOLS],
    pub dist_freq: [u32; DIST_SYMBOLS],
    /// Set if any token used the symbol-284 length-258 edge case
    /// (gates the dynamic-pruned iteration candidate).
    pub uses_sym284: bool,
    /// Exact original bits of this block, byte-unaligned span
    /// `[start_bit, end_bit)` in the source stream — used by the
    /// dynamic-original-fallback candidate (DeflOpt `0x406bcb` method).
    pub original_bit_range: (u64, u64),
}

/// Decoded stream: all blocks plus the exact bit length of the source,
/// used to slice out original bits for the fallback candidate.
pub struct ParsedStream<'a> {
    pub blocks: Vec<ParsedBlock>,
    pub source: &'a [u8],
}

/// Parse an existing Deflate stream into its constituent blocks and tokens.
/// Also reconstructs the decompressed byte stream internally (not exposed
/// on `ParsedStream`) so each `Token::Match` can carry its resolved literal
/// bytes — needed by the dynamic-pruned candidate's match→literal
/// expansion. Distances are checked against the amount of output produced
/// so far but not against the 32K window limit specifically.
pub fn parse_deflate<'a>(bits: &'a [u8]) -> Result<ParsedStream<'a>, Error> {
    let mut r = BitReader::new(bits);
    let mut blocks = Vec::new();
    let mut output: Vec<u8> = Vec::new();

    loop {
        let start_bit = r.bits_consumed();
        let bfinal = r.read_bit()?;
        let btype = r.read_bits(2)?;

        let mut litlen_freq = [0u32; LITLEN_SYMBOLS];
        let mut dist_freq = [0u32; DIST_SYMBOLS];
        let mut tokens = Vec::new();
        let mut uses_sym284 = false;

        match btype {
            0 => {
                // Stored block: align, LEN, NLEN, raw bytes.
                r.align_to_byte();
                let len = r.read_bits(16)? as u16;
                let nlen = r.read_bits(16)? as u16;
                if len != !nlen {
                    return Err(Error::InvalidBlockType(0));
                }
                for _ in 0..len {
                    let byte = r.read_bits(8)? as u8;
                    litlen_freq[byte as usize] += 1;
                    output.push(byte);
                    tokens.push(Token::Literal(byte));
                }
                litlen_freq[256] += 1; // conceptual EOB for frequency purposes
            }
            1 => {
                let litlen_lengths = fixed_litlen_lengths();
                let dist_lengths = fixed_dist_lengths();
                decode_huffman_block(
                    &mut r,
                    &litlen_lengths,
                    &dist_lengths,
                    &mut tokens,
                    &mut litlen_freq,
                    &mut dist_freq,
                    &mut uses_sym284,
                    &mut output,
                )?;
            }
            2 => {
                let (litlen_lengths, dist_lengths) = read_dynamic_header(&mut r)?;
                decode_huffman_block(
                    &mut r,
                    &litlen_lengths,
                    &dist_lengths,
                    &mut tokens,
                    &mut litlen_freq,
                    &mut dist_freq,
                    &mut uses_sym284,
                    &mut output,
                )?;
            }
            _ => return Err(Error::InvalidBlockType(btype as u8)),
        }

        let end_bit = r.bits_consumed();
        blocks.push(ParsedBlock {
            tokens,
            is_final: bfinal,
            litlen_freq,
            dist_freq,
            uses_sym284,
            original_bit_range: (start_bit, end_bit),
        });

        if bfinal {
            break;
        }
    }

    Ok(ParsedStream { blocks, source: bits })
}

/// Resolve a length/distance match against the decompressed output so far
/// into its literal bytes, via the standard LZ77 back-reference copy —
/// byte-by-byte, so a `distance < length` overlap (RLE-style repeats)
/// resolves correctly rather than as a bulk slice copy.
fn resolve_match(output: &[u8], length: u16, distance: u16) -> Vec<u8> {
    let start = output.len() - distance as usize;
    let mut resolved = Vec::with_capacity(length as usize);
    for i in 0..length as usize {
        let pos = start + i;
        // `distance < length` overlaps into bytes this same match is still
        // producing (RLE-style repeats) — those come from `resolved`, not
        // yet from `output`.
        let byte = if pos < output.len() { output[pos] } else { resolved[pos - output.len()] };
        resolved.push(byte);
    }
    resolved
}

#[allow(clippy::too_many_arguments)]
fn decode_huffman_block(
    r: &mut BitReader,
    litlen_lengths: &[u8],
    dist_lengths: &[u8],
    tokens: &mut Vec<Token>,
    litlen_freq: &mut [u32; LITLEN_SYMBOLS],
    dist_freq: &mut [u32; DIST_SYMBOLS],
    uses_sym284: &mut bool,
    output: &mut Vec<u8>,
) -> Result<(), Error> {
    let litlen_dec = HuffDecoder::from_lengths(litlen_lengths);
    let dist_dec = HuffDecoder::from_lengths(dist_lengths);

    loop {
        let sym = litlen_dec.decode(r)?;
        litlen_freq[sym as usize] += 1;
        if sym < 256 {
            output.push(sym as u8);
            tokens.push(Token::Literal(sym as u8));
        } else if sym == 256 {
            break; // end of block
        } else {
            let idx = (sym - 257) as usize;
            let (extra_bits, base) = *LENGTH_TABLE
                .get(idx)
                .ok_or(Error::InvalidBlockType(2))?;
            let extra = if extra_bits > 0 { r.read_bits(extra_bits as u32)? } else { 0 };
            let length = base + extra as u16;
            let edge_case = sym == 284 && extra == SYM284_LEN258_EXTRA;
            if edge_case {
                *uses_sym284 = true;
            }

            let dsym = dist_dec.decode(r)?;
            dist_freq[dsym as usize] += 1;
            let (dextra_bits, dbase) = *DIST_TABLE
                .get(dsym as usize)
                .ok_or(Error::InvalidBlockType(2))?;
            let dextra = if dextra_bits > 0 { r.read_bits(dextra_bits as u32)? } else { 0 };
            let distance = dbase + dextra as u16;

            if distance as usize > output.len() {
                return Err(Error::InvalidBlockType(2)); // distance beyond window/output
            }
            let resolved = resolve_match(output, length, distance);
            output.extend_from_slice(&resolved);
            tokens.push(Token::Match { length, distance, uses_sym284: edge_case, resolved });
        }
    }
    Ok(())
}

fn read_dynamic_header(r: &mut BitReader) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let hlit = r.read_bits(5)? as usize + 257;
    let hdist = r.read_bits(5)? as usize + 1;
    let hclen = r.read_bits(4)? as usize + 4;

    let mut clen_lengths = [0u8; 19];
    for i in 0..hclen {
        clen_lengths[CLEN_ORDER[i]] = r.read_bits(3)? as u8;
    }
    let clen_dec = HuffDecoder::from_lengths(&clen_lengths);

    let total = hlit + hdist;
    let mut lengths = Vec::with_capacity(total);
    while lengths.len() < total {
        let sym = clen_dec.decode(r)?;
        match sym {
            0..=15 => lengths.push(sym as u8),
            16 => {
                let prev = *lengths.last().ok_or(Error::InvalidBlockType(2))?;
                let repeat = r.read_bits(2)? + 3;
                for _ in 0..repeat {
                    lengths.push(prev);
                }
            }
            17 => {
                let repeat = r.read_bits(3)? + 3;
                for _ in 0..repeat {
                    lengths.push(0);
                }
            }
            18 => {
                let repeat = r.read_bits(7)? + 11;
                for _ in 0..repeat {
                    lengths.push(0);
                }
            }
            _ => return Err(Error::InvalidBlockType(2)),
        }
    }
    if lengths.len() != total {
        return Err(Error::InvalidBlockType(2));
    }

    let litlen_lengths = lengths[0..hlit].to_vec();
    let dist_lengths = lengths[hlit..hlit + hdist].to_vec();
    Ok((litlen_lengths, dist_lengths))
}

// --- Candidate encoding --------------------------------------------------
//
// Candidates are costed by formula (no speculative bit-writer construction)
// and the winner is emitted directly into the shared output `BitWriter`.
// This matters because Deflate blocks are *not* byte-aligned in general —
// only stored blocks force alignment, and how much padding that costs
// depends on the real bit position where the block starts in the overall
// stream. Building each candidate in an isolated writer (which always
// starts "aligned" at its own bit 0) would compute the wrong padding for
// stored candidates and silently corrupt the output. Costing by formula
// against the real writer's `bits_written()` avoids that trap entirely.

/// (symbol, extra_bits, extra_value) for a length 3..=258, matching RFC 1951
/// unless `force_sym284` requests the non-canonical symbol-284 encoding of
/// length 258 (extra value 0x1f) instead of the canonical symbol 285.
fn classify_length(length: u16, force_sym284: bool) -> (usize, u8, u16) {
    if length == 258 && force_sym284 {
        return (284, 5, SYM284_LEN258_EXTRA as u16);
    }
    for (i, &(extra_bits, base)) in LENGTH_TABLE.iter().enumerate() {
        let max = if extra_bits > 0 { base + (1u16 << extra_bits) - 1 } else { base };
        if length >= base && length <= max {
            return (257 + i, extra_bits, length - base);
        }
    }
    unreachable!("length {length} out of Deflate range 3..=258")
}

/// (symbol, extra_bits, extra_value) for a distance 1..=32768.
fn classify_distance(distance: u16) -> (usize, u8, u16) {
    for (sym, &(extra_bits, base)) in DIST_TABLE.iter().enumerate() {
        let max = if extra_bits > 0 { base + (1u16 << extra_bits) - 1 } else { base };
        if distance >= base && distance <= max {
            return (sym, extra_bits, distance - base);
        }
    }
    unreachable!("distance {distance} out of Deflate range 1..=32768")
}

fn write_length(w: &mut BitWriter, length: u16, force_sym284: bool, codes: &[(u32, u8)]) -> Result<(), Error> {
    let (sym, extra_bits, extra_value) = classify_length(length, force_sym284);
    write_huffman_symbol(w, codes, sym)?;
    if extra_bits > 0 {
        w.write_bits(extra_bits as u32, extra_value as u32);
    }
    Ok(())
}

fn write_distance(w: &mut BitWriter, distance: u16, codes: &[(u32, u8)]) -> Result<(), Error> {
    let (sym, extra_bits, extra_value) = classify_distance(distance);
    write_huffman_symbol(w, codes, sym)?;
    if extra_bits > 0 {
        w.write_bits(extra_bits as u32, extra_value as u32);
    }
    Ok(())
}

fn write_tokens(
    w: &mut BitWriter,
    tokens: &[Token],
    litlen_codes: &[(u32, u8)],
    dist_codes: &[(u32, u8)],
) -> Result<(), Error> {
    for t in tokens {
        match t {
            Token::Literal(b) => write_huffman_symbol(w, litlen_codes, *b as usize)?,
            Token::Match { length, distance, uses_sym284, .. } => {
                write_length(w, *length, *uses_sym284, litlen_codes)?;
                write_distance(w, *distance, dist_codes)?;
            }
        }
    }
    write_huffman_symbol(w, litlen_codes, 256) // end-of-block
}

/// Bits needed to emit `tokens` as fixed-Huffman (BTYPE=1), including the
/// 3-bit block header and end-of-block symbol. Always defined — fixed
/// tables cover every literal, length, and distance symbol.
fn fixed_bit_cost(tokens: &[Token], litlen_codes: &[(u32, u8)], dist_codes: &[(u32, u8)]) -> u64 {
    let mut bits: u64 = 3; // BFINAL + BTYPE
    for t in tokens {
        bits += match t {
            Token::Literal(b) => litlen_codes[*b as usize].1 as u64,
            Token::Match { length, distance, uses_sym284, .. } => {
                let (lsym, lextra, _) = classify_length(*length, *uses_sym284);
                let (dsym, dextra, _) = classify_distance(*distance);
                litlen_codes[lsym].1 as u64 + lextra as u64 + dist_codes[dsym].1 as u64 + dextra as u64
            }
        };
    }
    bits += litlen_codes[256].1 as u64; // EOB
    bits
}

/// Emit `tokens` as fixed-Huffman (BTYPE=1) directly into `w`.
fn emit_fixed(w: &mut BitWriter, tokens: &[Token], is_final_block: bool) -> Result<(), Error> {
    let litlen_lengths = fixed_litlen_lengths();
    let dist_lengths = fixed_dist_lengths();
    let litlen_codes = crate::huffman::canonical_codes(&litlen_lengths);
    let dist_codes = crate::huffman::canonical_codes(&dist_lengths);

    w.write_bits(1, is_final_block as u32);
    w.write_bits(2, 1); // BTYPE = fixed
    write_tokens(w, tokens, &litlen_codes, &dist_codes)
}

/// Bits needed to emit `tokens` as one or more stored (BTYPE=0) blocks,
/// starting at bit position `start_pos` in the real output stream — the
/// padding before each sub-block's LEN/NLEN depends on that position.
/// Returns `None` if any token is a match (stored blocks are literal-only).
/// Splits into multiple sub-blocks past [`MAX_STORED_LEN`] bytes (defluff
/// method) — DeflOpt instead rejects oversized stored candidates outright.
fn stored_bit_cost(tokens: &[Token], start_pos: u64) -> Option<u64> {
    let count = tokens.len();
    for t in tokens {
        if matches!(t, Token::Match { .. }) {
            return None;
        }
    }
    let chunk_sizes = chunk_lengths(count);
    let mut pos = start_pos;
    for chunk_len in chunk_sizes {
        pos += 3; // BFINAL + BTYPE
        let pad = (8 - (pos % 8)) % 8;
        pos += pad + 32 + (chunk_len as u64) * 8;
    }
    Some(pos - start_pos)
}

/// Emit `tokens` as one or more stored (BTYPE=0) blocks directly into `w`.
/// Caller must have already checked (via [`stored_bit_cost`]) that every
/// token is a literal.
fn emit_stored(w: &mut BitWriter, tokens: &[Token], is_final_block: bool) {
    let bytes: Vec<u8> = tokens
        .iter()
        .map(|t| match t {
            Token::Literal(b) => *b,
            Token::Match { .. } => unreachable!("emit_stored called with a non-literal token"),
        })
        .collect();

    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![&[]]
    } else {
        bytes.chunks(MAX_STORED_LEN).collect()
    };
    let n = chunks.len();
    for (i, chunk) in chunks.iter().enumerate() {
        let is_last_chunk = i + 1 == n;
        let bfinal = is_final_block && is_last_chunk;
        w.write_bits(1, bfinal as u32);
        w.write_bits(2, 0); // BTYPE = stored
        w.align_to_byte();
        let len = chunk.len() as u32;
        w.write_bits(16, len);
        w.write_bits(16, (!len) & 0xFFFF);
        for &b in chunk.iter() {
            w.write_bits(8, b as u32);
        }
    }
}

/// Sub-block byte lengths for a stored candidate of `total` literal tokens.
fn chunk_lengths(total: usize) -> Vec<usize> {
    if total == 0 {
        return vec![0];
    }
    let mut sizes = Vec::new();
    let mut remaining = total;
    while remaining > 0 {
        let take = remaining.min(MAX_STORED_LEN);
        sizes.push(take);
        remaining -= take;
    }
    sizes
}

/// Precomputed data for a dynamic-block candidate ("rebuilt" or "pruned"),
/// so costing and emission share one Huffman/RLE build instead of
/// computing it twice. Owns the token list it was built from, since the
/// pruned candidate's tokens (post match→literal expansion) differ from
/// the block's original tokens.
struct DynamicPlan {
    tokens: Vec<Token>,
    litlen_lengths: Vec<u8>,
    dist_lengths: Vec<u8>,
    hlit: usize,
    hdist: usize,
    hclen: usize,
    clen_lengths: Vec<u8>,
    rle_tokens: Vec<(u16, u8, u16)>,
    bit_cost: u64,
}

/// Build a dynamic-block plan from an explicit token list and its
/// litlen/dist frequency tables: package-merge (or whatever `huffman.rs`
/// currently implements) trees, header packed with c-lab's greedy
/// single-pass RLE encoder plus its local strict replacement pass.
fn plan_dynamic(
    tokens: Vec<Token>,
    litlen_freq: &[u32; LITLEN_SYMBOLS],
    dist_freq: &[u32; DIST_SYMBOLS],
) -> DynamicPlan {
    let litlen_lengths = crate::huffman::build_lengths(litlen_freq, 15);
    let mut dist_lengths = crate::huffman::build_lengths(dist_freq, 15);
    if dist_lengths.iter().all(|&l| l == 0) {
        // RFC 1951 requires HDIST >= 1; a block with no matches still needs
        // one (unused) distance code.
        dist_lengths[0] = 1;
    }

    let last_litlen = litlen_lengths.iter().rposition(|&l| l > 0).unwrap_or(255);
    let hlit = (last_litlen + 1).max(257);
    let last_dist = dist_lengths.iter().rposition(|&l| l > 0).unwrap_or(0);
    let hdist = (last_dist + 1).max(1);

    let initial_tokens = crate::rle::pack_lengths(&litlen_lengths[..hlit], &dist_lengths[..hdist]);
    let (clen_lengths, _, _, hclen) = crate::rle::build_code_length_tree(&initial_tokens);
    let combined: Vec<u8> = litlen_lengths[..hlit]
        .iter()
        .chain(dist_lengths[..hdist].iter())
        .copied()
        .collect();
    let rle_tokens = crate::rle::optimise_rle(&initial_tokens, &clen_lengths, &combined);

    let litlen_codes = crate::huffman::canonical_codes(&litlen_lengths);
    let dist_codes = crate::huffman::canonical_codes(&dist_lengths);

    let mut bit_cost: u64 = 3 + 5 + 5 + 4 + (hclen as u64) * 3;
    for &(sym, extra_bits, _) in &rle_tokens {
        bit_cost += clen_lengths[sym as usize] as u64 + extra_bits as u64;
    }
    for t in &tokens {
        bit_cost += match t {
            Token::Literal(b) => litlen_codes[*b as usize].1 as u64,
            Token::Match { length, distance, uses_sym284, .. } => {
                let (lsym, lextra, _) = classify_length(*length, *uses_sym284);
                let (dsym, dextra, _) = classify_distance(*distance);
                litlen_codes[lsym].1 as u64 + lextra as u64 + dist_codes[dsym].1 as u64 + dextra as u64
            }
        };
    }
    bit_cost += litlen_codes[256].1 as u64; // EOB

    DynamicPlan { tokens, litlen_lengths, dist_lengths, hlit, hdist, hclen, clen_lengths, rle_tokens, bit_cost }
}

/// Plan the "dynamic rebuilt" candidate: trees built from this block's own,
/// unmodified token frequencies (defluff method).
fn plan_dynamic_rebuilt(block: &ParsedBlock) -> DynamicPlan {
    plan_dynamic(block.tokens.clone(), &block.litlen_freq, &block.dist_freq)
}

/// Emit a dynamic-block candidate (BTYPE=2) directly into `w`, from a plan
/// already built by [`plan_dynamic`] (or [`plan_dynamic_rebuilt`] /
/// [`plan_dynamic_pruned`]).
fn emit_dynamic_plan(w: &mut BitWriter, plan: &DynamicPlan, is_final_block: bool) -> Result<(), Error> {
    w.write_bits(1, is_final_block as u32);
    w.write_bits(2, 2); // BTYPE = dynamic
    w.write_bits(5, (plan.hlit - 257) as u32);
    w.write_bits(5, (plan.hdist - 1) as u32);
    w.write_bits(4, (plan.hclen - 4) as u32);

    for i in 0..plan.hclen {
        w.write_bits(3, plan.clen_lengths[crate::rle::CLEN_PERMUTATION[i]] as u32);
    }

    let clen_codes = crate::huffman::canonical_codes(&plan.clen_lengths);
    for &(sym, extra_bits, extra_val) in &plan.rle_tokens {
        write_huffman_symbol(w, &clen_codes, sym as usize)?;
        if extra_bits > 0 {
            w.write_bits(extra_bits as u32, extra_val as u32);
        }
    }

    let litlen_codes = crate::huffman::canonical_codes(&plan.litlen_lengths);
    let dist_codes = crate::huffman::canonical_codes(&plan.dist_lengths);
    write_tokens(w, &plan.tokens, &litlen_codes, &dist_codes)
}

/// The "dynamic original fallback" candidate: copy the exact original
/// bits of this block verbatim (DeflOpt `0x406bcb` method) — used when
/// the rebuilt header is not strictly smaller than the source. Correct
/// regardless of the real output position since it copies bits, not bytes.
/// Cost is simply the original span's bit length.
fn emit_dynamic_original_fallback(w: &mut BitWriter, stream: &ParsedStream, block: &ParsedBlock) {
    let (start, end) = block.original_bit_range;
    let mut r = BitReader::new(stream.source);
    for _ in 0..start {
        let _ = r.read_bit();
    }
    for _ in start..end {
        let bit = r.read_bit().unwrap_or(false);
        w.write_bits(1, bit as u32);
    }
}

/// Maximum refinement passes for the pruned candidate (bounded, unlike
/// deft4j's "iterate until no saving" — see `research/design-v1.md`).
const PRUNED_MAX_ITERATIONS: u32 = 2;

/// The "dynamic pruned" candidate: scorer-driven match→literal expansion,
/// rebuilding trees from the resulting frequencies, bounded to at most
/// [`PRUNED_MAX_ITERATIONS`] iterations for determinism (adapted from
/// deft4j's unbounded prune/recode loop).
///
/// Each iteration builds trees from the current token/frequency state (the
/// "evaluated table"), then scans every `Match` token: if encoding its
/// `length` resolved bytes as literals under *that* table would cost no
/// more than keeping it as a length/distance pair (`<=`, not strict `<`)
/// — and every one of those literals already has a code in the table (code
/// length > 0) — the match is expanded. Frequencies are updated (match's
/// length/distance counts decremented, the expanded literals' counts
/// incremented) and the next iteration rebuilds trees from that. The
/// non-strict gate deliberately allows a break-even expansion through:
/// deft4j's own prune mode does the same, using it as a seed that a later
/// rebuild may turn into a real win even if this iteration alone doesn't
/// (confirmed empirically: relaxing `<` to `<=` and continuing to iterate
/// past a non-improving round, rather than stopping immediately, closed
/// a real gap versus a reference implementation on one test file exactly).
/// Only stops early if an iteration expands nothing at all (fully
/// converged) — otherwise runs the full [`PRUNED_MAX_ITERATIONS`] budget,
/// keeping the best cost seen across all of them.
fn plan_dynamic_pruned(
    tokens: Vec<Token>,
    litlen_freq: [u32; LITLEN_SYMBOLS],
    dist_freq: [u32; DIST_SYMBOLS],
) -> DynamicPlan {
    let mut tokens = tokens;
    let mut litlen_freq = litlen_freq;
    let mut dist_freq = dist_freq;

    let mut best = plan_dynamic(tokens.clone(), &litlen_freq, &dist_freq);

    for _ in 0..PRUNED_MAX_ITERATIONS {
        let litlen_codes = crate::huffman::canonical_codes(&crate::huffman::build_lengths(&litlen_freq, 15));

        let mut new_tokens = Vec::with_capacity(tokens.len());
        let mut new_litlen_freq = litlen_freq;
        let mut new_dist_freq = dist_freq;
        let mut expanded_any = false;

        for t in tokens.into_iter() {
            match t {
                Token::Literal(b) => new_tokens.push(Token::Literal(b)),
                Token::Match { length, distance, uses_sym284, resolved } => {
                    let (lsym, lextra, _) = classify_length(length, uses_sym284);
                    let (dsym, dextra, _) = classify_distance(distance);
                    // Distance encoding disappears entirely if expanded, so
                    // the match's full cost (length + distance) is what's
                    // being traded away, not just the length symbol.
                    let dist_codes = crate::huffman::canonical_codes(&crate::huffman::build_lengths(&dist_freq, 15));
                    let match_cost = litlen_codes[lsym].1 as u64
                        + lextra as u64
                        + dist_codes[dsym].1 as u64
                        + dextra as u64;
                    let all_literals_coded = resolved.iter().all(|&b| litlen_codes[b as usize].1 > 0);
                    let literal_cost: u64 = resolved.iter().map(|&b| litlen_codes[b as usize].1 as u64).sum();

                    if all_literals_coded && literal_cost <= match_cost {
                        new_litlen_freq[lsym] -= 1;
                        new_dist_freq[dsym] -= 1;
                        for &b in &resolved {
                            new_litlen_freq[b as usize] += 1;
                            new_tokens.push(Token::Literal(b));
                        }
                        expanded_any = true;
                    } else {
                        new_tokens.push(Token::Match { length, distance, uses_sym284, resolved });
                    }
                }
            }
        }

        if !expanded_any {
            break; // converged — no further expansion possible under this table
        }

        let candidate = plan_dynamic(new_tokens.clone(), &new_litlen_freq, &new_dist_freq);
        tokens = new_tokens;
        litlen_freq = new_litlen_freq;
        dist_freq = new_dist_freq;

        if candidate.bit_cost < best.bit_cost {
            best = candidate;
        }
        // Keep seeding forward even when this iteration didn't beat the
        // best-seen cost yet — deft4j's fixed-point loop does the same
        // (non-strict expansion can set up a later iteration's rebuild to
        // win, even if this one doesn't immediately).
    }

    best
}

// --- Block splitting (v1.2) --------------------------------------------------
//
// See `research/block-splitting.md` for the design. ace-dent's columbo v0.2
// alpha does this and none of the three RE'd reference tools (DeflOpt,
// defluff, deft4j) do — it's the dominant source of the byte-savings gap
// observed against it on large single-block files.

/// Below this token count, splitting is never attempted — header overhead
/// per extra block isn't worth it (matches `block-splitting.md`'s guidance).
const SPLIT_MIN_TOKENS: usize = 500;
/// Sub-blocks are never allowed to shrink below this many tokens.
const SPLIT_MIN_SUBBLOCK_TOKENS: usize = 200;
/// Recursion depth cap — bounds the number of resulting sub-blocks to at
/// most 2^SPLIT_MAX_DEPTH, matching `block-splitting.md`'s K <= 16 guidance.
const SPLIT_MAX_DEPTH: usize = 4;
/// Number of candidate split positions sampled per recursion level.
const SPLIT_SAMPLES: usize = 64;

fn token_litlen_symbol(t: &Token) -> usize {
    match t {
        Token::Literal(b) => *b as usize,
        Token::Match { length, uses_sym284, .. } => classify_length(*length, *uses_sym284).0,
    }
}

fn range_litlen_freq(tokens: &[Token], start: usize, end: usize) -> [u32; LITLEN_SYMBOLS] {
    let mut f = [0u32; LITLEN_SYMBOLS];
    for t in &tokens[start..end] {
        f[token_litlen_symbol(t)] += 1;
    }
    f
}

/// Like [`range_litlen_freq`], but also counts the end-of-block symbol
/// (256) once — every emitted sub-block needs its own EOB, unlike the
/// entropy-estimate use of `range_litlen_freq` in [`find_splits`], which
/// only compares relative costs and doesn't need it. Omitting this for a
/// real Huffman tree build leaves symbol 256 with a zero code length,
/// which then fails to encode when `write_tokens` emits the sub-block's
/// terminating EOB — caught via a real end-to-end roundtrip test that
/// silently fell back to unmodified output because the resulting `Error`
/// propagated up to `container::detect_and_optimise`'s `.ok()`.
fn range_litlen_freq_for_block(tokens: &[Token], start: usize, end: usize) -> [u32; LITLEN_SYMBOLS] {
    let mut f = range_litlen_freq(tokens, start, end);
    f[256] += 1;
    f
}

/// Cheap order-0 entropy estimate (not a full Huffman tree build) for
/// choosing WHERE to split — exact tree costs are only computed for the
/// final chosen split points, via [`plan_dynamic`].
fn estimate_bits(freq: &[u32; LITLEN_SYMBOLS], token_count: usize) -> f64 {
    if token_count == 0 {
        return 0.0;
    }
    let total = token_count as f64;
    freq.iter()
        .filter(|&&f| f > 0)
        .map(|&f| {
            let p = f as f64 / total;
            -(f as f64) * p.log2()
        })
        .sum()
}

/// Rough per-sub-block dynamic-header overhead (HLIT/HDIST/HCLEN + code-length
/// tree + RLE-encoded lengths): a few hundred bits typically (matches
/// `research/block-splitting.md`'s "~50-300 bits of header overhead" estimate
/// for each extra block). Each candidate split must beat the entropy sum by
/// more than this, or it's just trading payload bits for header bits.
const SPLIT_HEADER_OVERHEAD_BITS: f64 = 200.0;

/// Recursive bisection (zopfli-style): find the single split point in
/// `tokens[start..end]` that most reduces the estimated entropy cost (after
/// accounting for the extra header a second block costs), and recurse into
/// each half while it keeps helping, `SPLIT_MAX_DEPTH` levels deep or until
/// sub-blocks would fall below [`SPLIT_MIN_SUBBLOCK_TOKENS`]. Appends chosen
/// absolute split offsets to `splits` (unsorted).
fn find_splits(tokens: &[Token], start: usize, end: usize, depth_budget: usize, splits: &mut Vec<usize>) {
    let len = end - start;
    if depth_budget == 0 || len < 2 * SPLIT_MIN_SUBBLOCK_TOKENS {
        return;
    }

    let whole_cost = estimate_bits(&range_litlen_freq(tokens, start, end), len);

    let usable = len - 2 * SPLIT_MIN_SUBBLOCK_TOKENS;
    let step = (usable / SPLIT_SAMPLES).max(1);
    let mut best_split = None;
    let mut best_cost = whole_cost;
    let mut pos = start + SPLIT_MIN_SUBBLOCK_TOKENS;
    while pos + SPLIT_MIN_SUBBLOCK_TOKENS <= end {
        let left = estimate_bits(&range_litlen_freq(tokens, start, pos), pos - start);
        let right = estimate_bits(&range_litlen_freq(tokens, pos, end), end - pos);
        let cost = left + right + SPLIT_HEADER_OVERHEAD_BITS;
        if cost < best_cost {
            best_cost = cost;
            best_split = Some(pos);
        }
        pos += step;
    }

    if let Some(split) = best_split {
        splits.push(split);
        find_splits(tokens, start, split, depth_budget - 1, splits);
        find_splits(tokens, split, end, depth_budget - 1, splits);
    }
}

/// The winning representation for one (sub-)block, plus its exact bit cost.
enum BlockCandidate {
    Stored,
    Fixed,
    Dynamic(DynamicPlan),
}

/// Choose the cheapest of stored/fixed/dynamic-rebuilt/dynamic-pruned for
/// an explicit token range with its own local frequencies — shared between
/// the top-level per-block loop and per-sub-block evaluation after
/// splitting. Does not consider the dynamic-original-fallback candidate:
/// that needs a contiguous span of *original* bits, which a synthetic
/// sub-block (produced by splitting) doesn't have.
fn best_block_candidate(
    tokens: &[Token],
    litlen_freq: [u32; LITLEN_SYMBOLS],
    dist_freq: [u32; DIST_SYMBOLS],
    start_pos: u64,
    fixed_litlen_codes: &[(u32, u8)],
    fixed_dist_codes: &[(u32, u8)],
) -> (u64, BlockCandidate) {
    let stored_cost = stored_bit_cost(tokens, start_pos);
    let fixed_cost = fixed_bit_cost(tokens, fixed_litlen_codes, fixed_dist_codes);
    let rebuilt = plan_dynamic(tokens.to_vec(), &litlen_freq, &dist_freq);
    let pruned = plan_dynamic_pruned(tokens.to_vec(), litlen_freq, dist_freq);

    let mut best_cost = fixed_cost;
    let mut best = BlockCandidate::Fixed;
    if let Some(sc) = stored_cost {
        if sc <= best_cost {
            best_cost = sc;
            best = BlockCandidate::Stored;
        }
    }
    if rebuilt.bit_cost < best_cost {
        best_cost = rebuilt.bit_cost;
        best = BlockCandidate::Dynamic(rebuilt);
    }
    if pruned.bit_cost < best_cost {
        best_cost = pruned.bit_cost;
        best = BlockCandidate::Dynamic(pruned);
    }
    (best_cost, best)
}

fn emit_block_candidate(w: &mut BitWriter, tokens: &[Token], candidate: BlockCandidate, is_final: bool) -> Result<(), Error> {
    match candidate {
        BlockCandidate::Stored => emit_stored(w, tokens, is_final),
        BlockCandidate::Fixed => emit_fixed(w, tokens, is_final)?,
        BlockCandidate::Dynamic(plan) => emit_dynamic_plan(w, &plan, is_final)?,
    }
    Ok(())
}

/// A candidate that splits one block into several, each independently
/// optimised. `None` if splitting wasn't attempted (block too small) or
/// didn't beat the unsplit cost.
struct SplitPlan {
    /// Token ranges `[start, end)` for each resulting sub-block, in order.
    ranges: Vec<(usize, usize)>,
    #[allow(dead_code)] // kept for diagnostics; the winner-selection comparison already happened in `plan_split`
    total_bit_cost: u64,
}

/// Try splitting `tokens` into multiple independently-optimised blocks
/// (see `research/block-splitting.md`). Returns `None` if the block is too
/// small to consider, or if no split beats keeping it as one block.
/// `start_pos` is the real output bit position (needed for the first
/// sub-block's stored-candidate cost; later sub-blocks' positions are
/// computed relative to it as candidate costs are summed, though since all
/// candidates other than stored are position-independent this only matters
/// for an all-literal split, an uncommon case).
fn plan_split(
    tokens: &[Token],
    start_pos: u64,
    fixed_litlen_codes: &[(u32, u8)],
    fixed_dist_codes: &[(u32, u8)],
    unsplit_cost: u64,
) -> Option<SplitPlan> {
    if tokens.len() < SPLIT_MIN_TOKENS {
        return None;
    }

    let mut split_points = Vec::new();
    find_splits(tokens, 0, tokens.len(), SPLIT_MAX_DEPTH, &mut split_points);
    if split_points.is_empty() {
        return None;
    }
    split_points.sort_unstable();
    split_points.dedup();

    let mut ranges = Vec::with_capacity(split_points.len() + 1);
    let mut prev = 0;
    for &p in &split_points {
        ranges.push((prev, p));
        prev = p;
    }
    ranges.push((prev, tokens.len()));

    let mut total: u64 = 0;
    let mut pos = start_pos;
    for &(a, b) in &ranges {
        let litlen_freq = range_litlen_freq_for_block(tokens, a, b);
        let mut dist_freq = [0u32; DIST_SYMBOLS];
        for t in &tokens[a..b] {
            if let Token::Match { distance, .. } = t {
                dist_freq[classify_distance(*distance).0] += 1;
            }
        }
        let (cost, _) = best_block_candidate(&tokens[a..b], litlen_freq, dist_freq, pos, fixed_litlen_codes, fixed_dist_codes);
        total += cost;
        pos += cost; // approximate — good enough for the stored-candidate alignment heuristic
    }

    if total < unsplit_cost {
        Some(SplitPlan { ranges, total_bit_cost: total })
    } else {
        None
    }
}

fn emit_split_plan(
    w: &mut BitWriter,
    tokens: &[Token],
    plan: &SplitPlan,
    is_final_block: bool,
    fixed_litlen_codes: &[(u32, u8)],
    fixed_dist_codes: &[(u32, u8)],
) -> Result<(), Error> {
    let n = plan.ranges.len();
    for (i, &(a, b)) in plan.ranges.iter().enumerate() {
        let sub_is_final = is_final_block && i + 1 == n;
        let litlen_freq = range_litlen_freq_for_block(tokens, a, b);
        let mut dist_freq = [0u32; DIST_SYMBOLS];
        for t in &tokens[a..b] {
            if let Token::Match { distance, .. } = t {
                dist_freq[classify_distance(*distance).0] += 1;
            }
        }
        let start_pos = w.bits_written();
        let (_, candidate) = best_block_candidate(&tokens[a..b], litlen_freq, dist_freq, start_pos, fixed_litlen_codes, fixed_dist_codes);
        emit_block_candidate(w, &tokens[a..b], candidate, sub_is_final)?;
    }
    Ok(())
}

/// Re-encode a full Deflate stream, choosing the best of the available
/// candidates for each block. Blocks at or below [`SMALL_BLOCK_GATE`]
/// tokens skip the search entirely and are emitted as fixed Huffman
/// (defluff `0x4071db`).
///
/// All 5 candidates from `research/design-v1.md` are wired: stored, fixed,
/// dynamic-rebuilt, dynamic-pruned, dynamic-original-fallback.
pub fn optimise_deflate_stream(bits: &[u8]) -> Result<Vec<u8>, Error> {
    let parsed = parse_deflate(bits)?;
    let mut out = BitWriter::new();
    let litlen_lengths = fixed_litlen_lengths();
    let dist_lengths = fixed_dist_lengths();
    let litlen_codes = crate::huffman::canonical_codes(&litlen_lengths);
    let dist_codes = crate::huffman::canonical_codes(&dist_lengths);

    enum Winner {
        Stored,
        Fixed,
        Dynamic(DynamicPlan),
        DynamicOriginalFallback,
        Split(SplitPlan),
    }

    for block in &parsed.blocks {
        if block.tokens.len() <= SMALL_BLOCK_GATE {
            emit_fixed(&mut out, &block.tokens, block.is_final)?;
            continue;
        }

        let start_pos = out.bits_written();
        let stored_cost = stored_bit_cost(&block.tokens, start_pos);
        let fixed_cost = fixed_bit_cost(&block.tokens, &litlen_codes, &dist_codes);
        let rebuilt = plan_dynamic_rebuilt(block);
        let pruned = plan_dynamic_pruned(block.tokens.clone(), block.litlen_freq, block.dist_freq);
        let (fallback_start, fallback_end) = block.original_bit_range;
        let fallback_cost = fallback_end - fallback_start;

        // Ties favour the earlier candidate: stored < fixed < dynamic-rebuilt
        // < dynamic-pruned < dynamic-original-fallback (defluff's scan order
        // extended with deft4j's pruned candidate; the fallback is DeflOpt's
        // addition, so it sits last among ties by convention here). Split
        // (v1.2, see `research/block-splitting.md`) competes last, since it's
        // only even attempted once we know what it needs to beat.
        let mut best_cost = fixed_cost;
        let mut winner = Winner::Fixed;
        if let Some(sc) = stored_cost {
            if sc <= best_cost {
                best_cost = sc;
                winner = Winner::Stored;
            }
        }
        if rebuilt.bit_cost < best_cost {
            best_cost = rebuilt.bit_cost;
            winner = Winner::Dynamic(rebuilt);
        }
        if pruned.bit_cost < best_cost {
            best_cost = pruned.bit_cost;
            winner = Winner::Dynamic(pruned);
        }
        if fallback_cost < best_cost {
            best_cost = fallback_cost;
            winner = Winner::DynamicOriginalFallback;
        }
        if let Some(split) = plan_split(&block.tokens, start_pos, &litlen_codes, &dist_codes, best_cost) {
            winner = Winner::Split(split);
        }

        match winner {
            Winner::Stored => emit_stored(&mut out, &block.tokens, block.is_final),
            Winner::Fixed => emit_fixed(&mut out, &block.tokens, block.is_final)?,
            Winner::Dynamic(plan) => emit_dynamic_plan(&mut out, &plan, block.is_final)?,
            Winner::DynamicOriginalFallback => emit_dynamic_original_fallback(&mut out, &parsed, block),
            Winner::Split(plan) => emit_split_plan(&mut out, &block.tokens, &plan, block.is_final, &litlen_codes, &dist_codes)?,
        }
    }

    Ok(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_codes_match_rfc1951_example() {
        // RFC 1951 §3.2.2 worked example: lengths (3,3,3,3,3,2,4,4) for symbols A-H.
        let lengths = [3u8, 3, 3, 3, 3, 2, 4, 4];
        let codes = decode_codes(&lengths);
        assert_eq!(codes[5], (0b00, 2));
        assert_eq!(codes[0], (0b010, 3));
        assert_eq!(codes[6], (0b1110, 4));
    }

    #[test]
    fn resolve_match_handles_overlap_rle() {
        // distance=1, length=5 over "a" — classic RLE self-overlap: each
        // byte is copied from the position immediately before it, which for
        // i >= 1 is a byte this same match is still producing.
        let output = b"a".to_vec();
        let resolved = resolve_match(&output, 5, 1);
        assert_eq!(resolved, b"aaaaa");

        // distance=2, length=5 over "ab" — overlaps partway through.
        let output = b"ab".to_vec();
        let resolved = resolve_match(&output, 5, 2);
        assert_eq!(resolved, b"ababa");
    }

    /// Regression test for a real bug: sub-blocks produced by splitting
    /// need their own EOB (symbol 256) frequency counted, or their local
    /// Huffman tree assigns it a zero code length and emission fails with
    /// `Error::MissingCode(256)`. That error propagated up through
    /// `container::detect_and_optimise`'s `.ok()` and silently fell back
    /// to unmodified output — `optimise_deflate_stream` returning `Ok`
    /// with no savings looked identical to "nothing to optimise", masking
    /// a real encoder bug. This forces a large single block (above
    /// `SPLIT_MIN_TOKENS`) through the real pipeline and requires an `Ok`
    /// result *and* actual savings, so a similar regression can't hide
    /// behind a silently-swallowed error again.
    #[test]
    fn split_path_does_not_silently_fail_and_saves_bytes() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let data = std::fs::read("research/deflopt-methods.md").expect("run from crate root");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(&data).unwrap();
        let gz = encoder.finish().unwrap();
        let deflate = &gz[10..gz.len() - 8];

        let parsed = parse_deflate(deflate).unwrap();
        assert!(
            parsed.blocks[0].tokens.len() > SPLIT_MIN_TOKENS,
            "test fixture must be large enough to exercise splitting"
        );

        let optimised = optimise_deflate_stream(deflate).expect("must not error — see doc comment");
        assert!(
            optimised.len() < deflate.len(),
            "split should save real bytes on this file: {} -> {}",
            deflate.len(),
            optimised.len()
        );
    }

    #[test]
    fn empty_fixed_block_roundtrips_through_parser() {
        let mut w = BitWriter::new();
        w.write_bits(1, 1); // BFINAL
        w.write_bits(2, 1); // BTYPE = fixed
        // EOB (symbol 256, fixed code length 7, code 0000000)
        w.write_bits(7, 0);
        let bytes = w.into_bytes();

        let parsed = parse_deflate(&bytes).unwrap();
        assert_eq!(parsed.blocks.len(), 1);
        assert!(parsed.blocks[0].tokens.is_empty());
        assert!(parsed.blocks[0].is_final);
    }

    #[test]
    #[ignore] // diagnostic only, run with `cargo test diagnose_split -- --ignored --nocapture`
    fn diagnose_split_on_real_file() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let data = std::fs::read("research/deflopt-methods.md").expect("run from crate root");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(&data).unwrap();
        let gz = encoder.finish().unwrap();
        let deflate = &gz[10..gz.len() - 8];

        let parsed = parse_deflate(deflate).unwrap();
        let block = &parsed.blocks[0];
        eprintln!("tokens: {}", block.tokens.len());

        let mut split_points = Vec::new();
        find_splits(&block.tokens, 0, block.tokens.len(), SPLIT_MAX_DEPTH, &mut split_points);
        split_points.sort_unstable();
        split_points.dedup();
        eprintln!("split points found: {:?}", split_points);

        let litlen_lengths = fixed_litlen_lengths();
        let dist_lengths = fixed_dist_lengths();
        let litlen_codes = crate::huffman::canonical_codes(&litlen_lengths);
        let dist_codes = crate::huffman::canonical_codes(&dist_lengths);

        let rebuilt = plan_dynamic_rebuilt(block);
        eprintln!("unsplit dynamic_rebuilt cost: {}", rebuilt.bit_cost);

        let split = plan_split(&block.tokens, 0, &litlen_codes, &dist_codes, rebuilt.bit_cost);
        match &split {
            Some(s) => {
                eprintln!("split WINS: total_bit_cost={} ranges={:?}", s.total_bit_cost, s.ranges);
            }
            None => eprintln!("split did not beat unsplit cost"),
        }

        // Whole-entropy vs split-entropy at the top level, for comparison.
        let whole_entropy = estimate_bits(&range_litlen_freq(&block.tokens, 0, block.tokens.len()), block.tokens.len());
        eprintln!("whole-block entropy estimate: {whole_entropy:.0}");
        if let Some(&mid) = split_points.first() {
            let left = estimate_bits(&range_litlen_freq(&block.tokens, 0, mid), mid);
            let right = estimate_bits(&range_litlen_freq(&block.tokens, mid, block.tokens.len()), block.tokens.len() - mid);
            eprintln!("first split @ {mid}: left_entropy={left:.0} right_entropy={right:.0} sum={:.0}", left + right);
        }
    }

    #[test]
    #[ignore] // diagnostic only, run with `cargo test -- --ignored --nocapture`
    fn diagnose_candidate_choice_on_real_file() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let data = std::fs::read("research/deflopt-methods.md").expect("run from crate root");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(&data).unwrap();
        let gz = encoder.finish().unwrap();
        // Strip the 10-byte gzip header and 8-byte trailer to get the raw deflate payload.
        let deflate = &gz[10..gz.len() - 8];

        let parsed = parse_deflate(deflate).unwrap();
        eprintln!("blocks: {}", parsed.blocks.len());
        let litlen_lengths = fixed_litlen_lengths();
        let dist_lengths = fixed_dist_lengths();
        let litlen_codes = crate::huffman::canonical_codes(&litlen_lengths);
        let dist_codes = crate::huffman::canonical_codes(&dist_lengths);

        for (i, block) in parsed.blocks.iter().enumerate() {
            let (start, end) = block.original_bit_range;
            let original = end - start;
            let stored = stored_bit_cost(&block.tokens, 0);
            let fixed = fixed_bit_cost(&block.tokens, &litlen_codes, &dist_codes);
            let plan = plan_dynamic_rebuilt(block);
            eprintln!(
                "block {i}: tokens={} original={original} stored={stored:?} fixed={fixed} dynamic_rebuilt={} gate={}",
                block.tokens.len(),
                plan.bit_cost,
                block.tokens.len() <= SMALL_BLOCK_GATE
            );

            let header_bits: u64 = {
                let mut h = 3 + 5 + 5 + 4 + (plan.hclen as u64) * 3;
                for &(sym, extra_bits, _) in &plan.rle_tokens {
                    h += plan.clen_lengths[sym as usize] as u64 + extra_bits as u64;
                }
                h
            };
            let payload_bits = plan.bit_cost - header_bits;
            eprintln!(
                "  hlit={} hdist={} hclen={} header_bits={header_bits} payload_bits={payload_bits}",
                plan.hlit, plan.hdist, plan.hclen
            );

            // Top-5 most frequent litlen symbols and their assigned code length.
            let mut by_freq: Vec<(usize, u32)> = block
                .litlen_freq
                .iter()
                .enumerate()
                .filter(|&(_, &f)| f > 0)
                .map(|(s, &f)| (s, f))
                .collect();
            by_freq.sort_by_key(|&(_, f)| std::cmp::Reverse(f));
            for &(sym, freq) in by_freq.iter().take(5) {
                eprintln!("    litlen sym={sym} freq={freq} rebuilt_len={}", plan.litlen_lengths[sym]);
            }

            let ideal_bits: f64 = block
                .litlen_freq
                .iter()
                .filter(|&&f| f > 0)
                .map(|&f| {
                    let p = f as f64 / block.tokens.len().max(1) as f64;
                    -(f as f64) * p.log2()
                })
                .sum();
            eprintln!("  ideal_entropy_bits(litlen only, approx)={ideal_bits:.0}");

            let litlen_sum: u32 = block.litlen_freq.iter().sum();
            let dist_sum: u32 = block.dist_freq.iter().sum();
            eprintln!(
                "  litlen_freq_sum={litlen_sum} dist_freq_sum={dist_sum} tokens+1={}",
                block.tokens.len() + 1
            );

            let mut hist = [0u32; 16];
            for &l in &plan.litlen_lengths {
                hist[l as usize] += 1;
            }
            eprintln!("  litlen length histogram: {:?}", hist);

            // Actual litlen-only payload bits (excluding dist + extra bits),
            // to compare directly against ideal_entropy_bits.
            let litlen_codes_plan = crate::huffman::canonical_codes(&plan.litlen_lengths);
            let mut litlen_only_bits: u64 = 0;
            for t in &block.tokens {
                let sym = match t {
                    Token::Literal(b) => *b as usize,
                    Token::Match { length, uses_sym284, .. } => classify_length(*length, *uses_sym284).0,
                };
                litlen_only_bits += litlen_codes_plan[sym].1 as u64;
            }
            litlen_only_bits += litlen_codes_plan[256].1 as u64;
            eprintln!("  actual litlen-only bits={litlen_only_bits}");
        }
    }
}
