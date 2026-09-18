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
//
// The full license text is available in LICENSE.
use std::cmp::Reverse;
use std::collections::{BTreeSet, VecDeque};
use std::ops::Bound::{Excluded, Unbounded};

use rustc_hash::FxHashMap;

use super::NodeState;
use crate::interval::Interval;
use crate::positions::TermPositions;
use crate::query::SpanQuery;

type StartKey = (u32, Reverse<u32>, usize);
type EndKey = (u32, u32, usize);

#[derive(Clone, Copy)]
struct OperandInterval {
    interval: Interval,
    coverage: u64,
    contains_overlap: bool,
}

enum UnorderedChild {
    Single(NodeState),
    Repeated(RepeatingState),
}

impl UnorderedChild {
    fn new(state: NodeState, copies: usize) -> Self {
        if copies == 1 {
            Self::Single(state)
        } else {
            Self::Repeated(RepeatingState::new(state, copies))
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Single(state) => state.reset(),
            Self::Repeated(state) => state.reset(),
        }
    }

    fn next_interval(&mut self, positions: &impl TermPositions) -> Option<OperandInterval> {
        match self {
            Self::Single(state) => state
                .next_interval(positions)
                .map(|interval| OperandInterval {
                    interval,
                    coverage: u64::from(interval.width()),
                    contains_overlap: false,
                }),
            Self::Repeated(state) => state.next_interval(positions),
        }
    }
}

/// Sliding windows over distinct intervals from one structurally repeated child.
struct RepeatingState {
    child: NodeState,
    copies: usize,
    window: VecDeque<Interval>,
    coverage: u64,
    overlap_pairs: usize,
    exhausted: bool,
}

impl RepeatingState {
    fn new(child: NodeState, copies: usize) -> Self {
        debug_assert!(copies > 1);
        Self {
            child,
            copies,
            window: VecDeque::with_capacity(copies),
            coverage: 0,
            overlap_pairs: 0,
            exhausted: false,
        }
    }

    fn reset(&mut self) {
        self.child.reset();
        self.window.clear();
        self.coverage = 0;
        self.overlap_pairs = 0;
        self.exhausted = false;
    }

    fn next_interval(&mut self, positions: &impl TermPositions) -> Option<OperandInterval> {
        if self.exhausted {
            return None;
        }
        if self.window.len() == self.copies {
            self.pop_front();
        }

        while self.window.len() < self.copies {
            let Some(interval) = self.child.next_interval(positions) else {
                self.exhausted = true;
                return None;
            };
            self.push_back(interval);
        }

        let start = self.window.front()?.start;
        let end = self.window.back()?.end;
        Some(OperandInterval {
            interval: Interval::new(start, end),
            coverage: self.coverage,
            contains_overlap: self.overlap_pairs != 0,
        })
    }

    fn pop_front(&mut self) {
        let Some(front) = self.window.pop_front() else {
            return;
        };
        self.coverage -= u64::from(front.width());
        if self
            .window
            .front()
            .is_some_and(|next| front.overlaps(*next))
        {
            self.overlap_pairs -= 1;
        }
    }

    fn push_back(&mut self, interval: Interval) {
        if self
            .window
            .back()
            .is_some_and(|previous| previous.overlaps(interval))
        {
            self.overlap_pairs += 1;
        }
        self.coverage += u64::from(interval.width());
        self.window.push_back(interval);
    }
}

/// Indexed live intervals for an unordered conjunction.
struct CurrentIntervals {
    slots: Vec<Option<OperandInterval>>,
    by_start: BTreeSet<StartKey>,
    by_end: BTreeSet<EndKey>,
    coverage: u64,
    overlap_pairs: usize,
    overlapping_children: usize,
}

impl CurrentIntervals {
    fn new(children: usize) -> Self {
        Self {
            slots: vec![None; children],
            by_start: BTreeSet::new(),
            by_end: BTreeSet::new(),
            coverage: 0,
            overlap_pairs: 0,
            overlapping_children: 0,
        }
    }

    fn reset(&mut self) {
        self.slots.fill(None);
        self.by_start.clear();
        self.by_end.clear();
        self.coverage = 0;
        self.overlap_pairs = 0;
        self.overlapping_children = 0;
    }

    fn is_present(&self, child: usize) -> bool {
        self.slots[child].is_some()
    }

    fn is_full(&self) -> bool {
        self.by_start.len() == self.slots.len()
    }

    fn minimum(&self) -> Option<(usize, Interval)> {
        let &(start, Reverse(end), child) = self.by_start.first()?;
        Some((child, Interval::new(start, end)))
    }

    fn span(&self) -> Option<Interval> {
        let &(start, _, _) = self.by_start.first()?;
        let &(end, _, _) = self.by_end.last()?;
        Some(Interval::new(start, end))
    }

    fn gaps(&self, span: Interval) -> Option<u64> {
        if self.overlap_pairs != 0 || self.overlapping_children != 0 {
            return None;
        }
        u64::from(span.width()).checked_sub(self.coverage)
    }

