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
mod shared;
mod term;
mod unordered;

use std::cell::RefCell;
use std::rc::Rc;

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
pub(crate) use self::shared::{SharedSource, SharedState};
pub(crate) use self::term::TermState;
pub(crate) use self::unordered::UnorderedState;

// States this thread compiled and reset: the unit tests' size pins.
#[cfg(test)]
thread_local! {
    static STATES_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static STATES_RESET: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts one compiled state (unit tests only).
#[inline(always)]
fn built() {
    #[cfg(test)]
    STATES_BUILT.with(|built| built.set(built.get() + 1));
}

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
    Shared(SharedState),
}

impl NodeState {
    /// Compile a `SpanQuery` tree into a `NodeState` tree.
    pub(crate) fn compile(query: &SpanQuery) -> Self {
        Self::compile_gaps(query, false)
    }

    /// Compiles `query`; `gaps_read` says an enclosing gap filter reads this
    /// node's gap count. A gap filter passes no overlapping sub-intervals, so a
    /// gap-read `Unordered` compiles to its disjoint form: one `Ordered` per
    /// distinct child order, unioned. Building the overlapping form there would
    /// let an overlapping minimal interval (two children on one position)
    /// shadow the disjoint spans that contain it, and the filter would then
    /// reject them all.
    fn compile_gaps(query: &SpanQuery, gaps_read: bool) -> Self {
        built();
        if let Some(term_indices) = exact_phrase_term_indices(query) {
            return NodeState::Phrase(PhraseState::new(term_indices));
        }

        let compile = |q: &SpanQuery| NodeState::compile_gaps(q, false);
        let forward = |q: &SpanQuery| NodeState::compile_gaps(q, gaps_read);
        let filtered = |q: &SpanQuery| NodeState::compile_gaps(q, true);
        match query {
            SpanQuery::Empty => NodeState::Empty(EmptyState::new()),
            SpanQuery::Term(i) => NodeState::Term(TermState::new(*i)),
            SpanQuery::Ordered(children) => {
                NodeState::Ordered(OrderedState::new(children.iter().map(compile).collect()))
            }
            SpanQuery::Unordered(children) if gaps_read => disjoint_unordered(children),
            SpanQuery::Unordered(children) => {
                NodeState::Unordered(UnorderedState::compile(children))
            }
            SpanQuery::Or(children) => {
                NodeState::Or(OrState::new(children.iter().map(forward).collect()))
            }
            SpanQuery::MaxGaps { max_gaps, inner } => {
                NodeState::MaxGaps(MaxGapsState::new(filtered(inner), *max_gaps))
            }
            SpanQuery::GapsInRange {
                min_gaps,
                max_gaps,
                inner,
            } => {
                NodeState::GapsInRange(GapsInRangeState::new(filtered(inner), *min_gaps, *max_gaps))
            }
            SpanQuery::MaxWidth { max_width, inner } => {
                NodeState::MaxWidth(MaxWidthState::new(forward(inner), *max_width))
            }
            SpanQuery::WithinPositions { inner, lo, hi } => {
                NodeState::WithinPositions(WithinPositionsState::new(forward(inner), *lo, *hi))
            }
            // A relation reports its first operand's gaps.
            SpanQuery::Containing { big, little } => {
                NodeState::Relation(RelationState::containing(forward(big), compile(little)))
            }
            SpanQuery::ContainedBy { little, big } => {
                NodeState::Relation(RelationState::contained_by(forward(little), compile(big)))
            }
            SpanQuery::NotContaining { big, little } => {
                NodeState::Relation(RelationState::not_containing(forward(big), compile(little)))
            }
            SpanQuery::NotContainedBy { little, big } => NodeState::Relation(
                RelationState::not_contained_by(forward(little), compile(big)),
            ),
            SpanQuery::Overlapping { a, b } => {
                NodeState::Relation(RelationState::overlapping(forward(a), compile(b)))
            }
            SpanQuery::NonOverlapping { a, b } => {
                NodeState::Relation(RelationState::non_overlapping(forward(a), compile(b)))
            }
            SpanQuery::Before { a, b } => {
                NodeState::Relation(RelationState::before(forward(a), compile(b)))
            }
            SpanQuery::After { a, b } => {
                NodeState::Relation(RelationState::after(forward(a), compile(b)))
            }
        }
    }

