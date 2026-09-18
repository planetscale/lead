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
/// One matched query part over token-position coordinates.
///
/// `start` / `end` are inclusive token positions in the indexed token stream.
/// Point matches use `start == end`.
#[derive(Debug, PartialEq)]
pub struct MatchPosition {
    pub part: String,
    pub start: i32,
    pub end: i32,
}

impl MatchPosition {
    pub(crate) fn point(part: impl Into<String>, pos: u32) -> Self {
        Self::span(part, pos, pos)
    }

    pub(crate) fn span(part: impl Into<String>, start: u32, end: u32) -> Self {
        Self {
            part: part.into(),
            start: start as i32,
            end: end as i32,
        }
    }
}
