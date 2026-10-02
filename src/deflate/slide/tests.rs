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
    let mut slid = 0;
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
        let (left_literal, left_distance) = trees(&source.blocks[0]);
        let (right_literal, right_distance) = trees(&source.blocks[1]);
        let all: Vec<Token> = source
            .blocks
            .iter()
            .flat_map(|block| block.tokens.iter().copied())
            .collect();
        let price = |cut: usize| -> Option<u64> {
            let left = all[..cut].iter().try_fold(0, |bits, &token| {
                Some(bits + token_cost(token, &left_literal, &left_distance)?)
            })?;
            let right = all[cut..].iter().try_fold(0, |bits, &token| {
                Some(bits + token_cost(token, &right_literal, &right_distance)?)
            })?;
            Some(left + right)
        };
        let current = price(source.blocks[0].tokens.len()).unwrap();
        let best = (1..all.len()).filter_map(price).min().unwrap();
        match result {
            None => assert_eq!(best, current, "round {round}"),
            Some(plans) => {
                let bits = emit_checked(&data, &source, &plans);
                assert_eq!(
                    source.meaningful_bits - bits,
                    current - best,
                    "round {round}"
                );
                slid += 1;
            }
        }
    }
    assert!(slid > 30, "only {slid} streams exercised a slide");
}
