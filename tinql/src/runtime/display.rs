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
use super::{Query, RangeBound, SpanExpr, SpanTermSlot};
use std::fmt;

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Query::Term(s) => write!(f, "{s}"),
            Query::And(l, r) => write!(f, "AND({l}, {r})"),
            Query::Or(l, r) => write!(f, "OR({l}, {r})"),
            Query::Conjunction(children) => fmt_nary_query("AND", f, children),
            Query::Disjunction { min, children } if *min == 1 => fmt_nary_query("OR", f, children),
            Query::Disjunction { min, children } => {
                write!(f, "ATLEAST({min}")?;
                for child in children {
                    write!(f, ", {child}")?;
                }
                write!(f, ")")
            }
            Query::Not(inner) => write!(f, "NOT({inner})"),
            Query::Span {
                term_slots,
                span_query,
                position_filter,
            } => fmt_span_display(f, term_slots, span_query, position_filter.as_ref()),
            Query::SpanExpr {
                term_slots,
                span_expr,
            } => fmt_span_expr_display(f, term_slots, span_expr),
            Query::MatchAll => write!(f, "*"),
            Query::Regex(pat) => write!(f, "REGEX({pat})"),
            Query::Range { lower, upper } => write!(f, "RANGE({lower}, {upper})"),
            Query::Fuzzy {
                term,
                prefix,
                distance,
            } => write!(f, "{term}{}", fuzzy_suffix(*prefix, *distance)),
            Query::Boost { factor, inner } => {
                // Surface caret syntax so the printed query re-parses to this
                // same IR. The caret binds to one primary: terms and fuzzy
                // terms take it directly; every other inner needs the grouped
                // form.
                let factor = crate::BoostFactor(*factor);
                match inner.as_ref() {
                    Query::Term(_) | Query::Fuzzy { .. } => {
                        write!(f, "{inner}^{factor}")
                    }
                    _ => write!(f, "({inner})^{factor}"),
                }
            }
            Query::AtLeast { min, children } => {
                write!(f, "ATLEAST({min}")?;
                for child in children {
                    write!(f, ", {child}")?;
                }
                write!(f, ")")
            }
        }
    }
}

fn fmt_nary_query(name: &str, f: &mut fmt::Formatter<'_>, children: &[Query]) -> fmt::Result {
    write!(f, "{name}(")?;
    for (idx, child) in children.iter().enumerate() {
        if idx > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{child}")?;
    }
    write!(f, ")")
}

impl fmt::Display for SpanTermSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpanTermSlot::Term(s) => write!(f, "{s}"),
            SpanTermSlot::Regex(pat) => write!(f, "/{pat}/"),
            SpanTermSlot::Range { lower, upper } => write!(f, "{lower} TO {upper}"),
            SpanTermSlot::Fuzzy {
                term,
                prefix,
                distance,
            } => write!(f, "{term}{}", fuzzy_suffix(*prefix, *distance)),
        }
    }
}

impl fmt::Display for RangeBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RangeBound::Open => write!(f, "*"),
            RangeBound::Term(s) => write!(f, "{s}"),
        }
    }
}

fn fmt_span_expr_display(
    f: &mut fmt::Formatter<'_>,
    term_slots: &[SpanTermSlot],
    span_expr: &SpanExpr,
) -> fmt::Result {
    if let Some((span_query, position_filter)) = span_expr.to_fast_path_root() {
        return fmt_span_display(f, term_slots, &span_query, position_filter.as_ref());
    }

    write!(f, "SPAN(")?;
    fmt_span_expr_with_sugar(f, term_slots, span_expr)?;
    write!(f, ")")
}

fn fuzzy_suffix(prefix: u32, distance: u32) -> String {
    if prefix == 1 {
        format!("~{distance}")
    } else {
        format!("~{prefix}:{distance}")
    }
}

