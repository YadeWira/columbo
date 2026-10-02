// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Andrew C.E. Dent <https://github.com/ace-dent>

//! Slide block boundaries under the transmitted Huffman trees.
//!
//! A boundary changes neither the decoded bytes nor the shared history window,
//! so a token may join its neighbouring block whenever that block's trees can
//! code it. Holding both trees fixed leaves every header unchanged: a cheaper
//! cut saves payload bits outright. Boundary searches elsewhere plan fresh
//! trees for a few sampled cuts; this pass prices every token cut against the
//! trees actually selected, so it finds the shifts those samples pass over.

use std::sync::Arc;

use super::block::{reusable_original_bits, stored_block_bits};
use super::huffman::{FIXED_DISTANCE_CODE_LENGTHS, FIXED_LITERAL_CODE_LENGTHS};
use super::model::{ParsedBlock, PlannedBlock, Representation, SourceBlockType, Token};
use super::restore::token_cost;
use super::stop::SearchStop;

/// Every accepted move strictly lowers the payload, so sweeps terminate; the
/// cap only bounds work on a long chain of interacting boundaries.
const MAX_SWEEPS: usize = 16;
/// Poll the stop between boundaries and within long block pairs.
const STOP_POLL_TOKENS: usize = 1 << 16;

type Trees<'a> = (&'a [u8], &'a [u8]);

struct Slot<'a> {
    tokens: Arc<Vec<Token>>,
    plain: Arc<Vec<u8>>,
    /// Transmitted literal/length and distance code lengths; `None` when stored.
    trees: Option<Trees<'a>>,
    payload: u64,
    original_payload: u64,
}

fn payload_bits(tokens: &[Token], (literal, distances): Trees<'_>) -> Option<u64> {
    tokens.iter().try_fold(0_u64, |bits, &token| {
        bits.checked_add(token_cost(token, literal, distances)?)
    })
}

/// Find the cheapest cut of two adjacent token lists under fixed trees.
///
/// A feasible cut leaves both blocks nonempty and every token on a side whose
/// trees code it. Returns the new left length and both payloads when that cut
/// is strictly cheaper; ties keep the cut nearest the current one.
fn cheaper_cut(
    left: &[Token],
    right: &[Token],
    left_trees: Trees<'_>,
    right_trees: Trees<'_>,
    stop: &mut SearchStop<'_>,
) -> Option<(usize, u64, u64)> {
    // The current cut must lie inside the scanned range to be compared.
    if left.is_empty() || right.is_empty() {
        return None;
    }
    let n = left.len().checked_add(right.len())?;
    let at = |i: usize| {
        if i < left.len() {
            left[i]
        } else {
            right[i - left.len()]
        }
    };
    let left_cost = |token| token_cost(token, left_trees.0, left_trees.1);
    let right_cost = |token| token_cost(token, right_trees.0, right_trees.1);

    let first_left_gap = (0..n).find(|&i| left_cost(at(i)).is_none()).unwrap_or(n);
    let after_right_gap = (0..n)
        .rev()
        .find(|&i| right_cost(at(i)).is_none())
        .map_or(0, |i| i + 1);
    let lo = after_right_gap.max(1);
    let hi = first_left_gap.min(n.checked_sub(1)?);
    if lo >= hi {
        return None;
    }

    let mut prefix = (0..lo).try_fold(0_u64, |bits, i| bits.checked_add(left_cost(at(i))?))?;
    let mut suffix = (lo..n).try_fold(0_u64, |bits, i| bits.checked_add(right_cost(at(i))?))?;
    let current = left.len();
    let mut best: Option<(u64, usize, u64, u64)> = None;
    for cut in lo..=hi {
        if cut % STOP_POLL_TOKENS == 0 && stop.reached() {
            return None;
        }
        let total = prefix.checked_add(suffix)?;
        let better = best.map_or(true, |(bits, best_cut, _, _)| {
            (total, cut.abs_diff(current)) < (bits, best_cut.abs_diff(current))
        });
        if better {
            best = Some((total, cut, prefix, suffix));
        }
        if cut < hi {
            let token = at(cut);
            prefix = prefix.checked_add(left_cost(token)?)?;
            suffix = suffix.checked_sub(right_cost(token)?)?;
        }
    }
    let (_, cut, left_bits, right_bits) = best?;
    (cut != current).then_some((cut, left_bits, right_bits))
}

/// Split the concatenation of two slices at `cut` without infallible growth.
fn split_pair<T: Copy>(left: &[T], right: &[T], cut: usize) -> Option<(Vec<T>, Vec<T>)> {
    let total = left.len().checked_add(right.len())?;
    let mut first = Vec::new();
    let mut second = Vec::new();
    first.try_reserve_exact(cut).ok()?;
    second.try_reserve_exact(total.checked_sub(cut)?).ok()?;
    if cut <= left.len() {
        first.extend_from_slice(&left[..cut]);
        second.extend_from_slice(&left[cut..]);
        second.extend_from_slice(right);
    } else {
        first.extend_from_slice(left);
        first.extend_from_slice(right.get(..cut - left.len())?);
        second.extend_from_slice(&right[cut - left.len()..]);
    }
    Some((first, second))
}

