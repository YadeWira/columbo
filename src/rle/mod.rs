//! Greedy RLE encoder for dynamic Deflate headers.
//! Ported from defluff 0.3.2 RE (`research/defluff-methods.md`, `0x404d00`).

use crate::huffman;

/// Deflate's code-length alphabet permutation order.
/// HCLEN indices are transmitted in this specific order.
pub const CLEN_PERMUTATION: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Pack concatenated literal/length and distance code-length arrays
/// into an RLE token stream using a greedy strategy (defluff method).
///
/// Returns a vector of `(symbol, extra_bits, extra_value)` tuples
/// representing the packed dynamic header body.
pub fn pack_lengths(litlen_lengths: &[u8], dist_lengths: &[u8]) -> Vec<(u16, u8, u16)> {
    let combined: Vec<u8> = litlen_lengths
        .iter()
        .chain(dist_lengths.iter())
        .copied()
        .collect();
    pack_slice(&combined)
}

/// Pack a single slice of code-length values into RLE tokens.
fn pack_slice(lengths: &[u8]) -> Vec<(u16, u8, u16)> {
    let mut tokens = Vec::new();
    let mut i = 0;
    let n = lengths.len();

    while i < n {
        let val = lengths[i];

        if val == 0 {
            // Count zero run
            let run_start = i;
            while i < n && lengths[i] == 0 {
                i += 1;
            }
            let mut run = i - run_start;

            // Symbol 18: 11-138 zeros (7 extra bits)
            while run >= 11 {
                let count = run.min(138);
                tokens.push((18, 7, (count - 11) as u16));
                run -= count;
            }
            // Symbol 17: 3-10 zeros (3 extra bits)
            while run >= 3 {
                let count = run.min(10);
                tokens.push((17, 3, (count - 3) as u16));
                run -= count;
            }
            // Remaining 1-2 zeros: explicit symbol 0
            for _ in 0..run {
                tokens.push((0, 0, 0));
            }
        } else {
            // Non-zero value
            let val = val;
            tokens.push((val as u16, 0, 0));
            i += 1;

            // Count repeat run
            let repeat_start = i;
            while i < n && lengths[i] == val {
                i += 1;
            }
            let mut repeats = i - repeat_start;

            // Symbol 16: 3-6 repeats (2 extra bits)
            while repeats >= 3 {
                let count = repeats.min(6);
                tokens.push((16, 2, (count - 3) as u16));
                repeats -= count;
            }
            // Remaining 1-2: explicit
            for _ in 0..repeats {
                tokens.push((val as u16, 0, 0));
            }
        }
    }

    tokens
}

/// Build code-length Huffman codes for an RLE token stream.
/// Returns (code_lengths[19], hlit, hdist, hclen).
pub fn build_code_length_tree(
    tokens: &[(u16, u8, u16)],
) -> (Vec<u8>, usize, usize, usize) {
    // Count frequencies of code-length symbols
    let mut freq = [0u32; 19];
    for &(sym, _, _) in tokens {
        freq[sym as usize] += 1;
    }

    // Build length-limited tree (max 7 bits)
    let lengths = huffman::build_lengths(&freq, 7);

    // Compute HCLEN: index of last non-zero in permutation order
    let hclen = CLEN_PERMUTATION
        .iter()
        .enumerate()
        .rev()
        .find(|(_, p)| lengths[**p] > 0)
        .map(|(i, _)| i + 1)
        .unwrap_or(4)
        .max(4);

    // HLIT and HDIST are computed by the caller from trimmed active spans
    (lengths, 0, 0, hclen)
}

/// Compute the cost in bits of an RLE token under given code-length lengths.
pub fn rle_cost(
    tokens: &[(u16, u8, u16)],
    clen_lengths: &[u8],
) -> u32 {
    let mut cost = 0u32;
    for &(sym, extra_bits, _) in tokens {
        let cl = clen_lengths[sym as usize] as u32;
        if cl == 0 {
            return u32::MAX; // Invalid: missing code
        }
        cost += cl + extra_bits as u32;
    }
    cost
}

