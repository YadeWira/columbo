//! Deterministic length-limited Huffman codes.
//!
//! Builds a standard Huffman tree, then repairs codes that exceed the
//! length limit using DeflOpt's histogram-shift method.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// A Huffman tree node with explicit child tracking.
#[derive(Debug, Clone, Eq, PartialEq)]
enum Tree {
    Leaf { symbol: u16 },
    Node { left: Box<Tree>, right: Box<Tree> },
}

/// Wrapper for priority queue ordering: min-heap by (weight, insertion_id).
#[derive(Debug, Clone, Eq, PartialEq)]
struct HeapItem {
    weight: u32,
    id: u32,
    tree: Tree,
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other.weight.cmp(&self.weight)  // Reverse: lower weight = higher priority
            .then_with(|| other.id.cmp(&self.id))  // Higher id first (later insertions)
    }
}
impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

/// Build length-limited Huffman code lengths.
///
/// # Arguments
/// * `freq` — slice of symbol frequencies (index = symbol)
/// * `max_bits` — maximum code length (15 or 7)
///
/// # Returns
/// `lengths[symbol]` = code length (0 for unused, 1..=max_bits)
pub fn build_lengths(freq: &[u32], max_bits: u32) -> Vec<u8> {
    let n = freq.len();
    let max_bits = max_bits as usize;
    let mut result = vec![0u8; n];

    let active: Vec<(usize, u32)> = freq.iter()
        .enumerate()
        .filter(|(_, f)| **f > 0)
        .map(|(i, &f)| (i, f))
        .collect();

    if active.is_empty() {
        return result;
    }
    if active.len() == 1 {
        result[active[0].0] = 1;
        return result;
    }

    // Build Huffman tree with explicit tree tracking
    let mut heap = BinaryHeap::new();
    for (i, (sym, f)) in active.iter().enumerate() {
        heap.push(HeapItem {
            weight: *f,
            id: i as u32,  // lower insertion id = lower priority tie-break
            tree: Tree::Leaf { symbol: *sym as u16 },
        });
    }

    let mut next_id = active.len() as u32;
    while heap.len() > 1 {
        let a = heap.pop().unwrap();
        let b = heap.pop().unwrap();
        heap.push(HeapItem {
            weight: a.weight + b.weight,
            id: next_id,
            tree: Tree::Node {
                left: Box::new(a.tree),
                right: Box::new(b.tree),
            },
        });
        next_id += 1;
    }

    // Traverse tree to compute depths
    if let Some(root) = heap.pop() {
        fn walk(tree: &Tree, depth: u32, lengths: &mut [u8]) {
            match tree {
                Tree::Leaf { symbol } => {
                    lengths[*symbol as usize] = depth as u8;
                }
                Tree::Node { left, right } => {
                    walk(left, depth + 1, lengths);
                    walk(right, depth + 1, lengths);
                }
            }
        }
        walk(&root.tree, 0, &mut result);
    }

    // Clamp and ensure minimum length 1
    for &(sym, _) in &active {
        if result[sym] == 0 { result[sym] = 1; }
    }

    // Repair overflow if needed
    repair_overflow(&mut result, max_bits, &active);

    result
}

