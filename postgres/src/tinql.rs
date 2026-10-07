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
//! The backend's only entry into TINQL parsing.
//!
//! Every parse of query text in this crate goes through [`parse`], which
//! rejects text that is longer than [`MAX_QUERY_BYTES`] or nested deeper than
//! [`MAX_NESTING_DEPTH`] with SQLSTATE 54000 (`program_limit_exceeded`) before
//! the parser runs.

use ::tinql::runtime::{
    Query, QueryError, SimplificationProfile,
    lower::{lower, lower_with_profile},
    subtokenize::sub_tokenize,
};
use tokenizer::Tokenizer;

/// Longest accepted TINQL query text, in bytes.
pub(crate) const MAX_QUERY_BYTES: usize = 2 * 1024;

/// Deepest `(`/`[` nesting the parser will accept. It recurses one stack
/// frame per enclosing bracket, and an unoptimized build overflows an 8 MiB
/// stack near 450 levels; this caps well below that, leaving margin for
/// smaller stacks, and no real query nests anywhere near this deep. The byte
/// cap alone cannot stand in for this: a backend running on a smaller stack
/// could overflow within [`MAX_QUERY_BYTES`].
pub(crate) const MAX_NESTING_DEPTH: usize = 64;

/// Parse TINQL text with implicit AND, raising `program_limit_exceeded` when
/// it is longer than [`MAX_QUERY_BYTES`] or nested deeper than
/// [`MAX_NESTING_DEPTH`].
pub(crate) fn parse(query: &str) -> Result<::tinql::Expr, ::tinql::ParseError> {
    if query.len() > MAX_QUERY_BYTES {
        pgrx::ereport!(
            ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "tinql query is too long",
            format!(
                "The query is {} bytes; the limit is {MAX_QUERY_BYTES} bytes.",
                query.len()
            ),
        );
    }
    // Nesting depth never exceeds the byte length (each level opens a
    // bracket), so a query no longer than the limit cannot reach it; scan only
    // the ones that could.
    if query.len() > MAX_NESTING_DEPTH {
        let depth = ::tinql::max_bracket_depth(query);
        if depth > MAX_NESTING_DEPTH {
            pgrx::ereport!(
                ERROR,
                pgrx::PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "tinql query is nested too deeply",
                format!("The query nests {depth} levels deep; the limit is {MAX_NESTING_DEPTH}."),
            );
        }
    }
    ::tinql::parse(query, ::tinql::ImplicitOp::And)
}

/// Parse a tinql string, normalize it through the tokenizer, and lower it to a query AST.
pub(crate) fn parse_tinql_to_query<T: Tokenizer>(
    query: &str,
    tokenizer: &T,
) -> Result<Query, QueryError> {
    Ok(lower(&sub_tokenize(parse(query)?, tokenizer)?)?)
}

/// Like [`parse_tinql_to_query`], but keeps every written occurrence of a
/// repeated term, which is what scoring weighs.
pub(crate) fn parse_scoring_tinql_to_query<T: Tokenizer>(
    query: &str,
    tokenizer: &T,
) -> Result<Query, QueryError> {
    Ok(lower_with_profile(
        &sub_tokenize(parse(query)?, tokenizer)?,
        SimplificationProfile::StructuralPreserveTermMultiplicity,
    )?)
}