fn fmt_span_display(
    f: &mut fmt::Formatter<'_>,
    term_slots: &[SpanTermSlot],
    span_query: &boldi_vigna::SpanQuery,
    position_filter: Option<&super::SpanPositionFilter>,
) -> fmt::Result {
    if let Some((max_gaps, terms)) = phrase_like_terms(term_slots, span_query) {
        fmt_phrase_like(f, max_gaps, &terms)?;
    } else {
        write!(f, "SPAN(")?;
        fmt_span_query_with_sugar(f, term_slots, span_query)?;
        write!(f, ")")?;
    }

    if let Some(position_filter) = position_filter {
        write!(f, " {position_filter}")?;
    }

    Ok(())
}

fn fmt_span_query_with_sugar(
    f: &mut fmt::Formatter<'_>,
    term_slots: &[SpanTermSlot],
    span_query: &boldi_vigna::SpanQuery,
) -> fmt::Result {
    use boldi_vigna::SpanQuery;

    if let Some((max_gaps, terms)) = phrase_like_terms(term_slots, span_query) {
        return fmt_phrase_like(f, max_gaps, &terms);
    }

    match span_query {
        SpanQuery::Empty => write!(f, "EMPTY"),
        SpanQuery::Term(idx) => match term_slots.get(*idx) {
            Some(slot) => write!(f, "{slot}"),
            None => write!(f, "TERM({idx})"),
        },
        SpanQuery::Ordered(children) => fmt_span_nary("ORDERED", f, term_slots, children),
        SpanQuery::Unordered(children) => fmt_span_nary("UNORDERED", f, term_slots, children),
        SpanQuery::Or(children) => fmt_span_nary("OR", f, term_slots, children),
        SpanQuery::MaxGaps { max_gaps, inner } => {
            write!(f, "MAXGAPS({max_gaps}, ")?;
            fmt_span_query_with_sugar(f, term_slots, inner)?;
            write!(f, ")")
        }
        SpanQuery::GapsInRange {
            min_gaps,
            max_gaps,
            inner,
        } => {
            write!(f, "GAPS({min_gaps}..{max_gaps}, ")?;
            fmt_span_query_with_sugar(f, term_slots, inner)?;
            write!(f, ")")
        }
        SpanQuery::MaxWidth { max_width, inner } => {
            write!(f, "MAXWIDTH({max_width}, ")?;
            fmt_span_query_with_sugar(f, term_slots, inner)?;
            write!(f, ")")
        }
        SpanQuery::WithinPositions { inner, lo, hi } => {
            write!(f, "WITHIN_POSITIONS({lo}, {hi}, ")?;
            fmt_span_query_with_sugar(f, term_slots, inner)?;
            write!(f, ")")
        }
        SpanQuery::Containing { big, little } => {
            fmt_span_binary("CONTAINING", f, term_slots, big, little)
        }
        SpanQuery::ContainedBy { little, big } => {
            fmt_span_binary("CONTAINED_BY", f, term_slots, little, big)
        }
        SpanQuery::NotContaining { big, little } => {
            fmt_span_binary("NOT_CONTAINING", f, term_slots, big, little)
        }
        SpanQuery::NotContainedBy { little, big } => {
            fmt_span_binary("NOT_CONTAINED_BY", f, term_slots, little, big)
        }
        SpanQuery::Overlapping { a, b } => fmt_span_binary("OVERLAPPING", f, term_slots, a, b),
        SpanQuery::NonOverlapping { a, b } => {
            fmt_span_binary("NON_OVERLAPPING", f, term_slots, a, b)
        }
        SpanQuery::Before { a, b } => fmt_span_binary("BEFORE", f, term_slots, a, b),
        SpanQuery::After { a, b } => fmt_span_binary("AFTER", f, term_slots, a, b),
    }
}

fn fmt_span_nary(
    name: &str,
    f: &mut fmt::Formatter<'_>,
    term_slots: &[SpanTermSlot],
    children: &[boldi_vigna::SpanQuery],
) -> fmt::Result {
    write!(f, "{name}(")?;
    for (idx, child) in children.iter().enumerate() {
        if idx > 0 {
            write!(f, ", ")?;
        }
        fmt_span_query_with_sugar(f, term_slots, child)?;
    }
    write!(f, ")")
}

