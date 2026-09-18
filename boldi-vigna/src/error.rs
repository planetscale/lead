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
#[derive(Debug, thiserror::Error)]
pub enum SpanError {
    #[error("term index {index} out of range (num_terms = {num_terms})")]
    TermIndexOutOfRange { index: usize, num_terms: usize },

    #[error("operator requires at least {min} children, got {got}")]
    TooFewChildren { min: usize, got: usize },

    #[error("empty query")]
    EmptyQuery,
}