    fn replace(&mut self, child: usize, next: Option<OperandInterval>) {
        if let Some(previous) = self.slots[child] {
            self.unlink(child, previous);
            self.coverage -= previous.coverage;
            self.overlapping_children -= usize::from(previous.contains_overlap);
        }

        self.slots[child] = next;
        if let Some(next) = next {
            self.link(child, next);
            self.coverage += next.coverage;
            self.overlapping_children += usize::from(next.contains_overlap);
        }
    }

    fn link(&mut self, child: usize, operand: OperandInterval) {
        let key = start_key(child, operand.interval);
        let (previous, next) = self.neighbors(key);
        self.overlap_pairs -= overlapping_pair(previous, next);
        self.overlap_pairs +=
            overlapping_pair(previous, Some(key)) + overlapping_pair(Some(key), next);
        let inserted_start = self.by_start.insert(key);
        let inserted_end = self.by_end.insert(end_key(child, operand.interval));
        debug_assert!(inserted_start && inserted_end);
    }

    fn unlink(&mut self, child: usize, operand: OperandInterval) {
        let key = start_key(child, operand.interval);
        let (previous, next) = self.neighbors(key);
        self.overlap_pairs -=
            overlapping_pair(previous, Some(key)) + overlapping_pair(Some(key), next);
        self.overlap_pairs += overlapping_pair(previous, next);
        let removed_start = self.by_start.remove(&key);
        let removed_end = self.by_end.remove(&end_key(child, operand.interval));
        debug_assert!(removed_start && removed_end);
    }

    fn neighbors(&self, key: StartKey) -> (Option<StartKey>, Option<StartKey>) {
        let previous = self.by_start.range(..key).next_back().copied();
        let next = self
            .by_start
            .range((Excluded(key), Unbounded))
            .next()
            .copied();
        (previous, next)
    }
}

fn start_key(child: usize, interval: Interval) -> StartKey {
    (interval.start, Reverse(interval.end), child)
}

fn end_key(child: usize, interval: Interval) -> EndKey {
    (interval.end, interval.start, child)
}

fn overlapping_pair(left: Option<StartKey>, right: Option<StartKey>) -> usize {
    match (left, right) {
        (Some((_, Reverse(left_end), _)), Some((right_start, _, _))) => {
            usize::from(left_end >= right_start)
        }
        _ => 0,
    }
}

/// BV AND algorithm: unordered conjunction.
///
/// Finds minimal intervals containing one interval from each child,
/// in any order. Uses ⪯ priority ordering (left-to-right).
pub(crate) struct UnorderedState {
    children: Vec<UnorderedChild>,
    current: CurrentIntervals,
    prev: Option<Interval>,
    last_gaps: Option<u64>,
}

impl UnorderedState {
    pub(crate) fn compile(queries: &[SpanQuery]) -> Self {
        let mut group_indexes: FxHashMap<&SpanQuery, usize> = FxHashMap::default();
        let mut groups = Vec::<(&SpanQuery, usize)>::new();
        for query in queries {
            if let Some(&group) = group_indexes.get(query) {
                groups[group].1 += 1;
            } else {
                let group = groups.len();
                group_indexes.insert(query, group);
                groups.push((query, 1));
            }
        }

        let children = groups
            .into_iter()
            .map(|(query, copies)| UnorderedChild::new(NodeState::compile(query), copies))
            .collect();
        Self::new(children)
    }

    fn new(children: Vec<UnorderedChild>) -> Self {
        let current = CurrentIntervals::new(children.len());
        Self {
            children,
            current,
            prev: None,
            last_gaps: None,
        }
    }

    pub(crate) fn reset(&mut self) {
        for child in &mut self.children {
            child.reset();
        }
        self.current.reset();
        self.prev = None;
        self.last_gaps = None;
    }

    fn advance_child(&mut self, child: usize, positions: &impl TermPositions) {
        let next = self.children[child].next_interval(positions);
        self.current.replace(child, next);
    }

    fn advance_min(&mut self, positions: &impl TermPositions) {
        if let Some((child, _)) = self.current.minimum() {
            self.advance_child(child, positions);
        }
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        for child in 0..self.children.len() {
            if !self.current.is_present(child) {
                self.advance_child(child, positions);
            }
        }

        while self.current.is_full() {
            if let (Some(previous), Some(span)) = (self.prev, self.current.span())
                && span.contains(previous)
            {
                self.advance_min(positions);
                continue;
            }
            break;
        }

        if !self.current.is_full() {
            return None;
        }

        let mut candidate = self.current.span()?;
        let mut candidate_gaps = self.current.gaps(candidate);

        loop {
            if self
                .current
                .minimum()
                .is_some_and(|(_, interval)| interval == candidate)
            {
                break;
            }

            self.advance_min(positions);
            if !self.current.is_full() {
                break;
            }

            let new_span = self.current.span()?;
            if !candidate.contains(new_span) {
                break;
            }
            candidate = new_span;
            candidate_gaps = self.current.gaps(candidate);
        }

        self.prev = Some(candidate);
        self.last_gaps = candidate_gaps;
        Some(candidate)
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        self.last_gaps
    }
}
