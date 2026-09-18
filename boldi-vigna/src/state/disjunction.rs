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

/// BV OR algorithm: interval disjunction.
///
/// Merges child interval streams using ⊴ priority ordering (right-to-left)
/// and filters out non-minimal intervals to maintain the antichain property.
pub(crate) struct OrState {
    children: Vec<NodeState>,
    /// Current interval for each child.
    current: Vec<Option<Interval>>,
    /// Previously returned interval (for antichain filtering).
    prev: Option<Interval>,
    last_gaps: Option<u64>,
}

impl OrState {
    pub(crate) fn new(children: Vec<NodeState>) -> Self {
        let n = children.len();
        Self {
            children,
            current: vec![None; n],
            prev: None,
            last_gaps: None,
        }
    }

    pub(crate) fn reset(&mut self) {
        for child in &mut self.children {
            child.reset();
        }
        self.current.fill(None);
        self.prev = None;
        self.last_gaps = None;
    }

    /// Find the index of the child with the smallest current interval under ⊴.
    /// ⊴: smaller right endpoint wins; ties favor larger left (longer interval).
    fn min_child(&self) -> Option<(usize, Interval)> {
        let mut best: Option<(usize, Interval)> = None;
        for (i, cur) in self.current.iter().enumerate() {
            if let Some(iv) = cur {
                let is_better = match &best {
                    None => true,
                    Some((_, b)) => iv.end < b.end || (iv.end == b.end && iv.start > b.start),
                };
                if is_better {
                    best = Some((i, *iv));
                }
            }
        }
        best
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        // Initialize children that haven't been advanced yet
        for i in 0..self.children.len() {
            if self.current[i].is_none() {
                self.current[i] = self.children[i].next_interval(positions);
            }
        }

        loop {
            let (min_idx, min_iv) = self.min_child()?;
            let min_gaps = self.children[min_idx].gaps();

            // Advance the min child for next time
            self.current[min_idx] = self.children[min_idx].next_interval(positions);

            // Antichain filter: skip if the previous output contains this interval
            // (i.e., this interval is a super-interval of the previous output,
            // which would mean prev ⊆ min_iv — the ⊴ ordering plus the check
            // handles this). Actually: we skip if prev already returned something
            // that this new interval contains (non-minimal).
            //
            // Per the BV OR algorithm: skip while `c ⊆ top(Q)`, meaning the
            // previously returned interval is contained within the current top.
            // This means the current top is a super-interval of the previous
            // result and is therefore non-minimal.
            if let Some(prev) = self.prev
                && min_iv.contains(prev)
            {
                continue; // Non-minimal: contains previous output
            }

            self.prev = Some(min_iv);
            self.last_gaps = min_gaps;
            return Some(min_iv);
        }
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        self.last_gaps
    }
}
