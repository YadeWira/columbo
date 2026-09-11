// SPDX-License-Identifier: MIT

use super::*;
use crate::deflate::header::test_support::{coupled_swap_test_stream, rotation_test_block};
use crate::deflate::header::{plan_header_tree, token_bits, HeaderTreeBudget};
use crate::deflate::parse::parse_stream;

fn witness() -> ParsedBlock {
    parse_stream(&coupled_swap_test_stream(), 16384)
        .unwrap()
        .blocks
        .remove(1)
}

#[test]
fn coupled_move_beats_every_single_swap_on_a_generated_parent() {
    let block = witness();
    let parent = block.original_dynamic.as_ref().unwrap();
    let old = block.original.unwrap().len;
    assert_eq!(old, 4974);
    for side in [false, true] {
        let mut literal = parent.literal_lengths.clone();
        let mut distance = parent.distance_lengths.clone();
        let lengths = if side { &mut distance } else { &mut literal };
        let symbols: Vec<_> = lengths
            .iter()
            .enumerate()
            .filter_map(|(s, &l)| (l != 0).then_some(s))
            .collect();
        for (i, &a) in symbols.iter().enumerate() {
            for &b in &symbols[i + 1..] {
                if side {
                    distance.swap(a, b);
                } else {
                    literal.swap(a, b);
                }
                let payload = token_bits(&block.tokens, &literal, &distance).unwrap();
                let single = plan_for_advertised_lengths(&literal, &distance, payload).unwrap();
                assert!(single.bits >= old, "side {side}, swap {a}/{b}");
                if side {
                    distance.swap(a, b);
                } else {
                    literal.swap(a, b);
                }
            }
        }
    }
    assert!(plan_header_tree(
        &block,
        true,
        &mut HeaderTreeBudget::new(),
        &mut SearchStop::never()
    )
    .is_none());
    for strict in [false, true] {
        let candidate = plan_coupled_length_swaps(
            &block,
            strict,
            &mut CoupledSwapBudget::new(),
            &mut SearchStop::never(),
        )
        .unwrap();
        assert_eq!(candidate.bits, 4972);
        let payload = token_bits(
            &block.tokens,
            &candidate.literal_lengths,
            &candidate.distance_lengths,
        )
        .unwrap();
        let parent_payload = token_bits(
            &block.tokens,
            &parent.literal_lengths,
            &parent.distance_lengths,
        )
        .unwrap();
        assert_eq!(payload, parent_payload + 8);
        assert_eq!(
            dynamic_bits(0, &candidate).unwrap() + 10,
            dynamic_bits(0, parent).unwrap()
        );
        assert_eq!(dynamic_bits(payload, &candidate), Some(candidate.bits));
        assert!(candidate.has_strictly_compatible_huffman_codes());
        assert_eq!(
            (candidate.hlit, candidate.hdist),
            (parent.hlit, parent.hdist)
        );
        for (before, after) in [
            (&parent.literal_lengths, &candidate.literal_lengths),
            (&parent.distance_lengths, &candidate.distance_lengths),
        ] {
            assert!(before
                .iter()
                .zip(after)
                .all(|(&a, &b)| (a == 0) == (b == 0)));
            let mut a = before.clone();
            let mut b = after.clone();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b);
        }
    }
}

#[test]
fn work_price_and_callback_stops_keep_completed_candidates() {
    let block = witness();
    let mut previous = block.original.unwrap().len;
    for prices in [0, 1, 8, 32, 64, 128, 256, 512] {
        let mut budget = CoupledSwapBudget {
            work_left: STREAM_WORK,
            prices_left: prices,
        };
        let result = plan_coupled_length_swaps(&block, true, &mut budget, &mut SearchStop::never());
        let bits = result.map_or(block.original.unwrap().len, |p| p.bits);
        assert!(bits <= previous);
        previous = bits;
        assert!(budget.prices_left <= prices);
    }
    assert_eq!(previous, 4972);
    for work in [0, 1, 24, 48, 64, 128] {
        let mut budget = CoupledSwapBudget {
            work_left: work,
            prices_left: STREAM_PRICES,
        };
        let result = plan_coupled_length_swaps(&block, true, &mut budget, &mut SearchStop::never());
        assert!(result.is_none());
        assert!(budget.work_left <= work);
    }
    assert!(plan_coupled_length_swaps(
        &block,
        true,
        &mut CoupledSwapBudget::new(),
        &mut SearchStop::always()
    )
    .is_none());
    let mut calls = 0;
    let mut budget = CoupledSwapBudget::new();
    let full = plan_coupled_length_swaps(
        &block,
        true,
        &mut budget,
        &mut SearchStop::callback(&mut || {
            calls += 1;
            false
        }),
    )
    .unwrap();
    let prices = STREAM_PRICES - budget.prices_left;
    let mut observed = 0;
    let partial = plan_coupled_length_swaps(
        &block,
        true,
        &mut CoupledSwapBudget::new(),
        &mut SearchStop::callback(&mut || {
            observed += 1;
            observed >= calls
        }),
    );
    let prefix = plan_coupled_length_swaps(
        &block,
        true,
        &mut CoupledSwapBudget {
            work_left: STREAM_WORK,
            prices_left: prices - 1,
        },
        &mut SearchStop::never(),
    );
    assert_eq!(partial, prefix);
    assert_eq!(partial.unwrap().bits, full.bits);
    // Generation can finish using the last work unit; completed menus still
    // receive their independently bounded full prices.
    let spent = STREAM_WORK - budget.work_left;
    let mut exact = CoupledSwapBudget {
        work_left: spent,
        prices_left: STREAM_PRICES,
    };
    assert_eq!(
        plan_coupled_length_swaps(&block, true, &mut exact, &mut SearchStop::never()),
        Some(full)
    );
    assert_eq!(exact.work_left, 0);
}

