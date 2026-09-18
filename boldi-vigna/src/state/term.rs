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

/// Leaf node state: cursor over a single term's position list.
/// Produces `[pos, pos]` intervals — one per occurrence.
pub(crate) struct TermState {
    term_index: usize,
    cursor: usize,
}

impl TermState {
    pub(crate) fn new(term_index: usize) -> Self {
        Self {
            term_index,
            cursor: 0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        let pos_list = positions.positions(self.term_index);
        if self.cursor < pos_list.len() {
            let pos = pos_list[self.cursor];
            self.cursor += 1;
            Some(Interval::point(pos))
        } else {
            None
        }
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        Some(0)
    }
}
