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
use crate::error::SpanError;
use crate::interval::Interval;
use crate::positions::TermPositions;
use crate::query::SpanQuery;
use crate::state::NodeState;

/// Pre-compiled solver for a span query.
///
/// Allocates internal state once from a [`SpanQuery`] tree, then reuses it
/// across documents. Call [`intervals()`](Self::intervals) with each document's
/// position data to get a lazy iterator of minimal intervals.
pub struct SpanSolver {
    root: NodeState,
}

impl SpanSolver {
    /// Compile a solver from a query tree.
    pub fn new(query: &SpanQuery) -> Result<Self, SpanError> {
        query.validate(query.num_terms())?;
        Ok(Self {
            root: NodeState::compile(query),
        })
    }

    /// Iterate minimal intervals for a single document.
    ///
    /// Resets internal state and returns a lazy iterator that borrows both the
    /// solver (for mutable state) and the positions (for reading).
    pub fn intervals<'a, P: TermPositions>(&'a mut self, positions: &'a P) -> Intervals<'a, P> {
        self.root.reset();
        Intervals {
            root: &mut self.root,
            positions,
        }
    }

    /// Count minimal intervals without collecting them.
    pub fn span_freq(&mut self, positions: &impl TermPositions) -> u32 {
        self.intervals(positions).count() as u32
    }
}

/// Lazy iterator over minimal intervals for one document.
pub struct Intervals<'a, P: TermPositions> {
    root: &'a mut NodeState,
    positions: &'a P,
}

impl<P: TermPositions> Iterator for Intervals<'_, P> {
    type Item = Interval;

    fn next(&mut self) -> Option<Interval> {
        self.root.next_interval(self.positions)
    }
}