fn fmt_span_binary(
    name: &str,
    f: &mut fmt::Formatter<'_>,
    term_slots: &[SpanTermSlot],
    left: &boldi_vigna::SpanQuery,
    right: &boldi_vigna::SpanQuery,
) -> fmt::Result {
    write!(f, "{name}(")?;
    fmt_span_query_with_sugar(f, term_slots, left)?;
    write!(f, ", ")?;
    fmt_span_query_with_sugar(f, term_slots, right)?;
    write!(f, ")")
}

fn fmt_span_expr_with_sugar(
    f: &mut fmt::Formatter<'_>,
    term_slots: &[SpanTermSlot],
    span_expr: &SpanExpr,
) -> fmt::Result {
    match span_expr {
        SpanExpr::Empty => write!(f, "EMPTY"),
        SpanExpr::Term(idx) => match term_slots.get(*idx) {
            Some(slot) => write!(f, "{slot}"),
            None => write!(f, "TERM({idx})"),
        },
        SpanExpr::Ordered(children) => fmt_span_expr_nary("ORDERED", f, term_slots, children),
        SpanExpr::Unordered(children) => fmt_span_expr_nary("UNORDERED", f, term_slots, children),
        SpanExpr::Or(children) => fmt_span_expr_nary("OR", f, term_slots, children),
        SpanExpr::AtLeast { min, children } => {
            write!(f, "ATLEAST({min}")?;
            for child in children {
                write!(f, ", ")?;
                fmt_span_expr_with_sugar(f, term_slots, child)?;
            }
            write!(f, ")")
        }
        SpanExpr::MaxGaps { max_gaps, inner } => {
            write!(f, "MAXGAPS({max_gaps}, ")?;
            fmt_span_expr_with_sugar(f, term_slots, inner)?;
            write!(f, ")")
        }
        SpanExpr::GapsInRange {
            min_gaps,
            max_gaps,
            inner,
        } => {
            write!(f, "GAPS({min_gaps}..{max_gaps}, ")?;
            fmt_span_expr_with_sugar(f, term_slots, inner)?;
            write!(f, ")")
        }
        SpanExpr::MaxWidth { max_width, inner } => {
            write!(f, "MAXWIDTH({max_width}, ")?;
            fmt_span_expr_with_sugar(f, term_slots, inner)?;
            write!(f, ")")
        }
        SpanExpr::WithinPositions { inner, lo, hi } => {
            write!(f, "WITHIN_POSITIONS({lo}, {hi}, ")?;
            fmt_span_expr_with_sugar(f, term_slots, inner)?;
            write!(f, ")")
        }
        SpanExpr::PositionFilter { inner, filter } => {
            write!(f, "POSITION_FILTER({filter}, ")?;
            fmt_span_expr_with_sugar(f, term_slots, inner)?;
            write!(f, ")")
        }
        SpanExpr::Containing { big, little } => {
            fmt_span_expr_binary("CONTAINING", f, term_slots, big, little)
        }
        SpanExpr::ContainedBy { little, big } => {
            fmt_span_expr_binary("CONTAINED_BY", f, term_slots, little, big)
        }
        SpanExpr::NotContaining { big, little } => {
            fmt_span_expr_binary("NOT_CONTAINING", f, term_slots, big, little)
        }
        SpanExpr::NotContainedBy { little, big } => {
            fmt_span_expr_binary("NOT_CONTAINED_BY", f, term_slots, little, big)
        }
        SpanExpr::Overlapping { a, b } => fmt_span_expr_binary("OVERLAPPING", f, term_slots, a, b),
        SpanExpr::NonOverlapping { a, b } => {
            fmt_span_expr_binary("NON_OVERLAPPING", f, term_slots, a, b)
        }
        SpanExpr::Before { a, b } => fmt_span_expr_binary("BEFORE", f, term_slots, a, b),
        SpanExpr::After { a, b } => fmt_span_expr_binary("AFTER", f, term_slots, a, b),
    }
}

