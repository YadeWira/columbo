// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Andrew C.E. Dent <https://github.com/ace-dent>

use super::*;
use crate::deflate::bitstream::BitWriter;
use crate::deflate::block::{emit_block, plan_block};
use crate::deflate::model::{
    canonical_length_encoding, count_frequencies, ParsedStream, DISTANCE_BASE, DISTANCE_EXTRA_BITS,
};
use crate::deflate::parse::parse_stream;
use crate::Options;

fn matched(length: u16, distance: u16) -> Token {
    let symbol = DISTANCE_BASE
        .iter()
        .rposition(|&base| base <= distance)
        .unwrap();
    let (length_symbol, length_extra, length_extra_bits) =
        canonical_length_encoding(length).unwrap();
    Token::Match {
        length,
        distance,
        length_symbol,
        length_extra,
        length_extra_bits,
        distance_symbol: symbol as u8,
        distance_extra: distance - DISTANCE_BASE[symbol],
        distance_extra_bits: DISTANCE_EXTRA_BITS[symbol],
    }
}

fn block(tokens: Vec<Token>, plain: Vec<u8>) -> ParsedBlock {
    let (literal_frequencies, distance_frequencies) = count_frequencies(&tokens);
    ParsedBlock {
        tokens: tokens.into(),
        plain: plain.into(),
        literal_frequencies,
        distance_frequencies,
        original_literal_lengths: None,
        original_distance_lengths: None,
        original_dynamic: None,
        original: None,
        source_splits: Vec::new(),
        source_type: SourceBlockType::Dynamic,
    }
}

fn literals(bytes: &[u8]) -> ParsedBlock {
    block(
        bytes.iter().copied().map(Token::Literal).collect(),
        bytes.to_vec(),
    )
}

/// Emit Columbo's ordinary plan for each block and parse the result, so every
/// block carries its transmitted trees and original bit range.
fn planned_stream(blocks: &[ParsedBlock]) -> (Vec<u8>, ParsedStream) {
    let mut writer = BitWriter::default();
    for (index, block) in blocks.iter().enumerate() {
        let alignment = (writer.bit_position() % 8) as u8;
        let plan = plan_block(
            block,
            alignment,
            &Options::default(),
            &mut SearchStop::never(),
        );
        emit_block(&mut writer, &[], &plan, index + 1 == blocks.len()).unwrap();
    }
    let data = writer.into_bytes();
    let parsed = parse_stream(&data, 1 << 20).unwrap();
    (data, parsed)
}

/// Emit the slid plans, check their bit accounting and decoded identity, and
/// return the emitted bit count.
fn emit_checked(parent: &[u8], source: &ParsedStream, plans: &[PlannedBlock]) -> u64 {
    let mut writer = BitWriter::default();
    for (index, plan) in plans.iter().enumerate() {
        emit_block(&mut writer, parent, plan, index + 1 == plans.len()).unwrap();
    }
    let bits = writer.bit_position();
    assert_eq!(bits, plans.iter().map(|plan| plan.bits).sum::<u64>());
    let output = parse_stream(&writer.into_bytes(), 1 << 20).unwrap();
    let decoded = |stream: &ParsedStream| -> Vec<u8> {
        stream
            .blocks
            .iter()
            .flat_map(|block| block.plain.iter().copied())
            .collect()
    };
    assert_eq!(decoded(&output), decoded(source));
    assert_eq!(output.blocks.len(), source.blocks.len());
    for (after, before) in output.blocks.iter().zip(&source.blocks) {
        assert_eq!(after.source_type, before.source_type);
        assert_eq!(
            after.original_dynamic.as_ref().map(|d| &d.literal_lengths),
            before.original_dynamic.as_ref().map(|d| &d.literal_lengths)
        );
        assert_eq!(
            after.original_dynamic.as_ref().map(|d| &d.distance_lengths),
            before
                .original_dynamic
                .as_ref()
                .map(|d| &d.distance_lengths)
        );
        assert!(!after.tokens.is_empty());
    }
    bits
}

