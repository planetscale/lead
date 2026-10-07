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

use rustc_hash::FxHashMap;

use super::NodeState;
use crate::interval::Interval;
use crate::positions::TermPositions;
use crate::query::SpanQuery;

type StartKey = (u32, Reverse<u32>, usize);
type EndKey = (u32, u32, usize);

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

    fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        match self {
            Self::Single(state) => state.next_interval(positions),
            Self::Repeated(state) => state.next_interval(positions),
        }
    }
}

/// Sliding windows over distinct intervals from one structurally repeated child.
struct RepeatingState {
    child: NodeState,
    copies: usize,
    window: VecDeque<Interval>,
    exhausted: bool,
}

impl RepeatingState {
    fn new(child: NodeState, copies: usize) -> Self {
        debug_assert!(copies > 1);
        Self {
            child,
            copies,
            window: VecDeque::with_capacity(copies),
            exhausted: false,
        }
    }

    fn reset(&mut self) {
        self.child.reset();
        self.window.clear();
        self.exhausted = false;
    }

    fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        if self.exhausted {
            return None;
        }
        if self.window.len() == self.copies {
            self.window.pop_front();
        }

        while self.window.len() < self.copies {
            let Some(interval) = self.child.next_interval(positions) else {
                self.exhausted = true;
                return None;
            };
            self.window.push_back(interval);
        }

        let start = self.window.front()?.start;
        let end = self.window.back()?.end;
        Some(Interval::new(start, end))
    }
}

/// Indexed live intervals for an unordered conjunction.
struct CurrentIntervals {
    slots: Vec<Option<Interval>>,
    by_start: BTreeSet<StartKey>,
    by_end: BTreeSet<EndKey>,
}

impl CurrentIntervals {
    fn new(children: usize) -> Self {
        Self {
            slots: vec![None; children],
            by_start: BTreeSet::new(),
            by_end: BTreeSet::new(),
        }
    }

    fn reset(&mut self) {
        self.slots.fill(None);
        self.by_start.clear();
        self.by_end.clear();
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

    fn replace(&mut self, child: usize, next: Option<Interval>) {
        if let Some(previous) = self.slots[child] {
            let removed_start = self.by_start.remove(&start_key(child, previous));
            let removed_end = self.by_end.remove(&end_key(child, previous));
            debug_assert!(removed_start && removed_end);
        }

        self.slots[child] = next;
        if let Some(next) = next {
            let inserted_start = self.by_start.insert(start_key(child, next));
            let inserted_end = self.by_end.insert(end_key(child, next));
            debug_assert!(inserted_start && inserted_end);
        }
    }
}

fn start_key(child: usize, interval: Interval) -> StartKey {
    (interval.start, Reverse(interval.end), child)
}

fn end_key(child: usize, interval: Interval) -> EndKey {
    (interval.end, interval.start, child)
}

/// BV AND algorithm: unordered conjunction.
///
/// Finds minimal intervals containing one interval from each child,
/// in any order. Uses ⪯ priority ordering (left-to-right). Children may
/// overlap, so it reports no gap count: under a gap filter the compiler builds
/// the disjoint form instead (see `NodeState::compile_gaps`).
pub(crate) struct UnorderedState {
    children: Vec<UnorderedChild>,
    current: CurrentIntervals,
    prev: Option<Interval>,
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
        }
    }

    pub(crate) fn reset(&mut self) {
        for child in &mut self.children {
            child.reset();
        }
        self.current.reset();
        self.prev = None;
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
        }

        self.prev = Some(candidate);
        Some(candidate)
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        None
    }
}