fn fmt_span_expr_nary(
    name: &str,
    f: &mut fmt::Formatter<'_>,
    term_slots: &[SpanTermSlot],
    children: &[SpanExpr],
) -> fmt::Result {
    write!(f, "{name}(")?;
    for (idx, child) in children.iter().enumerate() {
        if idx > 0 {
            write!(f, ", ")?;
        }
        fmt_span_expr_with_sugar(f, term_slots, child)?;
    }
    write!(f, ")")
}

fn fmt_span_expr_binary(
    name: &str,
    f: &mut fmt::Formatter<'_>,
    term_slots: &[SpanTermSlot],
    left: &SpanExpr,
    right: &SpanExpr,
) -> fmt::Result {
    write!(f, "{name}(")?;
    fmt_span_expr_with_sugar(f, term_slots, left)?;
    write!(f, ", ")?;
    fmt_span_expr_with_sugar(f, term_slots, right)?;
    write!(f, ")")
}

fn phrase_like_terms<'a>(
    term_slots: &'a [SpanTermSlot],
    span_query: &boldi_vigna::SpanQuery,
) -> Option<(u32, Vec<&'a str>)> {
    let (max_gaps, children) = match span_query {
        boldi_vigna::SpanQuery::MaxGaps { max_gaps, inner } => match inner.as_ref() {
            boldi_vigna::SpanQuery::Ordered(children) if children.len() >= 2 => {
                (*max_gaps, children.as_slice())
            }
            _ => return None,
        },
        _ => return None,
    };

    let mut terms = Vec::with_capacity(children.len());
    for child in children {
        match child {
            boldi_vigna::SpanQuery::Term(idx) => match term_slots.get(*idx) {
                Some(SpanTermSlot::Term(term)) => terms.push(term.as_str()),
                _ => return None,
            },
            _ => return None,
        }
    }

    Some((max_gaps, terms))
}

fn fmt_phrase_like(f: &mut fmt::Formatter<'_>, max_gaps: u32, terms: &[&str]) -> fmt::Result {
    if max_gaps == 0 {
        write!(f, "PHRASE(")?;
    } else {
        write!(f, "PHRASE/{max_gaps}(")?;
    }
    write!(f, "\"")?;
    for (idx, term) in terms.iter().enumerate() {
        if idx > 0 {
            write!(f, " ")?;
        }
        fmt_phrase_term(f, term)?;
    }
    write!(f, "\")")
}

