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
/// Provides sorted token positions for leaf terms in a span query.
///
/// Each term in a [`SpanQuery::Term(i)`](crate::SpanQuery::Term) references
/// positions by index `i`. Implementors supply the sorted position list for
/// each index, borrowing existing data without copying.
pub trait TermPositions {
    /// Returns the sorted ascending positions for the term at `term_index`.
    ///
    /// # Panics
    ///
    /// May panic if `term_index` is out of range.
    fn positions(&self, term_index: usize) -> &[u32];
}

// --- Blanket implementations ---

impl TermPositions for Vec<Vec<u32>> {
    fn positions(&self, term_index: usize) -> &[u32] {
        &self[term_index]
    }
}

impl TermPositions for Vec<&[u32]> {
    fn positions(&self, term_index: usize) -> &[u32] {
        self[term_index]
    }
}

impl<const N: usize> TermPositions for [Vec<u32>; N] {
    fn positions(&self, term_index: usize) -> &[u32] {
        &self[term_index]
    }
}

impl<const N: usize> TermPositions for [&[u32]; N] {
    fn positions(&self, term_index: usize) -> &[u32] {
        self[term_index]
    }
}
