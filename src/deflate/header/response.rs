// SPDX-License-Identifier: MIT

//! Fit certified token spellings to proposed payload trees. A tree change can
//! lose on the current tokens yet win after matches respond to its code prices.

use super::{plan_for_advertised_lengths, token_bits, transitions_removed};
use crate::deflate::model::{
    canonical_length_encoding, ParsedBlock, PlannedBlock, Representation, Token,
    MAX_DYNAMIC_CODE_LENGTH_COUNT,
};
use crate::deflate::stop::SearchStop;

mod exchange;
pub(crate) use exchange::plan_length_exchange;

const STREAM_WORK: usize = 1 << 25;
const STREAM_PRICES: usize = 512;
const MENU_SIZE: usize = 256;
const MAX_PAYLOAD_TAX: i64 = 128;
const MAX_PLAIN: usize = 4096;
const MAX_TOKENS: usize = 1024;
const MAX_MATCHES: usize = 128;
const INF: u64 = u64::MAX / 4;

pub(crate) struct ResponseBudget {
    work_left: usize,
    prices_left: usize,
}

impl ResponseBudget {
    pub(crate) fn new() -> Self {
        Self {
            work_left: STREAM_WORK,
            prices_left: STREAM_PRICES,
        }
    }

    fn spend(&mut self, work: usize) -> Option<()> {
        if work > self.work_left {
            self.work_left = 0;
            return None;
        }
        self.work_left -= work;
        Some(())
    }
}

fn price(lengths: &[u8], symbol: usize) -> Option<u64> {
    lengths
        .get(symbol)
        .copied()
        .filter(|&l| l > 0)
        .map(u64::from)
}

fn match_price(token: Token, literal: &[u8], distance: &[u8]) -> Option<u64> {
    let Token::Match {
        length_symbol,
        distance_symbol,
        length_extra_bits,
        distance_extra_bits,
        ..
    } = token
    else {
        return None;
    };
    Some(
        price(literal, usize::from(length_symbol))?
            + price(distance, usize::from(distance_symbol))?
            + u64::from(length_extra_bits)
            + u64::from(distance_extra_bits),
    )
}

fn submatch(seed: Token, length: usize) -> Option<Token> {
    let Token::Match {
        distance,
        distance_symbol,
        distance_extra,
        distance_extra_bits,
        ..
    } = seed
    else {
        return None;
    };
    let (length_symbol, length_extra, length_extra_bits) =
        canonical_length_encoding(u16::try_from(length).ok()?)?;
    Some(Token::Match {
        length: length as u16,
        distance,
        length_symbol,
        length_extra,
        length_extra_bits,
        distance_symbol,
        distance_extra,
        distance_extra_bits,
    })
}

/// Exact shortest spelling for one existing match under unchanged code prices.
/// Missing symbols are forbidden, and every generated match stays inside this
/// interval at the original distance. The original token wins a cost tie.
fn spell(
    seed: Token,
    plain: &[u8],
    literal: &[u8],
    distance: &[u8],
    budget: &mut ResponseBudget,
    stop: &mut SearchStop<'_>,
    output: &mut Vec<Token>,
) -> Option<()> {
    let n = plain.len();
    if !(3..=258).contains(&n) || n != seed.decoded_len() || stop.reached() {
        return None;
    }
    // Charge array preparation and every DP edge, including absent symbols,
    // before starting an interval. A cutoff never exposes a partial spelling.
    budget.spend(3 * 259 + n * (n + 1) / 2)?;
    let mut matches = [None; 259];
    for (length, slot) in matches.iter_mut().enumerate().take(n + 1).skip(3) {
        let token = submatch(seed, length)?;
        *slot = match_price(token, literal, distance).map(|bits| (token, bits));
    }
    let mut costs = [INF; 259];
    // 1 emits a literal, 3..=258 a canonical submatch, 259 the exact source.
    let mut choices = [0_u16; 259];
    costs[n] = 0;
    for start in (0..n).rev() {
        if start & 15 == 0 && stop.reached() {
            return None;
        }
        if start == 0 {
            if let Some(bits) = match_price(seed, literal, distance) {
                costs[0] = bits;
                choices[0] = 259;
            }
        }
        if let Some(bits) = price(literal, usize::from(plain[start])) {
            let cost = bits + costs[start + 1];
            if cost < costs[start] {
                costs[start] = cost;
                choices[start] = 1;
            }
        }
        for (length, edge) in matches.iter().enumerate().take(n - start + 1).skip(3) {
            let Some((_, bits)) = edge else { continue };
            let cost = bits + costs[start + length];
            if cost < costs[start] {
                costs[start] = cost;
                choices[start] = length as u16;
            }
        }
    }
    output.try_reserve(n).ok()?;
    let mut start = 0;
    while start < n {
        let token = match choices[start] {
            1 => Token::Literal(plain[start]),
            259 => seed,
            length @ 3..=258 => matches[usize::from(length)]?.0,
            _ => return None,
        };
        start += token.decoded_len();
        output.push(token);
    }
    Some(())
}

