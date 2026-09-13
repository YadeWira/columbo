// SPDX-License-Identifier: MIT

//! Move a length code to an absent symbol, then repair its certified payload.
//! Price the header first and bound each interval by its cheapest bits/byte.

use super::{
    match_families, match_price, plan_for_advertised_lengths, price, response, token_bits,
    ResponseBudget, MAX_MATCHES, MAX_PLAIN, MAX_TOKENS,
};
use crate::deflate::model::{DynamicPlan, ParsedBlock, PlannedBlock, Representation, Token};
use crate::deflate::stop::SearchStop;

// k used length symbols and 29-k absent symbols yield at most 14*15 pairs.
const MAX_PROPOSALS: usize = 14 * 15;

struct Interval {
    seed: Token,
    literal_min: Option<u64>,
}

struct Proposal {
    bound: u64,
    from: usize,
    to: usize,
    header: DynamicPlan,
}

impl Proposal {
    fn key(&self) -> (u64, u64, usize, usize) {
        (self.bound, self.header.bits, self.from, self.to)
    }
}

/// Every legal edge costs at least its width times the smallest edge ratio.
/// Ignoring exact coverage and literal positions can only lower the price.
fn interval_bound(interval: &Interval, literal: &[u8], distance: &[u8]) -> Option<u64> {
    let n = interval.seed.decoded_len();
    let mut ratio = interval.literal_min.map(|bits| (bits, 1_u64));
    let mut consider = |bits: u64, width: usize| {
        let width = width as u64;
        if ratio.map_or(true, |(b, w)| bits * w < b * width) {
            ratio = Some((bits, width));
        }
    };
    // The exact source edge may include relaxed length-258 spelling, which
    // is deliberately absent from the generated canonical edges below.
    if let Some(bits) = match_price(interval.seed, literal, distance) {
        consider(bits, n);
    }
    // Equal-cost edges have their lowest bits/byte at the family's last width.
    for family in match_families(interval.seed, literal, distance) {
        consider(family.bits, family.last);
    }
    let (bits, width) = ratio?;
    Some((n as u64 * bits).div_ceil(width))
}

fn intervals(block: &ParsedBlock, literal: &[u8]) -> Option<(Vec<Interval>, u64)> {
    let mut intervals = Vec::new();
    intervals.try_reserve_exact(MAX_MATCHES).ok()?;
    let mut fixed = price(literal, 256)?;
    let mut offset = 0;
    for &seed in block.tokens.iter() {
        let end = offset + seed.decoded_len();
        match seed {
            Token::Literal(value) => fixed += price(literal, usize::from(value))?,
            Token::Match { .. } => intervals.push(Interval {
                seed,
                literal_min: block
                    .plain
                    .get(offset..end)?
                    .iter()
                    .filter_map(|&value| price(literal, usize::from(value)))
                    .min(),
            }),
        }
        offset = end;
    }
    (offset == block.plain.len()).then_some((intervals, fixed))
}

/// The donor and recipient are length symbols only: literal prices, distance
/// prices, and all match certificates remain fixed. Both advertised spans are
/// fully priced before the exact token solve, and only their cheaper header
/// needs to survive because they define the same payload tree.
/// The recipient can remain unused when its position makes the header cheaper.
pub(crate) fn plan_length_exchange(
    block: &ParsedBlock,
    strict: bool,
    budget: &mut ResponseBudget,
    stop: &mut SearchStop<'_>,
) -> Option<PlannedBlock> {
    if stop.reached()
        || budget.prices_left == 0
        || budget.work_left == 0
        || block.plain.len() > MAX_PLAIN
        || block.tokens.len() > MAX_TOKENS
    {
        return None;
    }
    let parent = block.original_dynamic.as_ref()?;
    if strict && !parent.has_strictly_compatible_huffman_codes() {
        return None;
    }
    let count = block
        .tokens
        .iter()
        .filter(|t| matches!(t, Token::Match { .. }))
        .count();
    if !(1..=MAX_MATCHES).contains(&count) {
        return None;
    }
    budget.spend(block.plain.len() + block.tokens.len())?;
    let (intervals, fixed) = intervals(block, &parent.literal_lengths)?;
    let mut literal = [0; 286];
    literal[..parent.literal_lengths.len()].copy_from_slice(&parent.literal_lengths);
    let distance = &parent.distance_lengths;
    let mut menu = Vec::new();
    menu.try_reserve_exact(MAX_PROPOSALS).ok()?;
    let mut best_bits = block.original?.len;
    'proposals: for from in 257..286 {
        if literal[from] == 0 || block.literal_frequencies[from] == 0 {
            continue;
        }
        for to in 257..286 {
            if stop.reached() || budget.prices_left == 0 || budget.spend(1).is_none() {
                break 'proposals;
            }
            if literal[to] != 0 {
                continue;
            }
            // Charge the lower-bound edge walks before generating this tree.
            if budget
                .spend(block.plain.len() + intervals.len() + literal.len())
                .is_none()
            {
                break 'proposals;
            }
            literal.swap(from, to);
            let bound = intervals.iter().try_fold(fixed, |sum, interval| {
                Some(sum + interval_bound(interval, &literal, distance)?)
            });
            let mut header: Option<DynamicPlan> = None;
            if let Some(bound) = bound.filter(|&bound| bound + 17 < best_bits) {
                let trimmed = literal.iter().rposition(|&l| l != 0)?.max(256) + 1;
                let retained = trimmed.max(parent.literal_lengths.len());
                for (i, span) in [trimmed, retained].into_iter().enumerate() {
                    if i == 1 && retained == trimmed {
                        continue;
                    }
                    if budget.prices_left == 0 || stop.reached() {
                        break;
                    }
                    budget.prices_left -= 1;
                    if let Some(plan) = plan_for_advertised_lengths(&literal[..span], distance, 0) {
                        if (!strict || plan.has_strictly_compatible_huffman_codes())
                            && header.as_ref().map_or(true, |h| plan.bits < h.bits)
                        {
                            header = Some(plan);
                        }
                    }
                }
                if let Some(header) = header.filter(|h| h.bits + bound < best_bits) {
                    menu.push(Proposal {
                        bound: header.bits + bound,
                        from,
                        to,
                        header,
                    });
                }
            }
            literal.swap(from, to);
        }
    }
    menu.sort_unstable_by_key(Proposal::key);
    let mut best = None;
    for proposal in menu {
        if proposal.bound >= best_bits || stop.reached() || budget.work_left == 0 {
            break;
        }
        let mut header = proposal.header;
        let Some(tokens) = response(block, &header.literal_lengths, distance, budget, stop) else {
            continue;
        };
        let payload = token_bits(&tokens, &header.literal_lengths, distance)?;
        header.bits += payload;
        if header.bits < best_bits {
            best_bits = header.bits;
            best = Some(PlannedBlock {
                tokens: tokens.into(),
                plain: block.plain.clone(),
                bits: best_bits,
                representation: Representation::Dynamic(header),
                source_type: block.source_type,
            });
        }
    }
    best
}

#[cfg(test)]
mod tests;
