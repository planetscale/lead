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

/// The deepest `(`/`[` nesting reached in `input`, counting grouping and
/// alternatives but not the atomic contents of a double-quoted phrase (a
/// `\` there escapes the next character). The grammar recurses one level per
/// enclosing bracket, so this bounds the parser's recursion depth from above,
/// and since each level opens a bracket it never exceeds `input.len()`. An
/// unmatched closer floors the running depth at zero rather than going
/// negative, so the result is an upper bound on any valid parse's depth and a
/// cheap pre-parse guard against stack-overflowing deeply nested input.
pub fn max_bracket_depth(input: &str) -> usize {
    let mut depth: usize = 0;
    let mut max: usize = 0;
    let mut in_phrase = false;
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if in_phrase {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_phrase = false,
                _ => {}
            }
        } else {
            match c {
                '"' => in_phrase = true,
                '(' | '[' => {
                    depth += 1;
                    max = max.max(depth);
                }
                ')' | ']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    max
}

#[cfg(test)]
mod depth_tests {
    use super::max_bracket_depth;

    #[test]
    fn counts_nested_grouping_and_alternatives() {
        assert_eq!(max_bracket_depth(""), 0);
        assert_eq!(max_bracket_depth("beer"), 0);
        assert_eq!(max_bracket_depth("(beer OR wine)"), 1);
        assert_eq!(max_bracket_depth("((a))"), 2);
        assert_eq!(max_bracket_depth("[a [b [c]]]"), 3);
        // Mixed brackets nest together.
        assert_eq!(max_bracket_depth("([a])"), 2);
        // The deepest point wins, not the last.
        assert_eq!(max_bracket_depth("((a)) (b)"), 2);
    }

    #[test]
    fn ignores_brackets_inside_a_phrase() {
        assert_eq!(max_bracket_depth("\"[[[[\""), 0);
        // An escaped quote stays inside the phrase; the trailing group counts.
        assert_eq!(max_bracket_depth("\"a \\\" b\" (c)"), 1);
        // An unterminated phrase swallows the rest, so nothing after counts.
        assert_eq!(max_bracket_depth("(a) \"[[["), 1);
    }

    #[test]
    fn unmatched_closers_floor_at_zero() {
        assert_eq!(max_bracket_depth(")))"), 0);
        assert_eq!(max_bracket_depth("a) (b"), 1);
        assert_eq!(max_bracket_depth("((("), 3);
    }

    #[test]
    fn depth_never_exceeds_length() {
        for q in ["", "beer", "(((x)))", "[a, b, c]", "\"[[[\" (())"] {
            assert!(max_bracket_depth(q) <= q.len());
        }
    }
}