#[test]
fn a_literal_run_moves_to_the_block_that_codes_it_cheaper() {
    let mut left = vec![b'a'; 733];
    left.extend(std::iter::repeat(b'z').take(291));
    let (data, source) = planned_stream(&[literals(&left), literals(&[b'z'; 1_024])]);

    let plans = plan_boundary_slide(&source.blocks, true, &mut SearchStop::never())
        .expect("the z run is cheaper under the right-hand tree");
    let bits = emit_checked(&data, &source, &plans);
    assert!(bits < source.meaningful_bits);
    assert_eq!(plans[0].plain.as_slice(), &[b'a'; 733]);
    assert_eq!(plans[1].plain.len(), 291 + 1_024);

    // The result is a fixed point, and an expired stop changes nothing.
    let (slid, again) = {
        let mut writer = BitWriter::default();
        for (index, plan) in plans.iter().enumerate() {
            emit_block(&mut writer, &data, plan, index + 1 == plans.len()).unwrap();
        }
        let slid = writer.into_bytes();
        let parsed = parse_stream(&slid, 1 << 20).unwrap();
        (slid, parsed)
    };
    assert!(!slid.is_empty());
    assert!(plan_boundary_slide(&again.blocks, true, &mut SearchStop::never()).is_none());
    assert!(plan_boundary_slide(&source.blocks, true, &mut SearchStop::always()).is_none());
}

#[test]
fn stored_boundaries_never_move() {
    let mut left = vec![b'a'; 733];
    left.extend(std::iter::repeat(b'z').take(291));
    let mut stored = literals(&(0..=255).collect::<Vec<u8>>());
    stored.source_type = SourceBlockType::Stored;
    let (_, source) = planned_stream(&[literals(&left), stored, literals(&[b'z'; 1_024])]);
    assert_eq!(source.blocks[1].source_type, SourceBlockType::Stored);
    assert!(plan_boundary_slide(&source.blocks, true, &mut SearchStop::never()).is_none());
}

/// Tokens and decoded bytes for a pseudo-random stream with short-distance
/// matches, so both sides use literal, length and distance codes.
fn random_tokens(seed: &mut u64, count: usize) -> (Vec<Token>, Vec<u8>) {
    let mut next = || {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    };
    let alphabet = 2 + next() % 6;
    let mut tokens = Vec::new();
    let mut plain = Vec::new();
    while tokens.len() < count {
        if plain.len() >= 8 && next() % 3 == 0 {
            let distance = 1 + (next() % plain.len().min(40) as u64) as u16;
            let length = 3 + (next() % 20) as u16;
            for _ in 0..length {
                plain.push(plain[plain.len() - usize::from(distance)]);
            }
            tokens.push(matched(length, distance));
        } else {
            let byte = b'a' + (next() % alphabet) as u8;
            plain.push(byte);
            tokens.push(Token::Literal(byte));
        }
    }
    (tokens, plain)
}

#[test]
fn two_block_slides_match_an_exhaustive_cut_oracle() {
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let (mut slid, mut respelled) = (0, 0);
    for round in 0..300 {
        let count = 20 + round % 180;
        let (tokens, plain) = random_tokens(&mut seed, count);
        let split = 1 + round * 7 % (count - 1);
        let decoded_split: usize = tokens[..split].iter().map(|t| t.decoded_len()).sum();
        let blocks = [
            block(tokens[..split].to_vec(), plain[..decoded_split].to_vec()),
            block(tokens[split..].to_vec(), plain[decoded_split..].to_vec()),
        ];
        let (data, source) = planned_stream(&blocks);
        let result = plan_boundary_slide(&source.blocks, true, &mut SearchStop::never());
        if source.blocks.len() != 2
            || source
                .blocks
                .iter()
                .any(|block| block.source_type == SourceBlockType::Stored)
        {
            assert!(result.is_none());
            continue;
        }

        let trees = |block: &ParsedBlock| match &block.original_dynamic {
            Some(dynamic) => (
                dynamic.literal_lengths.clone(),
                dynamic.distance_lengths.clone(),
            ),
            None => (
                FIXED_LITERAL_CODE_LENGTHS.to_vec(),
                FIXED_DISTANCE_CODE_LENGTHS.to_vec(),
            ),
        };
        let trees = [trees(&source.blocks[0]), trees(&source.blocks[1])];
        let saving = exhaustive_slide_saving(&source.blocks, &trees);
        match result {
            None => assert_eq!(saving, 0, "round {round}"),
            Some(plans) => {
                let bits = emit_checked(&data, &source, &plans);
                assert_eq!(source.meaningful_bits - bits, saving, "round {round}");
                slid += 1;
                let before: usize = source.blocks.iter().map(|b| b.tokens.len()).sum();
                let after: usize = plans.iter().map(|plan| plan.tokens.len()).sum();
                respelled += usize::from(after > before);
            }
        }
    }
    assert!(slid > 30, "only {slid} streams exercised a slide");
    assert!(
        respelled > 30,
        "only {respelled} slides respelled a joining match"
    );
}