#[test]
fn empty_singleton_and_uniform_distance_alphabets_skip_the_large_menu() {
    for lengths in [&[0][..], &[1][..], &[1, 1][..]] {
        let block = rotation_test_block(lengths);
        for strict in [false, true] {
            let mut budget = CoupledSwapBudget::new();
            assert!(plan_coupled_length_swaps(
                &block,
                strict,
                &mut budget,
                &mut SearchStop::never()
            )
            .is_none());
            assert!(STREAM_WORK - budget.work_left < 10);
            assert_eq!(budget.prices_left, STREAM_PRICES);
        }
    }
}

fn full_transitions(lengths: &[u8]) -> i64 {
    lengths.windows(2).filter(|p| p[0] != p[1]).count() as i64
}

#[test]
fn local_transition_prices_match_complete_sequences_including_the_seam() {
    let mut seed = 42u64;
    for _ in 0..40 {
        let mut lengths = [0; 12];
        for length in &mut lengths {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *length = ((seed >> 32) % 8) as u8;
        }
        for a in 0..5 {
            for b in a + 1..6 {
                let mut trial = lengths;
                trial.swap(a, b);
                assert_eq!(
                    transitions_removed(&lengths, [a, b]),
                    full_transitions(&lengths) - full_transitions(&trial)
                );
                for c in 6..11 {
                    for d in c + 1..12 {
                        trial.swap(c, d);
                        assert_eq!(
                            transitions_removed(&lengths, [a, b, c, d]),
                            full_transitions(&lengths) - full_transitions(&trial)
                        );
                        trial.swap(c, d);
                    }
                }
            }
        }
    }
}

#[test]
fn bounded_menus_match_independent_full_enumeration() {
    let mut seed = 8349u64;
    let mut lengths = [0; 318];
    let mut frequencies = [0; 318];
    for (length, frequency) in lengths.iter_mut().zip(&mut frequencies) {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        *length = ((seed >> 32) % 8) as u8;
        *frequency = ((seed >> 40) % 3) as u32;
    }
    // Nonzero reserved distance positions must remain outside both menus.
    lengths[316..].fill(1);
    let middle = 286;
    let mut expected = Vec::new();
    for (offset, count) in [(0, middle), (middle, 30)] {
        let mut all = Vec::new();
        for a in offset..offset + count {
            for b in a + 1..offset + count {
                if lengths[a] == 0 || lengths[b] == 0 || lengths[a] == lengths[b] {
                    continue;
                }
                let mut trial = lengths;
                trial.swap(a, b);
                let tax: i64 = frequencies
                    .iter()
                    .zip(trial.iter().zip(&lengths))
                    .map(|(&f, (&after, &before))| {
                        i64::from(f) * (i64::from(after) - i64::from(before))
                    })
                    .sum();
                let removed = full_transitions(&lengths) - full_transitions(&trial);
                if !(-MAX_PAYLOAD_TAX..=MAX_PAYLOAD_TAX).contains(&tax) || removed < 0 {
                    continue;
                }
                all.push(Swap {
                    rank: tax - 3 * removed,
                    tax,
                    positions: [a, b],
                });
            }
        }
        all.sort();
        assert!(all.len() > ALPHABET_MENU);
        all.truncate(ALPHABET_MENU);
        assert_eq!(
            alphabet_menu(
                &lengths,
                offset,
                &frequencies[offset..offset + count],
                &mut CoupledSwapBudget::new(),
                &mut SearchStop::never()
            )
            .unwrap(),
            all
        );
        expected.push(all);
    }
    let mut all = Vec::new();
    for (i, a) in expected[0].iter().enumerate() {
        for (j, b) in expected[1].iter().enumerate() {
            let mut trial = lengths;
            trial.swap(a.positions[0], a.positions[1]);
            trial.swap(b.positions[0], b.positions[1]);
            let tax: i64 = frequencies
                .iter()
                .zip(trial.iter().zip(&lengths))
                .map(|(&f, (&after, &before))| {
                    i64::from(f) * (i64::from(after) - i64::from(before))
                })
                .sum();
            if !(-MAX_PAYLOAD_TAX..=MAX_PAYLOAD_TAX).contains(&tax) {
                continue;
            }
            let removed = full_transitions(&lengths) - full_transitions(&trial);
            all.push(Pair {
                rank: tax - 3 * removed,
                tax,
                literal: i,
                distance: j,
            });
        }
    }
    all.sort();
    assert!(all.len() > PAIR_MENU);
    all.truncate(PAIR_MENU);
    assert_eq!(
        pair_menu(
            &lengths,
            &expected[0],
            &expected[1],
            &mut CoupledSwapBudget::new(),
            &mut SearchStop::never()
        )
        .unwrap(),
        all
    );
}