    pub(crate) fn reset(&mut self) {
        #[cfg(test)]
        STATES_RESET.with(|reset| reset.set(reset.get() + 1));
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
            NodeState::Shared(s) => s.reset(),
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
            NodeState::Shared(s) => s.next_interval(positions),
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
            NodeState::Shared(s) => s.gaps(),
        }
    }
}

/// Disjoint intervals are strictly ordered, so the minimal spans holding one
/// disjoint interval per child are the union over child orders of `Ordered`,
/// which consumes disjoint intervals by construction. Identical children give
/// identical orders; each distinct order compiles once. tinql's NEAR has two
/// children: at most two ordered walks. A child holding an Unordered compiles
/// once, and each walk reads it through a replay reader of its own
/// ([`SharedState`]): compiled once per walk, a chain of NEARs doubled at every
/// level, 2^65 states for 66 terms (wp-m4.9-gate.md §U6.13). Any other child,
/// a term, a phrase or an OR of them, compiles once per walk, as before.
fn disjoint_unordered(children: &[SpanQuery]) -> NodeState {
    let mut orders = Vec::new();
    let mut remaining: Vec<&SpanQuery> = children.iter().collect();
    child_orders(&mut Vec::new(), &mut remaining, &mut orders);
    let shared: Vec<Option<Rc<RefCell<SharedSource>>>> = (children.iter())
        .map(|child| {
            (orders.len() > 1 && child.holds_unordered())
                .then(|| SharedSource::new(NodeState::compile(child)))
        })
        .collect();
    let walk = |child: &SpanQuery| {
        let index = (children.iter())
            .position(|each| std::ptr::eq(each, child))
            .expect("an order walks the children themselves");
        match &shared[index] {
            Some(source) => {
                built();
                NodeState::Shared(SharedState::new(source))
            }
            None => NodeState::compile(child),
        }
    };
    let mut ordered: Vec<NodeState> = orders
        .iter()
        .map(|order| {
            built();
            NodeState::Ordered(OrderedState::new(
                order.iter().copied().map(&walk).collect(),
            ))
        })
        .collect();
    if ordered.len() == 1 {
        return ordered.pop().expect("one order");
    }
    built();
    NodeState::Or(OrState::new(ordered))
}

/// Pushes every distinct order of `remaining` after `prefix` onto `orders`.
fn child_orders<'q>(
    prefix: &mut Vec<&'q SpanQuery>,
    remaining: &mut Vec<&'q SpanQuery>,
    orders: &mut Vec<Vec<&'q SpanQuery>>,
) {
    if remaining.is_empty() {
        orders.push(prefix.clone());
        return;
    }
    for i in 0..remaining.len() {
        // Taking an equal child again at this depth repeats an order.
        if remaining[..i].contains(&remaining[i]) {
            continue;
        }
        let child = remaining.remove(i);
        prefix.push(child);
        child_orders(prefix, remaining, orders);
        prefix.pop();
        remaining.insert(i, child);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// tinql's left-deep `t0 NEAR/n t1 NEAR/n t2 …` over `terms` terms.
    fn near_chain(terms: usize, max_gaps: u32) -> SpanQuery {
        (1..terms).fold(SpanQuery::Term(0), |chain, term| SpanQuery::MaxGaps {
            max_gaps,
            inner: Box::new(SpanQuery::Unordered(vec![chain, SpanQuery::Term(term)])),
        })
    }

    /// The states `query` compiles to, and those one document's reset
    /// reaches.
    fn states_built_and_reset(query: &SpanQuery) -> (usize, usize) {
        let count = |counter: &'static std::thread::LocalKey<std::cell::Cell<usize>>| {
            counter.with(std::cell::Cell::get)
        };
        let built = count(&STATES_BUILT);
        let mut state = NodeState::compile(query);
        let reset = count(&STATES_RESET);
        state.reset();
        (count(&STATES_BUILT) - built, count(&STATES_RESET) - reset)
    }

    /// A NEAR chain compiles to states linear in its depth, and a document's
    /// reset reaches each once: each level's two disjoint orders share the
    /// chain below them, which only the first reader resets
    /// (wp-m4.9-gate.md §U6.13). Compiled once per order, every level doubled
    /// the chain below it, and 66 terms took 2^65 states.
    #[test]
    fn a_near_chain_compiles_and_resets_linear_in_its_depth() {
        for terms in [2, 3, 8, 20, 66] {
            let (built, reset) = states_built_and_reset(&near_chain(terms, 99));
            assert!(
                built <= 10 * terms && reset <= 10 * terms,
                "a {terms}-term NEAR chain compiled {built} states and reset {reset}"
            );
        }
    }
}
