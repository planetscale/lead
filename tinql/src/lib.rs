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
pub mod ast;
pub mod error;
mod parser;
mod quote;
pub mod runtime;
mod util;

#[cfg(test)]
mod tests;

pub use ast::*;
pub use error::ParseError;
pub use quote::maybe_quote;

/// The operator inserted between adjacent expressions that lack an
/// explicit boolean keyword.
///
/// - `And` — `beer wine` → `beer AND wine` (Google-style)
/// - `Or` — `beer wine` → `beer OR wine` (Lucene default-style)
#[derive(Clone, Copy)]
pub enum ImplicitOp {
    And,
    Or,
}

/// Parse a query string into an expression tree.
///
/// `implicit_op` controls how adjacent expressions without an explicit
/// operator are joined: `beer wine` becomes `beer AND wine` with
/// [`ImplicitOp::And`], or `beer OR wine` with [`ImplicitOp::Or`].
///
/// # Errors
///
/// Returns [`ParseError`] if the input is not a valid query string.
pub fn parse(input: &str, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    if input.trim().is_empty() {
        // An empty query matches nothing; it is not a syntax error.
        return Ok(Expr::MatchNone);
    }
    parser::pest_parser::parse(input, implicit_op)
}
