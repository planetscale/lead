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
use crate::interval::Interval;
use crate::positions::TermPositions;

/// Specialized evaluator for exact term-only phrases.
///
/// Compiled from `MaxGaps { max_gaps: 0, inner: Ordered(Term...) }`.
/// Instead of running the generic minimal-interval machinery, we choose the
/// rarest term in the current document as an anchor and probe the other term
/// position lists for exact offsets.
pub(crate) struct PhraseState {
    term_indices: Vec<usize>,
    phrase_width: u32,
    anchor_child: usize,
    anchor_cursor: usize,
    anchor_initialized: bool,
    exhausted: bool,
}

impl PhraseState {
    pub(crate) fn new(term_indices: Vec<usize>) -> Self {
        debug_assert!(term_indices.len() >= 2);
        Self {
            phrase_width: (term_indices.len() - 1) as u32,
            term_indices,
            anchor_child: 0,
            anchor_cursor: 0,
            anchor_initialized: false,
            exhausted: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.anchor_child = 0;
        self.anchor_cursor = 0;
        self.anchor_initialized = false;
        self.exhausted = false;
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        if !self.initialize_anchor(positions) {
            return None;
        }

        let anchor_term = self.term_indices[self.anchor_child];
        let anchor_positions = positions.positions(anchor_term);
        while self.anchor_cursor < anchor_positions.len() {
            let anchor_pos = anchor_positions[self.anchor_cursor];
            self.anchor_cursor += 1;

            let Some(start) = anchor_pos.checked_sub(self.anchor_child as u32) else {
                continue;
            };
            if self.matches_phrase_at(positions, start) {
                return Some(Interval::new(start, start + self.phrase_width));
            }
        }

        self.exhausted = true;
        None
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        Some(0)
    }

    fn initialize_anchor(&mut self, positions: &impl TermPositions) -> bool {
        if self.exhausted {
            return false;
        }
        if self.anchor_initialized {
            return true;
        }

        let mut best_child = 0usize;
        let mut best_len = usize::MAX;
        for (child_idx, &term_idx) in self.term_indices.iter().enumerate() {
            let len = positions.positions(term_idx).len();
            if len == 0 {
                self.exhausted = true;
                self.anchor_initialized = true;
                return false;
            }
            if len < best_len {
                best_len = len;
                best_child = child_idx;
            }
        }

        self.anchor_child = best_child;
        self.anchor_cursor = 0;
        self.anchor_initialized = true;
        true
    }

    fn matches_phrase_at(&self, positions: &impl TermPositions, start: u32) -> bool {
        for (child_idx, &term_idx) in self.term_indices.iter().enumerate() {
            let target = start + child_idx as u32;
            if positions
                .positions(term_idx)
                .binary_search(&target)
                .is_err()
            {
                return false;
            }
        }
        true
    }
}