/// Overflow repair: while any code exceeds max_bits, apply DeflOpt's
/// histogram shift: move two leaves from max_depth to max_depth-1,
/// which frees a slot at max_depth-1 for a previously over-length leaf.
fn repair_overflow(lengths: &mut [u8], max_bits: usize, active: &[(usize, u32)]) {
    let mut count = [0usize; 32];

    for &(sym, _) in active {
        let l = lengths[sym] as usize;
        if l > 0 && l < 32 {
            count[l] += 1;
        }
    }

    loop {
        let max_depth = (0..32).rev().find(|&d| count[d] > 0).unwrap_or(0);
        if max_depth <= max_bits {
            break;
        }

        // Find greatest occupied depth strictly below max_bits
        let mut bits = max_bits;
        while bits > 0 && count[bits] == 0 {
            bits -= 1;
        }
        if bits == 0 {
            break; // shouldn't happen
        }

        count[bits] -= 1;
        count[bits + 1] += 2;
        count[max_depth] -= 2;
        count[max_depth - 1] += 1;
    }

    // Sort symbols by decreasing frequency: most frequent gets shortest code
    let mut sorted: Vec<(usize, u32)> = active.to_vec();
    sorted.sort_by_key(|&(_, f)| std::cmp::Reverse(f));

    // Assign lengths: shortest codes to most frequent symbols
    let mut sym_iter = sorted.iter();
    let mut assigned = vec![0u8; lengths.len()];

    for depth in 1..=max_bits {
        for _ in 0..count[depth] {
            if let Some(&(sym, _)) = sym_iter.next() {
                assigned[sym] = depth as u8;
            }
        }
    }

    // Unassigned active symbols get max_bits
    for &(sym, _) in &sorted {
        if assigned[sym] == 0 {
            assigned[sym] = max_bits as u8;
        }
    }

    for &(sym, _) in active {
        lengths[sym] = assigned[sym].max(1);
    }
}

/// Compute canonical Deflate Huffman codes from code lengths.
pub fn canonical_codes(lengths: &[u8]) -> Vec<(u32, u8)> {
    let max_len = lengths.iter().copied().max().unwrap_or(0) as usize;
    if max_len == 0 {
        return vec![(0, 0); lengths.len()];
    }
    let mut bl_count = vec![0usize; max_len + 1];
    for &l in lengths { if l > 0 { bl_count[l as usize] += 1; } }
    let mut next_code = vec![0u32; max_len + 1];
    let mut code = 0u32;
    for bits in 1..=max_len {
        code = (code + bl_count[bits - 1] as u32) << 1;
        next_code[bits] = code;
    }
    let mut codes = vec![(0, 0); lengths.len()];
    for (sym, &len) in lengths.iter().enumerate() {
        if len > 0 {
            let rev = reverse_bits(next_code[len as usize], len);
            codes[sym] = (rev, len);
            next_code[len as usize] += 1;
        }
    }
    codes
}

fn reverse_bits(mut val: u32, bits: u8) -> u32 {
    let mut out = 0;
    for _ in 0..bits { out = (out << 1) | (val & 1); val >>= 1; }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() { assert!(build_lengths(&[], 15).is_empty()); }

    #[test]
    fn one_symbol() {
        let r = build_lengths(&[0, 5], 15);
        assert_eq!(r[0], 0); assert_eq!(r[1], 1);
    }

    #[test]
    fn deterministic() {
        let a = build_lengths(&[5, 5, 5], 15);
        let b = build_lengths(&[5, 5, 5], 15);
        assert_eq!(a, b);
    }

    #[test]
    fn within_limit() {
        let f = vec![1u32; 300];
        for &l in &build_lengths(&f, 15) {
            assert!(l == 0 || (1..=15).contains(&l));
        }
    }

    #[test]
    fn kraft_ok() {
        let f: Vec<u32> = (1..=20).collect();
        let l = build_lengths(&f, 15);
        let s: f64 = l.iter().filter(|&&x| x > 0).map(|&x| 2f64.powi(-(x as i32))).sum();
        assert!(s <= 1.0 + 1e-9, "Kraft ∑2^(-l)={}", s);
        assert!(s > 0.9, "Kraft too low: {}", s);
    }

    #[test]
    fn freq_ordering() {
        let f: Vec<u32> = (1..=20).collect();
        let l = build_lengths(&f, 15);
        // symbol 19 (freq=20) should have shorter or equal code vs symbol 0 (freq=1)
        assert!(l[19] <= l[0], "freq-20 len={} > freq-1 len={}", l[19], l[0]);
    }

    #[test]
    fn canonical() {
        let l = vec![2, 1, 3, 2];
        let c = canonical_codes(&l);
        assert_eq!(c[1].1, 1);
        assert_eq!(c[0].1, c[3].1);
        assert_ne!(c[0].0, c[3].0);
    }

    #[test]
    fn bitrev() {
        assert_eq!(reverse_bits(0b110, 3), 0b011);
        assert_eq!(reverse_bits(0b0001, 4), 0b1000);
    }
}
