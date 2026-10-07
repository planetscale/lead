// Copyright (C) 2026 PlanetScale
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.
//! `MAXGAPS(n, UNORDERED(a, b))` (tinql's `a NEAR/n b`) when the two slots can
//! match the same position: an OR group sharing a term with the other slot, or
//! a wide operand repeated in both slots.
//!
//! Semantics checked here, from the solver's documented contract (`SpanQuery`):
//! `MaxGaps` passes only assignments whose sub-intervals do not overlap, and
//! `Unordered` takes one interval from each child. So a NEAR match is a pair of
//! child intervals that share no position, in either order, whose gap count
//! (`width - covered positions`) is at most `n`; the solver reports the minimal
//! such spans. Two slots never reuse one position.

use boldi_vigna::*;
use proptest::prelude::*;

fn collect(query: &SpanQuery, positions: &impl TermPositions) -> Vec<Interval> {
    let mut solver = SpanSolver::new(query).unwrap();
    solver.intervals(positions).collect()
}

fn near(max_gaps: u32, left: SpanQuery, right: SpanQuery) -> SpanQuery {
    SpanQuery::MaxGaps {
        max_gaps,
        inner: Box::new(SpanQuery::Unordered(vec![left, right])),
    }
}

fn slot(terms: &[usize]) -> SpanQuery {
    match terms {
        [term] => SpanQuery::Term(*term),
        terms => SpanQuery::Or(terms.iter().copied().map(SpanQuery::Term).collect()),
    }
}

/// Brute force over point slots: every pair of distinct positions, one from
/// each slot, within `max_gaps`, reduced to minimal spans.
fn brute_force_near(
    max_gaps: u32,
    left: &[usize],
    right: &[usize],
    positions: &[Vec<u32>],
) -> Vec<Interval> {
    let slot_positions = |terms: &[usize]| -> Vec<u32> {
        let mut out: Vec<u32> = terms
            .iter()
            .flat_map(|&t| positions[t].iter().copied())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    };
    let (left, right) = (slot_positions(left), slot_positions(right));
    let mut spans = Vec::new();
    for &p in &left {
        for &q in &right {
            if p != q && p.abs_diff(q) - 1 <= max_gaps {
                spans.push(Interval::new(p.min(q), p.max(q)));
            }
        }
    }
    minimize(spans)
}

fn minimize(mut intervals: Vec<Interval>) -> Vec<Interval> {
    intervals.sort();
    intervals.dedup();
    intervals
        .iter()
        .copied()
        .filter(|&iv| !intervals.iter().any(|&o| o != iv && iv.contains(o)))
        .collect()
}

#[test]
fn group_slot_pairs_with_the_shared_term_in_the_other_slot() {
    // `(alpha OR bravo) NEAR/3 alpha` over "alpha the alpha": alpha=0, bravo=1.
    let positions = vec![vec![0, 2], vec![]];
    let q = near(3, slot(&[0, 1]), slot(&[0]));
    assert_eq!(collect(&q, &positions), vec![Interval::new(0, 2)]);
    // The single-term slot already matches; widening must not lose it.
    let narrow = near(3, slot(&[0]), slot(&[0]));
    assert_eq!(collect(&narrow, &positions), vec![Interval::new(0, 2)]);
}

#[test]
fn two_group_slots_sharing_a_term_pair_distinct_occurrences() {
    // `(alpha OR bravo) NEAR/2 (alpha OR charlie)` over "alpha of alpha".
    let positions = vec![vec![0, 2], vec![], vec![]];
    let q = near(2, slot(&[0, 1]), slot(&[0, 2]));
    assert_eq!(collect(&q, &positions), vec![Interval::new(0, 2)]);
}

#[test]
fn shared_term_does_not_shadow_a_pair_with_the_other_term() {
    // "bravo alpha": the alpha alone sits in both groups, but only bravo-alpha
    // is a pair of distinct positions.
    let positions = vec![vec![1], vec![0], vec![]];
    let q = near(0, slot(&[0, 1]), slot(&[0, 2]));
    assert_eq!(collect(&q, &positions), vec![Interval::new(0, 1)]);
}

#[test]
fn single_shared_occurrence_is_not_a_pair() {
    let positions = vec![vec![4], vec![]];
    let q = near(3, slot(&[0, 1]), slot(&[0]));
    assert!(collect(&q, &positions).is_empty());
}

#[test]
fn repeated_wide_operand_takes_disjoint_occurrences() {
    // `"a a" NEAR/0 "a a"` over "a a a a": the phrase occurs at [0,1], [1,2],
    // [2,3]; the disjoint pair [0,1] + [2,3] spans [0,3] with no gap.
    let positions = vec![vec![0, 1, 2, 3]];
    let phrase = SpanQuery::phrase([0, 0]);
    let q = near(0, phrase.clone(), phrase);
    assert_eq!(collect(&q, &positions), vec![Interval::new(0, 3)]);
}

#[test]
fn gaps_in_range_takes_disjoint_occurrences_too() {
    let positions = vec![vec![0, 2], vec![]];
    let q = SpanQuery::GapsInRange {
        min_gaps: 1,
        max_gaps: 1,
        inner: Box::new(SpanQuery::Unordered(vec![slot(&[0, 1]), slot(&[0])])),
    };
    assert_eq!(collect(&q, &positions), vec![Interval::new(0, 2)]);
}

fn positions_strategy(terms: usize) -> impl Strategy<Value = Vec<Vec<u32>>> {
    // One term per position, as the tokenizer produces.
    prop::collection::vec(0..terms + 2, 0..16).prop_map(move |doc| {
        let mut positions = vec![Vec::new(); terms];
        for (pos, term) in doc.into_iter().enumerate() {
            if term < terms {
                positions[term].push(pos as u32);
            }
        }
        positions
    })
}

fn slot_strategy(terms: usize) -> impl Strategy<Value = Vec<usize>> {
    prop::collection::btree_set(0..terms, 1..=terms).prop_map(|s| s.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn near_over_or_slots_matches_brute_force(
        positions in positions_strategy(3),
        left in slot_strategy(3),
        right in slot_strategy(3),
        max_gaps in 0u32..5,
    ) {
        let q = near(max_gaps, slot(&left), slot(&right));
        prop_assert_eq!(
            collect(&q, &positions),
            brute_force_near(max_gaps, &left, &right, &positions),
            "left={:?} right={:?} positions={:?}", left, right, positions
        );
    }

    #[test]
    fn widening_a_slot_never_loses_a_match(
        positions in positions_strategy(3),
        left in slot_strategy(3),
        right in slot_strategy(3),
        extra in 0usize..3,
        max_gaps in 0u32..5,
    ) {
        let narrow = near(max_gaps, slot(&left), slot(&right));
        let mut wider_left = left.clone();
        if !wider_left.contains(&extra) {
            wider_left.push(extra);
            wider_left.sort_unstable();
        }
        let wide = near(max_gaps, slot(&wider_left), slot(&right));
        if !collect(&narrow, &positions).is_empty() {
            prop_assert!(
                !collect(&wide, &positions).is_empty(),
                "{:?} matched but {:?} did not on {:?}", left, wider_left, positions
            );
        }
    }
}
