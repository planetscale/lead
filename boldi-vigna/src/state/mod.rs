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
mod disjunction;
mod empty;
mod filter;
mod ordered;
mod phrase;
mod relation;
mod term;
mod unordered;

use crate::interval::Interval;
use crate::positions::TermPositions;
use crate::query::SpanQuery;

pub(crate) use self::disjunction::OrState;
pub(crate) use self::empty::EmptyState;
pub(crate) use self::filter::{
    GapsInRangeState, MaxGapsState, MaxWidthState, WithinPositionsState,
};
pub(crate) use self::ordered::OrderedState;
pub(crate) use self::phrase::PhraseState;
pub(crate) use self::relation::RelationState;
pub(crate) use self::term::TermState;
pub(crate) use self::unordered::UnorderedState;

/// Internal state machine for a single node in the operator tree.
/// Mirrors the structure of `SpanQuery` but carries mutable iteration state.
pub(crate) enum NodeState {
    Empty(EmptyState),
    Term(TermState),
    Phrase(PhraseState),
    Ordered(OrderedState),
    Unordered(UnorderedState),
    Or(OrState),
    MaxGaps(MaxGapsState),
    GapsInRange(GapsInRangeState),
    MaxWidth(MaxWidthState),
    WithinPositions(WithinPositionsState),
    Relation(RelationState),
}

impl NodeState {
    /// Compile a `SpanQuery` tree into a `NodeState` tree.
    pub(crate) fn compile(query: &SpanQuery) -> Self {
        if let Some(term_indices) = exact_phrase_term_indices(query) {
            return NodeState::Phrase(PhraseState::new(term_indices));
        }

        match query {
            SpanQuery::Empty => NodeState::Empty(EmptyState::new()),
            SpanQuery::Term(i) => NodeState::Term(TermState::new(*i)),
            SpanQuery::Ordered(children) => {
                let child_states: Vec<_> = children.iter().map(NodeState::compile).collect();
                NodeState::Ordered(OrderedState::new(child_states))
            }
            SpanQuery::Unordered(children) => {
                NodeState::Unordered(UnorderedState::compile(children))
            }
            SpanQuery::Or(children) => {
                let child_states: Vec<_> = children.iter().map(NodeState::compile).collect();
                NodeState::Or(OrState::new(child_states))
            }
            SpanQuery::MaxGaps { max_gaps, inner } => {
                NodeState::MaxGaps(MaxGapsState::new(NodeState::compile(inner), *max_gaps))
            }
            SpanQuery::GapsInRange {
                min_gaps,
                max_gaps,
                inner,
            } => NodeState::GapsInRange(GapsInRangeState::new(
                NodeState::compile(inner),
                *min_gaps,
                *max_gaps,
            )),
            SpanQuery::MaxWidth { max_width, inner } => {
                NodeState::MaxWidth(MaxWidthState::new(NodeState::compile(inner), *max_width))
            }
            SpanQuery::WithinPositions { inner, lo, hi } => NodeState::WithinPositions(
                WithinPositionsState::new(NodeState::compile(inner), *lo, *hi),
            ),
            SpanQuery::Containing { big, little } => NodeState::Relation(
                RelationState::containing(NodeState::compile(big), NodeState::compile(little)),
            ),
            SpanQuery::ContainedBy { little, big } => NodeState::Relation(
                RelationState::contained_by(NodeState::compile(little), NodeState::compile(big)),
            ),
            SpanQuery::NotContaining { big, little } => NodeState::Relation(
                RelationState::not_containing(NodeState::compile(big), NodeState::compile(little)),
            ),
            SpanQuery::NotContainedBy { little, big } => {
                NodeState::Relation(RelationState::not_contained_by(
                    NodeState::compile(little),
                    NodeState::compile(big),
                ))
            }
            SpanQuery::Overlapping { a, b } => NodeState::Relation(RelationState::overlapping(
                NodeState::compile(a),
                NodeState::compile(b),
            )),
            SpanQuery::NonOverlapping { a, b } => NodeState::Relation(
                RelationState::non_overlapping(NodeState::compile(a), NodeState::compile(b)),
            ),
            SpanQuery::Before { a, b } => NodeState::Relation(RelationState::before(
                NodeState::compile(a),
                NodeState::compile(b),
            )),
            SpanQuery::After { a, b } => NodeState::Relation(RelationState::after(
                NodeState::compile(a),
                NodeState::compile(b),
            )),
        }
    }

    pub(crate) fn reset(&mut self) {
        match self {
            NodeState::Empty(s) => s.reset(),
            NodeState::Term(s) => s.reset(),
            NodeState::Phrase(s) => s.reset(),
            NodeState::Ordered(s) => s.reset(),
            NodeState::Unordered(s) => s.reset(),
            NodeState::Or(s) => s.reset(),
            NodeState::MaxGaps(s) => s.reset(),
            NodeState::GapsInRange(s) => s.reset(),
            NodeState::MaxWidth(s) => s.reset(),
            NodeState::WithinPositions(s) => s.reset(),
            NodeState::Relation(s) => s.reset(),
        }
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        match self {
            NodeState::Empty(s) => s.next_interval(positions),
            NodeState::Term(s) => s.next_interval(positions),
            NodeState::Phrase(s) => s.next_interval(positions),
            NodeState::Ordered(s) => s.next_interval(positions),
            NodeState::Unordered(s) => s.next_interval(positions),
            NodeState::Or(s) => s.next_interval(positions),
            NodeState::MaxGaps(s) => s.next_interval(positions),
            NodeState::GapsInRange(s) => s.next_interval(positions),
            NodeState::MaxWidth(s) => s.next_interval(positions),
            NodeState::WithinPositions(s) => s.next_interval(positions),
            NodeState::Relation(s) => s.next_interval(positions),
        }
    }

    /// Gap count for the current interval.
    pub(crate) fn gaps(&self) -> Option<u64> {
        match self {
            NodeState::Empty(s) => s.gaps(),
            NodeState::Term(s) => s.gaps(),
            NodeState::Phrase(s) => s.gaps(),
            NodeState::Ordered(s) => s.gaps(),
            NodeState::Unordered(s) => s.gaps(),
            NodeState::Or(s) => s.gaps(),
            NodeState::MaxGaps(s) => s.gaps(),
            NodeState::GapsInRange(s) => s.gaps(),
            NodeState::MaxWidth(s) => s.gaps(),
            NodeState::WithinPositions(s) => s.gaps(),
            NodeState::Relation(s) => s.gaps(),
        }
    }
}

fn exact_phrase_term_indices(query: &SpanQuery) -> Option<Vec<usize>> {
    let SpanQuery::MaxGaps { max_gaps, inner } = query else {
        return None;
    };
    if *max_gaps != 0 {
        return None;
    }

    let SpanQuery::Ordered(children) = inner.as_ref() else {
        return None;
    };
    if children.len() < 2 {
        return None;
    }

    let mut term_indices = Vec::with_capacity(children.len());
    for child in children {
        let SpanQuery::Term(idx) = child else {
            return None;
        };
        term_indices.push(*idx);
    }
    Some(term_indices)
}
