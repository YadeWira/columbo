// SPDX-License-Identifier: MIT

use super::*;

fn seed(length: usize) -> Token {
    submatch(
        Token::Match {
            length: 3,
            distance: 1,
            length_symbol: 257,
            length_extra: 0,
            length_extra_bits: 0,
            distance_symbol: 0,
            distance_extra: 0,
            distance_extra_bits: 0,
        },
        length,
    )
    .unwrap()
}

fn oracle(plain: &[u8], literal: &[u8], distance: &[u8]) -> u64 {
    if plain.is_empty() {
        return 0;
    }
    let mut best = price(literal, usize::from(plain[0]))
        .map_or(INF, |p| p + oracle(&plain[1..], literal, distance));
    for n in 3..=plain.len() {
        if let Some(p) = match_price(seed(n), literal, distance) {
            best = best.min(p + oracle(&plain[n..], literal, distance));
        }
    }
    best
}

#[test]
fn exact_response_matches_exhaustive_spellings_including_absent_symbols() {
    let plain = [0, 1, 0, 0, 1, 1, 0, 1, 0, 1, 1, 0];
    for n in 3..=plain.len() {
        for profile in 0..32 {
            let mut ll = [0; 286];
            ll[0] = profile % 5;
            ll[1] = (profile / 3) % 5;
            ll[256] = 1;
            for (i, l) in ll.iter_mut().enumerate().skip(257) {
                *l = ((i + usize::from(profile)) % 6) as u8;
            }
            ll[match seed(n) {
                Token::Match { length_symbol, .. } => length_symbol as usize,
                _ => unreachable!(),
            }] = 4;
            let dd = [1 + profile % 4];
            let mut out = Vec::new();
            spell(
                seed(n),
                &plain[..n],
                &ll,
                &dd,
                &mut ResponseBudget::new(),
                &mut SearchStop::never(),
                &mut out,
            )
            .unwrap();
            assert_eq!(
                token_bits(&out, &ll, &dd).unwrap() - 1,
                oracle(&plain[..n], &ll, &dd)
            );
            assert_eq!(out.iter().map(|t| t.decoded_len()).sum::<usize>(), n);
            assert!(out
                .iter()
                .all(|t| !matches!(t, Token::Match {distance,..} if *distance != 1)));
        }
    }
}

#[test]
fn absent_literals_source_ties_canonical_258_and_interval_stops() {
    let mut ll = [0; 286];
    ll[256] = 1;
    ll[285] = 1;
    let mut out = Vec::new();
    spell(
        seed(258),
        &[42; 258],
        &ll,
        &[1],
        &mut ResponseBudget::new(),
        &mut SearchStop::never(),
        &mut out,
    )
    .unwrap();
    assert_eq!(out, vec![seed(258)]);
    assert!(matches!(
        out[0],
        Token::Match {
            length_symbol: 285,
            length_extra_bits: 0,
            ..
        }
    ));
    for work in [0, 1, 258] {
        let mut budget = ResponseBudget {
            work_left: work,
            prices_left: 1,
        };
        let mut out = Vec::new();
        assert!(spell(
            seed(258),
            &[42; 258],
            &ll,
            &[1],
            &mut budget,
            &mut SearchStop::never(),
            &mut out
        )
        .is_none());
        assert!(out.is_empty());
        assert_eq!(budget.work_left, 0);
    }
    let mut out = Vec::new();
    assert!(spell(
        seed(258),
        &[42; 258],
        &ll,
        &[1],
        &mut ResponseBudget::new(),
        &mut SearchStop::always(),
        &mut out
    )
    .is_none());
    assert!(out.is_empty());
    assert!(spell(
        seed(258),
        &[42; 257],
        &ll,
        &[1],
        &mut ResponseBudget::new(),
        &mut SearchStop::never(),
        &mut out
    )
    .is_none());
}

fn witness() -> ParsedBlock {
    crate::deflate::parse::parse_stream(
        &crate::deflate::header::test_support::header_response_test_stream(),
        1024,
    )
    .unwrap()
    .blocks
    .remove(0)
}

