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
use super::NodeState;
use crate::interval::Interval;
use crate::positions::TermPositions;

/// MaxGaps filter: keeps only intervals from inner where gaps ≤ max_gaps.
pub(crate) struct MaxGapsState {
    inner: Box<NodeState>,
    max_gaps: u32,
}

impl MaxGapsState {
    pub(crate) fn new(inner: NodeState, max_gaps: u32) -> Self {
        Self {
            inner: Box::new(inner),
            max_gaps,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.inner.reset();
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let iv = self.inner.next_interval(positions)?;
            if self
                .inner
                .gaps()
                .is_some_and(|gaps| gaps <= u64::from(self.max_gaps))
            {
                return Some(iv);
            }
        }
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        self.inner.gaps()
    }
}

/// GapsInRange filter: keeps only intervals from inner where
/// min_gaps ≤ gaps ≤ max_gaps. The exact-gap case (min == max) implements
/// phrase gaps, which pin a specific number of intervening positions.
pub(crate) struct GapsInRangeState {
    inner: Box<NodeState>,
    min_gaps: u32,
    max_gaps: u32,
}

impl GapsInRangeState {
    pub(crate) fn new(inner: NodeState, min_gaps: u32, max_gaps: u32) -> Self {
        Self {
            inner: Box::new(inner),
            min_gaps,
            max_gaps,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.inner.reset();
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let iv = self.inner.next_interval(positions)?;
            if self.inner.gaps().is_some_and(|gaps| {
                gaps >= u64::from(self.min_gaps) && gaps <= u64::from(self.max_gaps)
            }) {
                return Some(iv);
            }
        }
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        self.inner.gaps()
    }
}

/// MaxWidth filter (BV LOWPASS): keeps only intervals with width ≤ max_width.
pub(crate) struct MaxWidthState {
    inner: Box<NodeState>,
    max_width: u32,
}

impl MaxWidthState {
    pub(crate) fn new(inner: NodeState, max_width: u32) -> Self {
        Self {
            inner: Box::new(inner),
            max_width,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.inner.reset();
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let iv = self.inner.next_interval(positions)?;
            if iv.width() <= self.max_width {
                return Some(iv);
            }
        }
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        self.inner.gaps()
    }
}

/// Positional range filter: keeps intervals where start >= lo and end <= hi.
pub(crate) struct WithinPositionsState {
    inner: Box<NodeState>,
    lo: u32,
    hi: u32,
}

impl WithinPositionsState {
    pub(crate) fn new(inner: NodeState, lo: u32, hi: u32) -> Self {
        Self {
            inner: Box::new(inner),
            lo,
            hi,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.inner.reset();
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let iv = self.inner.next_interval(positions)?;
            // If the interval starts past our window, no more can match
            if iv.start > self.hi {
                return None;
            }
            if iv.start >= self.lo && iv.end <= self.hi {
                return Some(iv);
            }
        }
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        self.inner.gaps()
    }
}
