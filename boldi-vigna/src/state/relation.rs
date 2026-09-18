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

/// Which binary relation to evaluate between two interval streams.
enum RelationKind {
    Containing,
    ContainedBy,
    NotContaining,
    NotContainedBy,
    Overlapping,
    NonOverlapping,
    Before,
    After,
}

/// Two-pointer relation operator between interval streams `a` (source) and `b` (filter).
///
/// All relation operators share the same structure:
/// 1. Advance `a` to get the next candidate
/// 2. Skip `b` forward using a kind-specific skip condition
/// 3. Test a kind-specific match condition
/// 4. Return `a`'s interval if the condition is met (positive) or not met (negative)
pub(crate) struct RelationState {
    kind: RelationKind,
    a: Box<NodeState>,
    b: Box<NodeState>,
    /// Cached current interval from b (avoids re-advancing past what we've seen).
    b_current: Option<Interval>,
    b_exhausted: bool,
}

impl RelationState {
    fn new(kind: RelationKind, a: NodeState, b: NodeState) -> Self {
        Self {
            kind,
            a: Box::new(a),
            b: Box::new(b),
            b_current: None,
            b_exhausted: false,
        }
    }

    pub(crate) fn containing(big: NodeState, little: NodeState) -> Self {
        Self::new(RelationKind::Containing, big, little)
    }

    pub(crate) fn contained_by(little: NodeState, big: NodeState) -> Self {
        Self::new(RelationKind::ContainedBy, little, big)
    }

    pub(crate) fn not_containing(big: NodeState, little: NodeState) -> Self {
        Self::new(RelationKind::NotContaining, big, little)
    }

    pub(crate) fn not_contained_by(little: NodeState, big: NodeState) -> Self {
        Self::new(RelationKind::NotContainedBy, little, big)
    }

    pub(crate) fn overlapping(a: NodeState, b: NodeState) -> Self {
        Self::new(RelationKind::Overlapping, a, b)
    }

    pub(crate) fn non_overlapping(a: NodeState, b: NodeState) -> Self {
        Self::new(RelationKind::NonOverlapping, a, b)
    }

    pub(crate) fn before(a: NodeState, b: NodeState) -> Self {
        Self::new(RelationKind::Before, a, b)
    }

    pub(crate) fn after(a: NodeState, b: NodeState) -> Self {
        Self::new(RelationKind::After, a, b)
    }

    pub(crate) fn reset(&mut self) {
        self.a.reset();
        self.b.reset();
        self.b_current = None;
        self.b_exhausted = false;
    }

    fn advance_b(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        if self.b_exhausted {
            return None;
        }
        match self.b.next_interval(positions) {
            Some(iv) => {
                self.b_current = Some(iv);
                Some(iv)
            }
            None => {
                self.b_exhausted = true;
                self.b_current = None;
                None
            }
        }
    }

    fn ensure_b(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        if self.b_current.is_some() {
            return self.b_current;
        }
        self.advance_b(positions)
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        match self.kind {
            RelationKind::Containing => self.next_containing(positions),
            RelationKind::ContainedBy => self.next_contained_by(positions),
            RelationKind::NotContaining => self.next_not_containing(positions),
            RelationKind::NotContainedBy => self.next_not_contained_by(positions),
            RelationKind::Overlapping => self.next_overlapping(positions),
            RelationKind::NonOverlapping => self.next_non_overlapping(positions),
            RelationKind::Before => self.next_before(positions),
            RelationKind::After => self.next_after(positions),
        }
    }