#[test]
fn combined_move_crosses_a_tree_and_spelling_barrier() {
    let block = witness();
    let parent = block.original_dynamic.as_ref().unwrap();
    assert_eq!(block.original.unwrap().len, 438);
    let mut literal = parent.literal_lengths.clone();
    literal.swap(7, 267);
    let tokens = response(
        &block,
        &literal,
        &parent.distance_lengths,
        &mut ResponseBudget::new(),
        &mut SearchStop::never(),
    )
    .unwrap();
    let priced = |tokens: &[Token], ll: &[u8]| {
        plan_for_advertised_lengths(
            ll,
            &parent.distance_lengths,
            token_bits(tokens, ll, &parent.distance_lengths).unwrap(),
        )
        .unwrap()
        .bits
    };
    assert_eq!(priced(&block.tokens, &literal), 438);
    assert_eq!(priced(&tokens, &parent.literal_lengths), 440);
    assert_eq!(priced(&tokens, &literal), 437);
    let unchanged = response(
        &block,
        &parent.literal_lengths,
        &parent.distance_lengths,
        &mut ResponseBudget::new(),
        &mut SearchStop::never(),
    )
    .unwrap();
    assert_eq!(priced(&unchanged, &parent.literal_lengths), 438);
    for strict in [false, true] {
        let plan = plan_header_response(
            &block,
            strict,
            &mut ResponseBudget::new(),
            &mut SearchStop::never(),
        )
        .unwrap();
        assert!(plan.bits <= 437);
        crate::deflate::symbol_set::test_support::assert_proven_rewrite(&block, &plan.tokens);
        let Representation::Dynamic(dynamic) = &plan.representation else {
            panic!("expected dynamic")
        };
        assert!(dynamic.has_strictly_compatible_huffman_codes());
        assert_eq!((dynamic.hlit, dynamic.hdist), (parent.hlit, parent.hdist));
        for (before, after) in [
            (&parent.literal_lengths, &dynamic.literal_lengths),
            (&parent.distance_lengths, &dynamic.distance_lengths),
        ] {
            assert!(before
                .iter()
                .zip(after)
                .all(|(a, b)| (*a == 0) == (*b == 0)));
            let mut a = before.clone();
            let mut b = after.clone();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b);
        }
        let mut w = crate::deflate::bitstream::BitWriter::default();
        crate::deflate::block::emit_block(&mut w, &[], &plan, true).unwrap();
        assert_eq!(w.bit_position(), plan.bits);
        let check = crate::deflate::parse::parse_stream(&w.into_bytes(), 1024).unwrap();
        assert_eq!(check.blocks[0].plain, block.plain);
    }
}

#[test]
fn caps_callbacks_and_shared_budgets_preserve_completed_winners() {
    let block = witness();
    for prices in [0, 1, 8, 32, 128, 512] {
        let mut budget = ResponseBudget {
            work_left: STREAM_WORK,
            prices_left: prices,
        };
        let p = plan_header_response(&block, true, &mut budget, &mut SearchStop::never());
        assert!(p.as_ref().map_or(438, |p| p.bits) <= 438);
        assert!(budget.prices_left <= prices);
        if prices == 0 {
            assert!(p.is_none());
        }
    }
    let mut previous = 438;
    for work in [0, 1, 1000, 10000, 100000, 1000000, STREAM_WORK] {
        let mut budget = ResponseBudget {
            work_left: work,
            prices_left: STREAM_PRICES,
        };
        let p = plan_header_response(&block, true, &mut budget, &mut SearchStop::never());
        let bits = p.map_or(438, |p| p.bits);
        assert!(bits <= previous);
        previous = bits;
        assert!(budget.work_left <= work);
    }
    assert!(previous < 438);
    let mut calls = 0;
    let full = plan_header_response(
        &block,
        true,
        &mut ResponseBudget::new(),
        &mut SearchStop::callback(&mut || {
            calls += 1;
            false
        }),
    )
    .unwrap();
    let mut previous = 438;
    for cap in [0, calls / 4, calls / 2, 3 * calls / 4, calls + 1] {
        let mut seen = 0;
        let p = plan_header_response(
            &block,
            true,
            &mut ResponseBudget::new(),
            &mut SearchStop::callback(&mut || {
                seen += 1;
                seen > cap
            }),
        );
        let bits = p.map_or(438, |p| p.bits);
        assert!(bits <= previous);
        previous = bits;
    }
    assert_eq!(previous, full.bits);
    let mut budget = ResponseBudget::new();
    for _ in 0..8 {
        let _ = plan_header_response(&block, true, &mut budget, &mut SearchStop::never());
    }
    assert!(budget.work_left < STREAM_WORK);
    assert!(budget.prices_left < STREAM_PRICES);
    for oversized in [MAX_PLAIN + 1, MAX_PLAIN * 2] {
        let mut b = block.clone();
        b.plain = vec![0; oversized].into();
        assert!(plan_header_response(
            &b,
            true,
            &mut ResponseBudget::new(),
            &mut SearchStop::never()
        )
        .is_none());
    }
    let mut b = block.clone();
    b.tokens = vec![seed(3); MAX_TOKENS + 1].into();
    assert!(plan_header_response(
        &b,
        true,
        &mut ResponseBudget::new(),
        &mut SearchStop::never()
    )
    .is_none());
    b.tokens = vec![seed(3); MAX_MATCHES + 1].into();
    assert!(plan_header_response(
        &b,
        true,
        &mut ResponseBudget::new(),
        &mut SearchStop::never()
    )
    .is_none());
}