/// Apply local strict replacements to an RLE stream: replace repeat
/// tokens with explicit values when cheaper. Uses the current code-length
/// tree for cost comparison.
pub fn optimise_rle(
    tokens: &[(u16, u8, u16)],
    clen_lengths: &[u8],
    original_lengths: &[u8], // the actual code-length values
) -> Vec<(u16, u8, u16)> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut pos = 0; // position in original_lengths

    for &(sym, extra_bits, extra_val) in tokens {
        if sym == 16 {
            // Repeat: count = extra_val + 3
            let count = extra_val as usize + 3;
            let val = original_lengths[pos - 1]; // previous decoded value

            let repeat_cost = clen_lengths[16] as u32 + 2u32;
            let explicit_cost = count as u32 * clen_lengths[val as usize] as u32;

            if explicit_cost < repeat_cost && clen_lengths[val as usize] > 0 {
                // Replace with explicit values
                for _ in 0..count {
                    out.push((val as u16, 0, 0));
                    pos += 1;
                }
            } else {
                out.push((sym, extra_bits, extra_val));
                pos += count;
            }
        } else if sym == 17 {
            let count = extra_val as usize + 3;
            let repeat_cost = clen_lengths[17] as u32 + 3u32;
            let explicit_cost = count as u32 * clen_lengths[0] as u32;

            if explicit_cost < repeat_cost && clen_lengths[0] > 0 {
                for _ in 0..count {
                    out.push((0, 0, 0));
                    pos += 1;
                }
            } else {
                out.push((sym, extra_bits, extra_val));
                pos += count;
            }
        } else if sym == 18 {
            let count = extra_val as usize + 11;
            let repeat_cost = clen_lengths[18] as u32 + 7u32;
            let explicit_cost = count as u32 * clen_lengths[0] as u32;

            if explicit_cost < repeat_cost && clen_lengths[0] > 0 {
                for _ in 0..count {
                    out.push((0, 0, 0));
                    pos += 1;
                }
            } else {
                out.push((sym, extra_bits, extra_val));
                pos += count;
            }
        } else {
            // Literal value or end marker
            out.push((sym, extra_bits, extra_val));
            pos += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_literals() {
        // All distinct values — no RLE possible
        let lengths = vec![1u8, 2, 3, 4, 5];
        let tokens = pack_slice(&lengths);
        assert_eq!(tokens.len(), 5);
        for (i, &(sym, eb, _)) in tokens.iter().enumerate() {
            assert_eq!(eb, 0);
            assert_eq!(sym as u8, lengths[i]);
        }
    }

    #[test]
    fn test_zero_runs() {
        let lengths = vec![0u8; 20];
        let tokens = pack_slice(&lengths);
        // 18 zeros: symbol 18 with extra bits 7 (20-11=9)
        // then 2 zeros: explicit
        // or: 18→18 max (138), then 17→17 (10), then explicit
        let total: usize = tokens
            .iter()
            .filter(|&&(s, _, _)| s == 0 || s == 17 || s == 18)
            .map(|&(_, eb, ev)| {
                match eb {
                    0 => 1,         // explicit zero
                    3 => ev as usize + 3,  // sym 17
                    7 => ev as usize + 11, // sym 18
                    _ => 0,
                }
            })
            .sum();
        assert_eq!(total, 20);
    }

    #[test]
    fn test_repeat_runs() {
        let lengths = vec![5u8; 10];
        let tokens = pack_slice(&lengths);
        // First 5 explicit, then sym 16 for 6 repeats (extra 2 bits)
        // Or: 5 explicit + symbol 16 (3+3=6) remaining → total 10 accounted
        let total: usize = tokens
            .iter()
            .map(|&(s, _eb, ev)| match s {
                16 => ev as usize + 3, // repeat count
                _ => 1,                // explicit value
            })
            .sum();
        assert_eq!(total, 10);
    }

    #[test]
    fn test_optimise_strict() {
        // Create a case where explicit is cheaper than repeat
        let lengths = vec![5u8; 6]; // 5 repeated 6 times
        let tokens = pack_slice(&lengths);
        // First: symbol 5 explicit (1 occurrence), then symbol 16 (5 repeats)

        // Build code-length tree
        let (clen_lengths, _, _, _) = build_code_length_tree(&tokens);

        // Optimise
        let opt = optimise_rle(&tokens, &clen_lengths, &lengths);
        let orig_cost = rle_cost(&tokens, &clen_lengths);
        let opt_cost = rle_cost(&opt, &clen_lengths);
        assert!(opt_cost <= orig_cost, "Optimised not cheaper: {} vs {}", opt_cost, orig_cost);
    }
}
