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
//! A joining match the neighbour cannot code, or codes expensively, may be
//! spelled as its decoded literals instead; equal bytes need no match proof.

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

/// A token's payload bits in one block, and whether it is spelled as literals.
///
/// A token keeps its own spelling in its own block. A joining match takes the
/// cheaper of its own spelling and its decoded bytes as literals, when the
/// block's trees code either; an equal price keeps the match.
fn price(token: Token, bytes: &[u8], trees: Trees<'_>, joining: bool) -> Option<(u64, bool)> {
    let own = token_cost(token, trees.0, trees.1);
    if !joining || matches!(token, Token::Literal(_)) {
        return own.map(|bits| (bits, false));
    }
    let literals = bytes.iter().try_fold(0_u64, |bits, &byte| {
        bits.checked_add(token_cost(Token::Literal(byte), trees.0, trees.1)?)
    });
    match (own, literals) {
        (Some(own), Some(literals)) if literals < own => Some((literals, true)),
        (Some(own), _) => Some((own, false)),
        (None, literals) => literals.map(|bits| (bits, true)),
    }
}

/// Two adjacent Huffman blocks with the trees each transmits.
struct Pair<'a> {
    left: &'a [Token],
    left_plain: &'a [u8],
    right: &'a [Token],
    right_plain: &'a [u8],
    left_trees: Trees<'a>,
    right_trees: Trees<'a>,
}

impl<'a> Pair<'a> {
    /// Every token in order with its decoded bytes and whether it starts in
    /// the left block. An item is `None` if a token overruns its block.
    fn walk(&self) -> impl Iterator<Item = Option<(Token, &'a [u8], bool)>> + 'a {
        let side = |tokens: &'a [Token], plain: &'a [u8], from_left: bool| {
            tokens.iter().scan(0_usize, move |at, &token| {
                let start = *at;
                *at = start.saturating_add(token.decoded_len());
                Some(plain.get(start..*at).map(|bytes| (token, bytes, from_left)))
            })
        };
        side(self.left, self.left_plain, true).chain(side(self.right, self.right_plain, false))
    }

    fn left_price(&self, token: Token, bytes: &[u8], from_left: bool) -> Option<(u64, bool)> {
        price(token, bytes, self.left_trees, !from_left)
    }

    fn right_price(&self, token: Token, bytes: &[u8], from_left: bool) -> Option<(u64, bool)> {
        price(token, bytes, self.right_trees, from_left)
    }

    /// Rebuild both token lists for `cut`, spelling each joining token as its
    /// price chose.
    fn split(&self, cut: usize) -> Option<(Vec<Token>, Vec<Token>)> {
        let mut first = Vec::new();
        let mut second = Vec::new();
        for (index, item) in self.walk().enumerate() {
            let (token, bytes, from_left) = item?;
            let (side, (_, literals)) = if index < cut {
                (&mut first, self.left_price(token, bytes, from_left)?)
            } else {
                (&mut second, self.right_price(token, bytes, from_left)?)
            };
            if literals {
                side.try_reserve(bytes.len()).ok()?;
                side.extend(bytes.iter().copied().map(Token::Literal));
            } else {
                side.try_reserve(1).ok()?;
                side.push(token);
            }
        }
        Some((first, second))
    }
}

/// Find the cheapest cut of two adjacent blocks under their fixed trees.
///
/// A feasible cut leaves both blocks nonempty and every token on a side that
/// can price it. Returns the new left token count and both payloads when that
/// cut is strictly cheaper; ties keep the cut nearest the current one.
fn cheaper_cut(pair: &Pair<'_>, stop: &mut SearchStop<'_>) -> Option<(usize, u64, u64)> {
    let current = pair.left.len();
    // The current cut must lie inside the scanned range to be compared.
    if current == 0 || pair.right.is_empty() {
        return None;
    }
    let n = current.checked_add(pair.right.len())?;
    let (mut lo, mut hi) = (1, n - 1);
    for (index, item) in pair.walk().enumerate() {
        let (token, bytes, from_left) = item?;
        if from_left {
            if pair.right_price(token, bytes, true).is_none() {
                lo = index + 1;
            }
        } else if pair.left_price(token, bytes, false).is_none() {
            hi = hi.min(index);
            break;
        }
    }
    if lo >= hi {
        return None;
    }

    let (mut prefix, mut suffix) = (0_u64, 0_u64);
    for (index, item) in pair.walk().enumerate() {
        let (token, bytes, from_left) = item?;
        if index < lo {
            prefix = prefix.checked_add(pair.left_price(token, bytes, from_left)?.0)?;
        } else {
            suffix = suffix.checked_add(pair.right_price(token, bytes, from_left)?.0)?;
        }
    }
    let mut walk = pair.walk().skip(lo);
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
            let (token, bytes, from_left) = walk.next()??;
            prefix = prefix.checked_add(pair.left_price(token, bytes, from_left)?.0)?;
            suffix = suffix.checked_sub(pair.right_price(token, bytes, from_left)?.0)?;
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
            let pair = Pair {
                left: &slots[k].tokens,
                left_plain: &slots[k].plain,
                right: &slots[k + 1].tokens,
                right_plain: &slots[k + 1].plain,
                left_trees,
                right_trees,
            };
            let Some((cut, left_bits, right_bits)) = cheaper_cut(&pair, stop) else {
                continue;
            };
            let (left_tokens, right_tokens) = pair.split(cut)?;
            let left_plain_len = left_tokens.iter().try_fold(0_usize, |total, token| {
                total.checked_add(token.decoded_len())
            })?;
            let (left_plain, right_plain) =
                split_pair(pair.left_plain, pair.right_plain, left_plain_len)?;
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