    /// Containing(a, b): intervals from `a` that contain some interval from `b`.
    fn next_containing(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let a_iv = self.a.next_interval(positions)?;
            self.ensure_b(positions);

            // Skip b while b is entirely before/smaller than a and can't be contained
            while let Some(b_iv) = self.b_current {
                if b_iv.start < a_iv.start && b_iv.end < a_iv.end {
                    self.advance_b(positions);
                } else {
                    break;
                }
            }

            // Check if current b is contained by a
            if let Some(b_iv) = self.b_current
                && a_iv.contains(b_iv)
            {
                return Some(a_iv);
            }
        }
    }

    /// ContainedBy(a, b): intervals from `a` that are contained by some interval from `b`.
    fn next_contained_by(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let a_iv = self.a.next_interval(positions)?;
            self.ensure_b(positions);

            // Skip b while b doesn't extend far enough right
            while let Some(b_iv) = self.b_current {
                if b_iv.end < a_iv.end {
                    self.advance_b(positions);
                } else {
                    break;
                }
            }

            // Check if a is contained by current b
            if let Some(b_iv) = self.b_current
                && b_iv.start <= a_iv.start
            {
                // b.end >= a.end is guaranteed by skip
                return Some(a_iv);
            }
        }
    }

    /// NotContaining(a, b): intervals from `a` that do NOT contain any from `b`.
    fn next_not_containing(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let a_iv = self.a.next_interval(positions)?;
            self.ensure_b(positions);

            // Skip b while b is entirely before/smaller than a
            while let Some(b_iv) = self.b_current {
                if b_iv.start < a_iv.start && b_iv.end < a_iv.end {
                    self.advance_b(positions);
                } else {
                    break;
                }
            }

            // If b is exhausted or not contained by a, accept a
            match self.b_current {
                None => return Some(a_iv),
                Some(b_iv) => {
                    if !a_iv.contains(b_iv) {
                        return Some(a_iv);
                    }
                }
            }
        }
    }

    /// NotContainedBy(a, b): intervals from `a` NOT contained by any from `b`.
    fn next_not_contained_by(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let a_iv = self.a.next_interval(positions)?;
            self.ensure_b(positions);

            // Skip b while b doesn't extend far enough right
            while let Some(b_iv) = self.b_current {
                if b_iv.end < a_iv.end {
                    self.advance_b(positions);
                } else {
                    break;
                }
            }

            // If b is exhausted or doesn't contain a, accept a
            match self.b_current {
                None => return Some(a_iv),
                Some(b_iv) => {
                    if b_iv.start > a_iv.start {
                        // b.end >= a.end but b.start > a.start, so not contained
                        return Some(a_iv);
                    }
                }
            }
        }
    }

    /// Overlapping(a, b): intervals from `a` that overlap some from `b`.
    fn next_overlapping(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let a_iv = self.a.next_interval(positions)?;
            self.ensure_b(positions);

            // Skip b while b ends before a starts
            while let Some(b_iv) = self.b_current {
                if b_iv.end < a_iv.start {
                    self.advance_b(positions);
                } else {
                    break;
                }
            }

            if let Some(b_iv) = self.b_current
                && b_iv.start <= a_iv.end
            {
                return Some(a_iv);
            }
        }
    }

    /// NonOverlapping(a, b): intervals from `a` that do NOT overlap any from `b`.
    fn next_non_overlapping(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let a_iv = self.a.next_interval(positions)?;
            self.ensure_b(positions);

            // Skip b while b ends before a starts
            while let Some(b_iv) = self.b_current {
                if b_iv.end < a_iv.start {
                    self.advance_b(positions);
                } else {
                    break;
                }
            }

            match self.b_current {
                None => return Some(a_iv),
                Some(b_iv) => {
                    if b_iv.start > a_iv.end {
                        return Some(a_iv);
                    }
                }
            }
        }
    }

    /// Before(a, b): intervals from `a` for which some `b` starts later.
    ///
    /// "Later" compares starts (`b.start > a.start`); the spans may overlap.
    /// Skipping is safe: interval streams ascend by start, so a `b` whose
    /// start is not past this `a`'s start can never be past a later `a`'s.
    fn next_before(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let a_iv = self.a.next_interval(positions)?;
            self.ensure_b(positions);

            while let Some(b_iv) = self.b_current {
                if b_iv.start <= a_iv.start {
                    self.advance_b(positions);
                } else {
                    break;
                }
            }

            // Any remaining b starts after a (guaranteed by the skip).
            if self.b_current.is_some() {
                return Some(a_iv);
            }
        }
    }

    /// After(a, b): intervals from `a` for which some `b` starts earlier.
    ///
    /// "Earlier" compares starts (`b.start < a.start`); the spans may
    /// overlap. The first `b` interval has the smallest start in its stream,
    /// so it alone decides the relation and `b` is never advanced.
    fn next_after(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        loop {
            let a_iv = self.a.next_interval(positions)?;

            let first_b = self.ensure_b(positions)?;
            if first_b.start < a_iv.start {
                return Some(a_iv);
            }
        }
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        self.a.gaps()
    }
}
