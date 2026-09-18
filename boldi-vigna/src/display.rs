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
use crate::query::{SpanQuery, SpanQueryWithTerms};
use std::fmt;

impl fmt::Display for SpanQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_span_query(self, f, &|idx, f| write!(f, "TERM({idx})"))
    }
}

impl<T: fmt::Display> fmt::Display for SpanQueryWithTerms<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_span_query(self.query, f, &|idx, f| match self.terms.get(idx) {
            Some(term) => write!(f, "{term}"),
            None => write!(f, "TERM({idx})"),
        })
    }
}

fn fmt_span_query<F>(query: &SpanQuery, f: &mut fmt::Formatter<'_>, leaf: &F) -> fmt::Result
where
    F: Fn(usize, &mut fmt::Formatter<'_>) -> fmt::Result,
{
    match query {
        SpanQuery::Empty => write!(f, "EMPTY"),
        SpanQuery::Term(idx) => leaf(*idx, f),
        SpanQuery::Ordered(children) => fmt_nary("ORDERED", children, f, leaf),
        SpanQuery::Unordered(children) => fmt_nary("UNORDERED", children, f, leaf),
        SpanQuery::Or(children) => fmt_nary("OR", children, f, leaf),
        SpanQuery::MaxGaps { max_gaps, inner } => {
            write!(f, "MAXGAPS({max_gaps}, ")?;
            fmt_span_query(inner, f, leaf)?;
            write!(f, ")")
        }
        SpanQuery::GapsInRange {
            min_gaps,
            max_gaps,
            inner,
        } => {
            write!(f, "GAPS({min_gaps}..{max_gaps}, ")?;
            fmt_span_query(inner, f, leaf)?;
            write!(f, ")")
        }
        SpanQuery::MaxWidth { max_width, inner } => {
            write!(f, "MAXWIDTH({max_width}, ")?;
            fmt_span_query(inner, f, leaf)?;
            write!(f, ")")
        }
        SpanQuery::WithinPositions { inner, lo, hi } => {
            write!(f, "WITHIN_POSITIONS({lo}, {hi}, ")?;
            fmt_span_query(inner, f, leaf)?;
            write!(f, ")")
        }
        SpanQuery::Containing { big, little } => fmt_binary("CONTAINING", big, little, f, leaf),
        SpanQuery::ContainedBy { little, big } => fmt_binary("CONTAINED_BY", little, big, f, leaf),
        SpanQuery::NotContaining { big, little } => {
            fmt_binary("NOT_CONTAINING", big, little, f, leaf)
        }
        SpanQuery::NotContainedBy { little, big } => {
            fmt_binary("NOT_CONTAINED_BY", little, big, f, leaf)
        }
        SpanQuery::Overlapping { a, b } => fmt_binary("OVERLAPPING", a, b, f, leaf),
        SpanQuery::NonOverlapping { a, b } => fmt_binary("NON_OVERLAPPING", a, b, f, leaf),
        SpanQuery::Before { a, b } => fmt_binary("BEFORE", a, b, f, leaf),
        SpanQuery::After { a, b } => fmt_binary("AFTER", a, b, f, leaf),
    }
}

fn fmt_nary<F>(
    name: &str,
    children: &[SpanQuery],
    f: &mut fmt::Formatter<'_>,
    leaf: &F,
) -> fmt::Result
where
    F: Fn(usize, &mut fmt::Formatter<'_>) -> fmt::Result,
{
    write!(f, "{name}(")?;
    for (idx, child) in children.iter().enumerate() {
        if idx > 0 {
            write!(f, ", ")?;
        }
        fmt_span_query(child, f, leaf)?;
    }
    write!(f, ")")
}

fn fmt_binary<F>(
    name: &str,
    left: &SpanQuery,
    right: &SpanQuery,
    f: &mut fmt::Formatter<'_>,
    leaf: &F,
) -> fmt::Result
where
    F: Fn(usize, &mut fmt::Formatter<'_>) -> fmt::Result,
{
    write!(f, "{name}(")?;
    fmt_span_query(left, f, leaf)?;
    write!(f, ", ")?;
    fmt_span_query(right, f, leaf)?;
    write!(f, ")")
}

#[cfg(test)]
mod tests {
    use crate::SpanQuery;

    #[test]
    fn display_uses_internal_operator_tree() {
        let q = SpanQuery::MaxWidth {
            max_width: 7,
            inner: Box::new(SpanQuery::Or(vec![SpanQuery::Term(0), SpanQuery::Term(1)])),
        };

        assert_eq!(q.to_string(), "MAXWIDTH(7, OR(TERM(0), TERM(1)))");
    }

    #[test]
    fn display_with_terms_substitutes_labels() {
        let q = SpanQuery::MaxWidth {
            max_width: 7,
            inner: Box::new(SpanQuery::Or(vec![SpanQuery::Term(0), SpanQuery::Term(1)])),
        };

        assert_eq!(
            q.display_with_terms(&["l.a", "nood1e"]).to_string(),
            "MAXWIDTH(7, OR(l.a, nood1e))"
        );
    }
}