#[test]
fn a_match_the_neighbour_cannot_code_joins_it_as_literals() {
    // The left tree codes only `a` and end-of-block, so the leading match of
    // the right block can join it only as three `a` literals. Rare literals
    // on the right make that match's own length code expensive there.
    let left = literals(&[b'a'; 1_000]);
    let mut tokens = vec![matched(3, 1)];
    let mut plain = vec![b'a'; 3];
    for &byte in std::iter::repeat(&b'z').take(1_000).chain(b"bcdefgh") {
        tokens.push(Token::Literal(byte));
        plain.push(byte);
    }
    let (data, source) = planned_stream(&[left, block(tokens, plain)]);
    assert!(source.blocks[0]
        .original_dynamic
        .as_ref()
        .is_some_and(|dynamic| dynamic.literal_lengths.iter().skip(257).all(|&n| n == 0)));

    let plans = plan_boundary_slide(&source.blocks, true, &mut SearchStop::never())
        .expect("the match is cheaper as left-hand literals");
    let bits = emit_checked(&data, &source, &plans);
    assert!(bits < source.meaningful_bits);
    assert_eq!(plans[0].plain.as_slice(), &[b'a'; 1_003]);
    assert!(plans[0]
        .tokens
        .iter()
        .all(|&token| token == Token::Literal(b'a')));
}

/// Repeat the slide's move rule by brute force: try every cut of the pair,
/// price each token at home or joining (where a match may be spelled as its
/// literals), keep the cheapest cut nearest the current one, and stop when no
/// cut is strictly cheaper. Returns the total payload saving.
fn exhaustive_slide_saving(blocks: &[ParsedBlock], trees: &[(Vec<u8>, Vec<u8>); 2]) -> u64 {
    let spelled = |block: &ParsedBlock| -> Vec<(Token, Vec<u8>)> {
        let mut at = 0;
        block
            .tokens
            .iter()
            .map(|&token| {
                let bytes = block.plain[at..at + token.decoded_len()].to_vec();
                at += token.decoded_len();
                (token, bytes)
            })
            .collect()
    };
    let mut sides = [spelled(&blocks[0]), spelled(&blocks[1])];
    // Bits and literal choice of a token on `side`, home or joining.
    let price = |token: Token, bytes: &[u8], side: usize, joining: bool| {
        let (literal, distance) = &trees[side];
        let own = token_cost(token, literal, distance);
        if !joining || matches!(token, Token::Literal(_)) {
            return own.map(|bits| (bits, false));
        }
        let literals = bytes.iter().try_fold(0, |bits, &byte| {
            Some(bits + token_cost(Token::Literal(byte), literal, distance)?)
        });
        match (own, literals) {
            (Some(own), Some(literals)) if literals < own => Some((literals, true)),
            (Some(own), _) => Some((own, false)),
            (None, literals) => literals.map(|bits| (bits, true)),
        }
    };
    let mut saving = 0;
    for _ in 0..16 {
        let home = sides[0].len();
        let all: Vec<(Token, Vec<u8>, usize)> = sides[0]
            .iter()
            .map(|(t, b)| (*t, b.clone(), 0))
            .chain(sides[1].iter().map(|(t, b)| (*t, b.clone(), 1)))
            .collect();
        let cost = |cut: usize| -> Option<u64> {
            all.iter()
                .enumerate()
                .try_fold(0, |bits, (index, (token, bytes, from))| {
                    let side = usize::from(index >= cut);
                    Some(bits + price(*token, bytes, side, side != *from)?.0)
                })
        };
        let current = cost(home).unwrap();
        let Some((best, cut)) = (1..all.len())
            .filter_map(|cut| Some((cost(cut)?, cut)))
            .min_by_key(|&(bits, cut)| (bits, cut.abs_diff(home), cut))
        else {
            break;
        };
        if best >= current {
            break;
        }
        saving += current - best;
        let mut next = [Vec::new(), Vec::new()];
        for (index, (token, bytes, from)) in all.into_iter().enumerate() {
            let side = usize::from(index >= cut);
            if price(token, &bytes, side, side != from).unwrap().1 {
                next[side].extend(bytes.iter().map(|&byte| (Token::Literal(byte), vec![byte])));
            } else {
                next[side].push((token, bytes));
            }
        }
        sides = next;
    }
    saving
}