fn fmt_phrase_term(f: &mut fmt::Formatter<'_>, term: &str) -> fmt::Result {
    for c in term.chars() {
        if matches!(c, '\\' | '"' | '[' | ']' | '_') {
            write!(f, "\\")?;
        }
        write!(f, "{c}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{PositionFilterBound, SpanPositionFilter};
    use boldi_vigna::SpanQuery;

    #[test]
    fn display_span_includes_positional_structure() {
        let query = Query::Span {
            term_slots: vec![
                SpanTermSlot::Term("l.a".into()),
                SpanTermSlot::Term("nood1e".into()),
            ],
            span_query: SpanQuery::MaxWidth {
                max_width: 7,
                inner: Box::new(SpanQuery::Or(vec![SpanQuery::Term(0), SpanQuery::Term(1)])),
            },
            position_filter: None,
        };

        assert_eq!(query.to_string(), "SPAN(MAXWIDTH(7, OR(l.a, nood1e)))");
    }

    #[test]
    fn display_exact_phrase_uses_phrase_sugar() {
        let query = Query::Span {
            term_slots: vec![
                SpanTermSlot::Term("big".into()),
                SpanTermSlot::Term("bad".into()),
                SpanTermSlot::Term("wolf".into()),
            ],
            span_query: SpanQuery::MaxGaps {
                max_gaps: 0,
                inner: Box::new(SpanQuery::Ordered(vec![
                    SpanQuery::Term(0),
                    SpanQuery::Term(1),
                    SpanQuery::Term(2),
                ])),
            },
            position_filter: None,
        };

        assert_eq!(query.to_string(), r#"PHRASE("big bad wolf")"#);
    }

    #[test]
    fn display_sloppy_phrase_uses_phrase_slop_sugar() {
        let query = Query::Span {
            term_slots: vec![
                SpanTermSlot::Term("craft".into()),
                SpanTermSlot::Term("beer".into()),
            ],
            span_query: SpanQuery::MaxGaps {
                max_gaps: 2,
                inner: Box::new(SpanQuery::Ordered(vec![
                    SpanQuery::Term(0),
                    SpanQuery::Term(1),
                ])),
            },
            position_filter: None,
        };

        assert_eq!(query.to_string(), r#"PHRASE/2("craft beer")"#);
    }

    #[test]
    fn display_phrase_sugar_escapes_phrase_specials() {
        let query = Query::Span {
            term_slots: vec![
                SpanTermSlot::Term(r#"say"hi"#.into()),
                SpanTermSlot::Term(r#"under_score"#.into()),
            ],
            span_query: SpanQuery::MaxGaps {
                max_gaps: 0,
                inner: Box::new(SpanQuery::Ordered(vec![
                    SpanQuery::Term(0),
                    SpanQuery::Term(1),
                ])),
            },
            position_filter: None,
        };

        assert_eq!(query.to_string(), r#"PHRASE("say\"hi under\_score")"#);
    }

    #[test]
    fn display_nested_phrase_subtree_uses_phrase_sugar() {
        let query = Query::Span {
            term_slots: vec![
                SpanTermSlot::Term("bad".into()),
                SpanTermSlot::Term("cow".into()),
                SpanTermSlot::Term("eat".into()),
                SpanTermSlot::Term("more".into()),
                SpanTermSlot::Term("chicken".into()),
            ],
            span_query: SpanQuery::MaxGaps {
                max_gaps: 5,
                inner: Box::new(SpanQuery::Unordered(vec![
                    SpanQuery::Term(0),
                    SpanQuery::Unordered(vec![
                        SpanQuery::Term(1),
                        SpanQuery::MaxGaps {
                            max_gaps: 0,
                            inner: Box::new(SpanQuery::Ordered(vec![
                                SpanQuery::Term(2),
                                SpanQuery::Term(3),
                                SpanQuery::Term(4),
                            ])),
                        },
                    ]),
                ])),
            },
            position_filter: None,
        };

        assert_eq!(
            query.to_string(),
            r#"SPAN(MAXGAPS(5, UNORDERED(bad, UNORDERED(cow, PHRASE("eat more chicken")))))"#
        );
    }

    #[test]
    fn display_boost_uses_caret_surface_syntax() {
        let boosted_term = Query::Boost {
            factor: 2.0,
            inner: Box::new(Query::Term("beer".into())),
        };
        assert_eq!(boosted_term.to_string(), "beer^2");

        let fractional = Query::Boost {
            factor: 1.5,
            inner: Box::new(Query::Term("beer".into())),
        };
        assert_eq!(fractional.to_string(), "beer^1.5");

        let boosted_fuzzy = Query::Boost {
            factor: 3.0,
            inner: Box::new(Query::Fuzzy {
                term: "beer".into(),
                prefix: 1,
                distance: 2,
            }),
        };
        assert_eq!(boosted_fuzzy.to_string(), "beer~2^3");

        let boosted_group = Query::Boost {
            factor: 2.0,
            inner: Box::new(Query::MatchAll),
        };
        assert_eq!(boosted_group.to_string(), "(*)^2");
    }

    #[test]
    fn display_fuzzy_uses_tilde_surface_syntax() {
        let default_prefix = Query::Fuzzy {
            term: "beer".into(),
            prefix: 1,
            distance: 2,
        };
        assert_eq!(default_prefix.to_string(), "beer~2");

        let explicit_prefix = Query::Fuzzy {
            term: "beer".into(),
            prefix: 3,
            distance: 2,
        };
        assert_eq!(explicit_prefix.to_string(), "beer~3:2");
    }

    #[test]
    fn display_span_appends_position_filter() {
        let query = Query::Span {
            term_slots: vec![SpanTermSlot::Term("beer".into())],
            span_query: SpanQuery::Term(0),
            position_filter: Some(SpanPositionFilter::Last(PositionFilterBound::Absolute(5))),
        };

        assert_eq!(query.to_string(), "SPAN(beer) IN LAST 5 WORDS");
    }
}