fn response(
    block: &ParsedBlock,
    literal: &[u8],
    distance: &[u8],
    budget: &mut ResponseBudget,
    stop: &mut SearchStop<'_>,
) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    tokens.try_reserve_exact(block.plain.len()).ok()?;
    budget.spend(block.tokens.len())?;
    let mut offset = 0;
    for &token in block.tokens.iter() {
        let end = offset + token.decoded_len();
        match token {
            Token::Literal(_) => tokens.push(token),
            Token::Match { .. } => spell(
                token,
                block.plain.get(offset..end)?,
                literal,
                distance,
                budget,
                stop,
                &mut tokens,
            )?,
        }
        offset = end;
    }
    (offset == block.plain.len()).then_some(tokens)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Proposal {
    rank: i64,
    positions: [usize; 2],
}

fn proposals(
    block: &ParsedBlock,
    lengths: &[u8],
    middle: usize,
    budget: &mut ResponseBudget,
    stop: &mut SearchStop<'_>,
) -> Option<Vec<Proposal>> {
    let mut menu = Vec::new();
    menu.try_reserve_exact(MENU_SIZE + 1).ok()?;
    // Include the unchanged tree's exact response as the first candidate.
    menu.push(Proposal {
        rank: i64::MIN,
        positions: [0, 0],
    });
    for (offset, frequencies) in [
        (0, &block.literal_frequencies[..middle]),
        (
            middle,
            &block.distance_frequencies[..lengths.len().saturating_sub(middle).min(30)],
        ),
    ] {
        for a in 0..frequencies.len() {
            if stop.reached() || budget.spend(1).is_none() {
                return Some(menu);
            }
            let la = lengths[offset + a];
            if la == 0 {
                continue;
            }
            for b in a + 1..frequencies.len() {
                if budget.spend(1).is_none() {
                    return Some(menu);
                }
                let lb = lengths[offset + b];
                if lb == 0 || la == lb {
                    continue;
                }
                let tax = (i64::from(frequencies[a]) - i64::from(frequencies[b]))
                    * (i64::from(lb) - i64::from(la));
                if tax > MAX_PAYLOAD_TAX {
                    continue;
                }
                let positions = [offset + a, offset + b];
                let removed = transitions_removed(lengths, positions, [lb, la]);
                // These are admission/ranking heuristics, not lower bounds:
                // a response can recover more than the original payload tax.
                if removed < 0 {
                    continue;
                }
                let proposal = Proposal {
                    rank: tax - 3 * removed,
                    positions,
                };
                let at = menu.binary_search(&proposal).unwrap_or_else(|at| at);
                if at < MENU_SIZE {
                    menu.insert(at, proposal);
                    menu.truncate(MENU_SIZE);
                }
            }
        }
    }
    Some(menu)
}

/// Keep each proposed tree while fitting its certified payload. No rebuilding
/// from the new frequencies is required. Tree-only or spelling-only steps need
/// not improve; only an exactly priced complete block may replace its parent.
pub(crate) fn plan_header_response(
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
    let matches = block
        .tokens
        .iter()
        .filter(|t| matches!(t, Token::Match { .. }))
        .count();
    if !(1..=MAX_MATCHES).contains(&matches) {
        return None;
    }
    let middle = parent.literal_lengths.len();
    let count = middle.checked_add(parent.distance_lengths.len())?;
    let mut lengths = [0; MAX_DYNAMIC_CODE_LENGTH_COUNT];
    let lengths = lengths.get_mut(..count)?;
    lengths[..middle].copy_from_slice(&parent.literal_lengths);
    lengths[middle..].copy_from_slice(&parent.distance_lengths);
    let menu = proposals(block, lengths, middle, budget, stop)?;
    let mut best = None;
    let mut best_bits = block.original?.len;
    for proposal in menu {
        if budget.prices_left == 0 || stop.reached() {
            break;
        }
        let [a, b] = proposal.positions;
        lengths.swap(a, b);
        let tokens = response(block, &lengths[..middle], &lengths[middle..], budget, stop);
        let candidate = tokens.and_then(|tokens| {
            // Unchanged spellings belong to the existing fixed-token searches.
            if tokens == *block.tokens {
                return None;
            }
            let payload = token_bits(&tokens, &lengths[..middle], &lengths[middle..])?;
            if payload + 17 >= best_bits {
                return None;
            }
            budget.prices_left -= 1;
            let plan =
                plan_for_advertised_lengths(&lengths[..middle], &lengths[middle..], payload)?;
            (plan.bits < best_bits && (!strict || plan.has_strictly_compatible_huffman_codes()))
                .then_some(PlannedBlock {
                    tokens: tokens.into(),
                    plain: block.plain.clone(),
                    bits: plan.bits,
                    representation: Representation::Dynamic(plan),
                    source_type: block.source_type,
                })
        });
        lengths.swap(a, b);
        if let Some(plan) = candidate {
            best_bits = plan.bits;
            best = Some(plan);
        }
        if budget.work_left == 0 {
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests;
