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

/// BV AND< algorithm: ordered composition with backwards tightening.
///
/// Finds minimal intervals where child intervals appear in document order,
/// non-overlapping. The algorithm:
///
/// 1. Build a valid chain greedily (each child after the previous).
/// 2. Tighten backwards: from child m-2 down to 0, advance each child as
///    far as possible while still before the next child. This produces the
///    tightest (minimal) spanning interval.
/// 3. Buffer any "overshot" intervals from tightening for future iterations.
pub(crate) struct OrderedState {
    children: Vec<NodeState>,
    /// Current chain interval for each child.
    current: Vec<Option<Interval>>,
    /// Lookahead buffer: when tightening overshoots, the overshot interval
    /// is saved here so it's returned first on the next advance of that child.
    lookahead: Vec<Option<Interval>>,
    last_gaps: Option<u64>,
}

impl OrderedState {
    pub(crate) fn new(children: Vec<NodeState>) -> Self {
        let n = children.len();
        Self {
            children,
            current: vec![None; n],
            lookahead: vec![None; n],
            last_gaps: None,
        }
    }

    pub(crate) fn reset(&mut self) {
        for child in &mut self.children {
            child.reset();
        }
        self.current.fill(None);
        self.lookahead.fill(None);
        self.last_gaps = None;
    }

    /// Advance child k: returns buffered lookahead if present, else advances.
    fn child_next(&mut self, k: usize, positions: &impl TermPositions) -> Option<Interval> {
        if let Some(iv) = self.lookahead[k].take() {
            Some(iv)
        } else {
            self.children[k].next_interval(positions)
        }
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        let m = self.children.len();

        // Step 1: Advance child 0
        self.current[0] = self.child_next(0, positions);
        self.current[0]?;

        // Step 2: Build chain greedily — each child must start after the previous ends
        let mut i = 1;
        while i < m {
            loop {
                // Use cached current if it's already past the previous child
                if let Some(cur_i) = self.current[i]
                    && cur_i.start > self.current[i - 1].unwrap().end
                {
                    break;
                }
                // Advance child i
                let iv = self.child_next(i, positions)?;
                self.current[i] = Some(iv);
                if iv.start > self.current[i - 1].unwrap().end {
                    break;
                }
            }
            i += 1;
        }

        // Step 3: Tighten backwards — from child m-2 down to 0, advance each
        // child as far as possible while remaining before the next child.
        // Working backwards ensures each child is tightened relative to
        // the already-tightened child to its right.
        for k in (0..m - 1).rev() {
            let limit = self.current[k + 1].unwrap().start;
            loop {
                let next = self.children[k].next_interval(positions);
                match next {
                    Some(iv) if iv.end < limit => {
                        // Tighter — use this interval
                        self.current[k] = Some(iv);
                    }
                    other => {
                        // Either exhausted or overshot. Buffer the overshot
                        // for future use.
                        self.lookahead[k] = other;
                        break;
                    }
                }
            }
        }

        // Compute result
        let start = self.current[0].unwrap().start;
        let end = self.current[m - 1].unwrap().end;

        let child_width_sum = self
            .current
            .iter()
            .flatten()
            .map(|interval| u64::from(interval.width()))
            .sum();
        let interval = Interval::new(start, end);
        self.last_gaps = u64::from(interval.width()).checked_sub(child_width_sum);

        Some(interval)
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        self.last_gaps
    }
}
