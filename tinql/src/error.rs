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
use thiserror::Error;

/// An error produced during query parsing.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("expected {expected} (at byte {pos}), found {found}")]
    Expected {
        expected: String,
        pos: usize,
        found: String,
    },

    #[error("empty alternatives (at byte {pos})")]
    EmptyAlternatives { pos: usize },

    #[error("empty phrase (at byte {pos})")]
    EmptyPhrase { pos: usize },

    #[error(
        "number \"{text}\" is out of range (at byte {pos}): must fit in an unsigned 32-bit integer"
    )]
    NumberOutOfRange { text: String, pos: usize },

    #[error(
        "range bound \"{text}\" contains a wildcard (at byte {pos}): bounds must be plain terms"
    )]
    WildcardInRangeBound { text: String, pos: usize },

    #[error(
        "boost factor \"{text}\" is out of range (at byte {pos}): must be a finite value of at most {max}",
        max = crate::ast::BoostFactor::MAX
    )]
    BoostOutOfRange { text: String, pos: usize },
}