/// Move Huffman block boundaries to cheaper token cuts under fixed trees.
///
/// Stored blocks and their boundaries stay in place. Every Huffman block keeps
/// its transmitted header and at least one token, so the parse that follows
/// retains the block layout later terminal methods require. The caller must
/// compare the complete emission because stored padding can absorb a saving.
pub(crate) fn plan_boundary_slide(
    blocks: &[ParsedBlock],
    strict: bool,
    stop: &mut SearchStop<'_>,
) -> Option<Vec<PlannedBlock>> {
    if blocks.len() < 2 {
        return None;
    }
    let mut slots = Vec::new();
    slots.try_reserve_exact(blocks.len()).ok()?;
    for block in blocks {
        let trees = match block.source_type {
            SourceBlockType::Stored => None,
            // Fixed trees cannot change, so the strict policy is decided here.
            _ if reusable_original_bits(block, 0, strict).is_none() => return None,
            SourceBlockType::Fixed => Some((
                &FIXED_LITERAL_CODE_LENGTHS[..],
                &FIXED_DISTANCE_CODE_LENGTHS[..],
            )),
            SourceBlockType::Dynamic => {
                let dynamic = block.original_dynamic.as_ref()?;
                Some((&dynamic.literal_lengths[..], &dynamic.distance_lengths[..]))
            }
        };
        let payload = match trees {
            Some(trees) => payload_bits(&block.tokens, trees)?,
            None => 0,
        };
        slots.push(Slot {
            tokens: Arc::clone(&block.tokens),
            plain: Arc::clone(&block.plain),
            trees,
            payload,
            original_payload: payload,
        });
    }

    let mut changed = false;
    'sweeps: for _ in 0..MAX_SWEEPS {
        let mut moved = false;
        for k in 0..slots.len() - 1 {
            if stop.reached() {
                break 'sweeps;
            }
            let (Some(left_trees), Some(right_trees)) = (slots[k].trees, slots[k + 1].trees) else {
                continue;
            };
            let Some((cut, left_bits, right_bits)) = cheaper_cut(
                &slots[k].tokens,
                &slots[k + 1].tokens,
                left_trees,
                right_trees,
                stop,
            ) else {
                continue;
            };
            let (left_tokens, right_tokens) =
                split_pair(&slots[k].tokens, &slots[k + 1].tokens, cut)?;
            let left_plain_len = left_tokens.iter().try_fold(0_usize, |total, token| {
                total.checked_add(token.decoded_len())
            })?;
            let (left_plain, right_plain) =
                split_pair(&slots[k].plain, &slots[k + 1].plain, left_plain_len)?;
            slots[k].tokens = Arc::new(left_tokens);
            slots[k].plain = Arc::new(left_plain);
            slots[k].payload = left_bits;
            slots[k + 1].tokens = Arc::new(right_tokens);
            slots[k + 1].plain = Arc::new(right_plain);
            slots[k + 1].payload = right_bits;
            moved = true;
            changed = true;
        }
        if !moved {
            break;
        }
    }
    if !changed {
        return None;
    }

    let mut plans = Vec::new();
    plans.try_reserve_exact(blocks.len()).ok()?;
    let mut alignment = 0_u8;
    for (block, slot) in blocks.iter().zip(slots) {
        let original = reusable_original_bits(block, alignment, strict);
        let (representation, bits) = if block.source_type == SourceBlockType::Stored {
            // Earlier savings can shift a stored block's padding.
            match original {
                Some(original) => (Representation::Original(original), original.len),
                None => (
                    Representation::Stored,
                    stored_block_bits(alignment, block.plain.len()),
                ),
            }
        } else if Arc::ptr_eq(&slot.tokens, &block.tokens) {
            let original = original?;
            (Representation::Original(original), original.len)
        } else {
            let bits = original?
                .len
                .checked_sub(slot.original_payload)?
                .checked_add(slot.payload)?;
            match &block.original_dynamic {
                Some(dynamic) => {
                    let mut dynamic = dynamic.try_clone()?;
                    dynamic.bits = bits;
                    (Representation::Dynamic(dynamic), bits)
                }
                None => (Representation::Fixed, bits),
            }
        };
        alignment = ((u64::from(alignment) + bits) & 7) as u8;
        plans.push(PlannedBlock {
            tokens: slot.tokens,
            plain: slot.plain,
            representation,
            bits,
            source_type: block.source_type,
        });
    }
    Some(plans)
}

#[cfg(test)]
mod tests;
