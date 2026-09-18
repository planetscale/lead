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
//! Lowers a `tinql::Expr` into the shared runtime `Query` / `SpanQuery`
//! representation.
//!
//! The lowering pass also canonicalizes boolean structure:
//!
//! - contiguous AND chains become `Query::Conjunction`
//! - OR and AT LEAST become `Query::Disjunction { min, children }`
//! - duplicate terms are removed from flat AND chains and plain OR chains
//! - `MatchAll` is folded out of compound boolean nodes
//! - single-term phrases collapse to plain `Term` nodes

use rustc_hash::FxHashMap;

use super::{
    CompiledRegex, PositionFilterBound, Query, RangeBound, SimplificationProfile, SpanExpr,
    SpanPositionFilter, SpanTermSlot, simplify,
};

#[derive(Debug, thiserror::Error)]
pub enum LowerError {
    #[error("MatchAll (*) is not valid inside a span/positional context")]
    MatchAllInSpanContext,
    #[error("{0}")]
    InvalidRegex(#[from] super::RegexError),
}

pub fn lower(expr: &crate::Expr) -> Result<Query, LowerError> {
    lower_with_profile(expr, SimplificationProfile::Structural)
}

pub fn lower_with_profile(
    expr: &crate::Expr,
    profile: SimplificationProfile,
) -> Result<Query, LowerError> {
    Ok(simplify(lower_boolean(expr)?, profile))
}

fn lower_boolean(expr: &crate::Expr) -> Result<Query, LowerError> {
    use crate::Expr;

    match expr {
        Expr::Term(s) => Ok(Query::Term(s.clone())),
        Expr::MatchAll => Ok(Query::MatchAll),
        // The canonical match-nothing query; simplify folds it out of
        // enclosing boolean nodes without disturbing AT LEAST thresholds.
        Expr::MatchNone => Ok(Query::Not(Box::new(Query::MatchAll))),
        Expr::Fuzzy {
            term,
            prefix,
            distance,
        } => Ok(Query::Fuzzy {
            term: term.clone(),
            prefix: *prefix,
            distance: *distance,
        }),
        Expr::Wildcard(parts) => Ok(Query::Regex(CompiledRegex::new(&wildcard_parts_regex(
            parts,
        ))?)),
        Expr::Regex(pat) => Ok(Query::Regex(CompiledRegex::new(pat)?)),
        Expr::Range { lower, upper } => Ok(Query::Range {
            lower: convert_range_bound(lower),
            upper: convert_range_bound(upper),
        }),
        Expr::And(l, r) => Ok(Query::Conjunction(vec![
            lower_boolean(l)?,
            lower_boolean(r)?,
        ])),
        Expr::Or(l, r) => Ok(Query::Disjunction {
            min: 1,
            children: vec![lower_boolean(l)?, lower_boolean(r)?],
        }),
        Expr::AndNot { positive, negative } => Ok(Query::Conjunction(vec![
            lower_boolean(positive)?,
            Query::Not(Box::new(lower_boolean(negative)?)),
        ])),
        Expr::Alternatives(xs) => Ok(Query::Disjunction {
            min: 1,
            children: xs
                .iter()
                .map(lower_boolean)
                .collect::<Result<Vec<_>, _>>()?,
        }),
        Expr::AtLeast { threshold, exprs } => {
            let min = resolve_threshold(threshold, exprs.len());
            let children = exprs
                .iter()
                .map(lower_boolean)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Query::Disjunction { min, children })
        }
        Expr::Boost { factor, inner } => Ok(Query::Boost {
            factor: factor.0,
            inner: Box::new(lower_boolean(inner)?),
        }),
        // A single-term phrase is just a term — no span machinery needed.
        Expr::Phrase { elements, .. }
            if elements.len() == 1 && matches!(elements[0], crate::PhraseElement::Term(_)) =>
        {
            match &elements[0] {
                crate::PhraseElement::Term(s) => Ok(Query::Term(s.clone())),
                _ => unreachable!(),
            }
        }
        Expr::Phrase { .. }
        | Expr::Then { .. }
        | Expr::Near { .. }
        | Expr::Encloses { .. }
        | Expr::NotEncloses { .. }
        | Expr::EnclosedBy { .. }
        | Expr::NotEnclosedBy { .. }
        | Expr::Overlapping { .. }
        | Expr::NotOverlapping { .. }
        | Expr::Before { .. }
        | Expr::After { .. }
        | Expr::First { .. }
        | Expr::Last { .. }
        | Expr::Middle { .. }
        | Expr::Between { .. }
        | Expr::Within { .. } => lower_as_span(expr),
    }
}

fn lower_as_span(expr: &crate::Expr) -> Result<Query, LowerError> {
    let mut builder = SpanBuilder::new();
    let span_expr = builder.lower_span_expr(expr)?;
    if let Some((span_query, position_filter)) = span_expr.to_fast_path_root() {
        Ok(Query::Span {
            term_slots: builder.term_slots,
            span_query,
            position_filter,
        })
    } else {
        Ok(Query::SpanExpr {
            term_slots: builder.term_slots,
            span_expr,
        })
    }
}

struct SpanBuilder {
    term_slots: Vec<SpanTermSlot>,
    intern_map: FxHashMap<SpanTermSlot, usize>,
}

impl SpanBuilder {
    fn new() -> Self {
        Self {
            term_slots: Vec::new(),
            intern_map: FxHashMap::default(),
        }
    }

    fn intern(&mut self, slot: SpanTermSlot) -> usize {
        if let Some(&idx) = self.intern_map.get(&slot) {
            return idx;
        }
        let idx = self.term_slots.len();
        self.intern_map.insert(slot.clone(), idx);
        self.term_slots.push(slot);
        idx
    }

    fn lower_span_expr(&mut self, expr: &crate::Expr) -> Result<SpanExpr, LowerError> {
        use crate::Expr;

        match expr {
            Expr::Term(s) => {
                let idx = self.intern(SpanTermSlot::Term(s.clone()));
                Ok(SpanExpr::Term(idx))
            }
            Expr::MatchAll => Err(LowerError::MatchAllInSpanContext),
            Expr::MatchNone => Ok(SpanExpr::Empty),
            Expr::Fuzzy {
                term,
                prefix,
                distance,
            } => {
                let idx = self.intern(SpanTermSlot::Fuzzy {
                    term: term.clone(),
                    prefix: *prefix,
                    distance: *distance,
                });
                Ok(SpanExpr::Term(idx))
            }
            Expr::Wildcard(parts) => {
                let idx = self.intern(SpanTermSlot::Regex(CompiledRegex::new(
                    &wildcard_parts_regex(parts),
                )?));
                Ok(SpanExpr::Term(idx))
            }
            Expr::Regex(pat) => {
                let idx = self.intern(SpanTermSlot::Regex(CompiledRegex::new(pat)?));
                Ok(SpanExpr::Term(idx))
            }
            Expr::Range { lower, upper } => {
                let idx = self.intern(SpanTermSlot::Range {
                    lower: convert_range_bound(lower),
                    upper: convert_range_bound(upper),
                });
                Ok(SpanExpr::Term(idx))
            }
            Expr::Phrase { elements, slop } => self.lower_phrase(elements, *slop),
            Expr::And(l, r) => {
                let left = self.lower_span_expr(l)?;
                let right = self.lower_span_expr(r)?;
                Ok(SpanExpr::Unordered(vec![left, right]))
            }
            Expr::Or(l, r) => {
                let left = self.lower_span_expr(l)?;
                let right = self.lower_span_expr(r)?;
                Ok(SpanExpr::Or(vec![left, right]))
            }
            Expr::AndNot { positive, negative } => {
                let big = self.lower_span_expr(positive)?;
                let little = self.lower_span_expr(negative)?;
                Ok(SpanExpr::NotContaining {
                    big: Box::new(big),
                    little: Box::new(little),
                })
            }
            Expr::Alternatives(xs) => {
                let children = xs
                    .iter()
                    .map(|e| self.lower_span_expr(e))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(SpanExpr::Or(children))
            }
            Expr::AtLeast { threshold, exprs } => {
                let min = resolve_threshold(threshold, exprs.len());
                let children = exprs
                    .iter()
                    .map(|expr| self.lower_span_expr(expr))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(SpanExpr::AtLeast { min, children })
            }
            Expr::Then { left, right, gap } => {
                let l = self.lower_span_expr(left)?;
                let r = self.lower_span_expr(right)?;
                Ok(SpanExpr::MaxGaps {
                    max_gaps: *gap,
                    inner: Box::new(SpanExpr::Ordered(vec![l, r])),
                })
            }
            Expr::Near { left, right, gap } => {
                let l = self.lower_span_expr(left)?;
                let r = self.lower_span_expr(right)?;
                Ok(SpanExpr::MaxGaps {
                    max_gaps: *gap,
                    inner: Box::new(SpanExpr::Unordered(vec![l, r])),
                })
            }
            Expr::Encloses { big, little } => {
                let b = self.lower_span_expr(big)?;
                let l = self.lower_span_expr(little)?;
                Ok(SpanExpr::Containing {
                    big: Box::new(b),
                    little: Box::new(l),
                })
            }
            Expr::NotEncloses { big, little } => {
                let b = self.lower_span_expr(big)?;
                let l = self.lower_span_expr(little)?;
                Ok(SpanExpr::NotContaining {
                    big: Box::new(b),
                    little: Box::new(l),
                })
            }
            Expr::EnclosedBy { little, big } => {
                let l = self.lower_span_expr(little)?;
                let b = self.lower_span_expr(big)?;
                Ok(SpanExpr::ContainedBy {
                    little: Box::new(l),
                    big: Box::new(b),
                })
            }
            Expr::NotEnclosedBy { little, big } => {
                let l = self.lower_span_expr(little)?;
                let b = self.lower_span_expr(big)?;
                Ok(SpanExpr::NotContainedBy {
                    little: Box::new(l),
                    big: Box::new(b),
                })
            }
            Expr::Overlapping { a, b } => {
                let qa = self.lower_span_expr(a)?;
                let qb = self.lower_span_expr(b)?;
                Ok(SpanExpr::Overlapping {
                    a: Box::new(qa),
                    b: Box::new(qb),
                })
            }
            Expr::NotOverlapping { a, b } => {
                let qa = self.lower_span_expr(a)?;
                let qb = self.lower_span_expr(b)?;
                Ok(SpanExpr::NonOverlapping {
                    a: Box::new(qa),
                    b: Box::new(qb),
                })
            }
            Expr::Before { a, b } => {
                let qa = self.lower_span_expr(a)?;
                let qb = self.lower_span_expr(b)?;
                Ok(SpanExpr::Before {
                    a: Box::new(qa),
                    b: Box::new(qb),
                })
            }
            Expr::After { a, b } => {
                let qa = self.lower_span_expr(a)?;
                let qb = self.lower_span_expr(b)?;
                Ok(SpanExpr::After {
                    a: Box::new(qa),
                    b: Box::new(qb),
                })
            }
            Expr::First { bound, inner } => Ok(SpanExpr::PositionFilter {
                inner: Box::new(self.lower_span_expr(inner)?),
                filter: SpanPositionFilter::First(PositionFilterBound::from(bound)),
            }),
            Expr::Last { bound, inner } => Ok(SpanExpr::PositionFilter {
                inner: Box::new(self.lower_span_expr(inner)?),
                filter: SpanPositionFilter::Last(PositionFilterBound::from(bound)),
            }),
            Expr::Middle { percent, inner } => Ok(SpanExpr::PositionFilter {
                inner: Box::new(self.lower_span_expr(inner)?),
                filter: SpanPositionFilter::Middle { percent: *percent },
            }),
            Expr::Between { lo, hi, inner } => Ok(SpanExpr::PositionFilter {
                inner: Box::new(self.lower_span_expr(inner)?),
                filter: SpanPositionFilter::Between { lo: *lo, hi: *hi },
            }),
            Expr::Within { width, inner } => {
                let sq = self.lower_span_expr(inner)?;
                Ok(SpanExpr::MaxWidth {
                    max_width: *width,
                    inner: Box::new(sq),
                })
            }
            Expr::Boost { inner, .. } => self.lower_span_expr(inner),
        }
    }

    fn lower_phrase(
        &mut self,
        elements: &[crate::PhraseElement],
        slop: Option<u32>,
    ) -> Result<SpanExpr, LowerError> {
        // Each child carries the gap pinned between it and the previous
        // child. Gaps before the first child or after the last have no
        // anchoring pair and are ignored.
        let mut children: Vec<(u32, SpanExpr)> = Vec::new();
        let mut pending_gap: u32 = 0;

        for elem in elements {
            match elem {
                crate::PhraseElement::Term(s) => {
                    let idx = self.intern(SpanTermSlot::Term(s.clone()));
                    children.push((pending_gap, SpanExpr::Term(idx)));
                    pending_gap = 0;
                }
                crate::PhraseElement::Gap(n) => {
                    pending_gap = pending_gap.saturating_add(*n);
                }
                crate::PhraseElement::Alternatives(exprs) => {
                    let alts = exprs
                        .iter()
                        .map(|e| self.lower_span_expr(e))
                        .collect::<Result<Vec<_>, _>>()?;
                    children.push((pending_gap, SpanExpr::Or(alts)));
                    pending_gap = 0;
                }
            }
        }

        if children.is_empty() {
            // Every element tokenized away ("...", emoji-only phrases): the
            // analyzed phrase is empty and matches nothing.
            return Ok(SpanExpr::Empty);
        }

        if children.len() == 1 {
            return Ok(children.into_iter().next().unwrap().1);
        }

        let total_gaps = children
            .iter()
            .skip(1)
            .fold(0u32, |acc, (gap, _)| acc.saturating_add(*gap));

        if total_gaps == 0 {
            // No interior gaps: slop is a total budget over the whole phrase.
            let flat = children.into_iter().map(|(_, child)| child).collect();
            return Ok(SpanExpr::MaxGaps {
                max_gaps: slop.unwrap_or(0),
                inner: Box::new(SpanExpr::Ordered(flat)),
            });
        }

        if let Some(slop) = slop {
            // Gaps combined with slop: the pinned gaps are the floor and the
            // slop adds a total tolerance on top of them.
            let flat = children.into_iter().map(|(_, child)| child).collect();
            return Ok(SpanExpr::GapsInRange {
                min_gaps: total_gaps,
                max_gaps: total_gaps.saturating_add(slop),
                inner: Box::new(SpanExpr::Ordered(flat)),
            });
        }

        // Exact gaps: pin every adjacency. Left-nested pairs keep each gap at
        // its own slot ("a _ b __ c" requires exactly one position between a
        // and b and exactly two between b and c), which one shared budget
        // over the flat sequence cannot express.
        let mut iter = children.into_iter();
        let mut acc = iter.next().expect("length checked above").1;
        for (gap, child) in iter {
            acc = SpanExpr::GapsInRange {
                min_gaps: gap,
                max_gaps: gap,
                inner: Box::new(SpanExpr::Ordered(vec![acc, child])),
            };
        }
        Ok(acc)
    }
}

/// Translates a wildcard pattern to its anchored-regex equivalent: `*` -> `.*`
/// (zero or more characters), `?` -> `.` (exactly one), literals escaped. The
/// sub-tokenize pass normally performs this conversion with tokenizer-folded
/// literals before lowering runs; this handles expressions lowered without
/// sub-tokenization, preserving the written literal text.
fn wildcard_parts_regex(parts: &[crate::WildcardPart]) -> String {
    let mut pattern = String::new();
    for part in parts {
        match part {
            crate::WildcardPart::Literal(s) => pattern.push_str(&regex_syntax::escape(s)),
            crate::WildcardPart::Any => pattern.push_str(".*"),
            crate::WildcardPart::Single => pattern.push('.'),
        }
    }
    pattern
}

fn convert_range_bound(b: &crate::RangeBound) -> RangeBound {
    match b {
        crate::RangeBound::Open => RangeBound::Open,
        crate::RangeBound::Term(s) => RangeBound::Term(s.clone()),
    }
}

fn resolve_threshold(threshold: &crate::AtLeastThreshold, n: usize) -> u32 {
    match threshold {
        crate::AtLeastThreshold::Count(c) => *c,
        crate::AtLeastThreshold::Percent(p) => (*p as u64 * n as u64).div_ceil(100) as u32,
        crate::AtLeastThreshold::All => n as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ImplicitOp;
    use tokenizer::presets::default_pipeline;

    fn parse(input: &str) -> crate::Expr {
        crate::parse(input, ImplicitOp::And).expect("query should parse")
    }

    fn parse_and_lower(input: &str) -> Query {
        let expr = super::super::subtokenize::sub_tokenize(parse(input), default_pipeline())
            .expect("query should sub-tokenize");
        lower(&expr).expect("query should lower")
    }

    #[test]
    fn top_level_runtime_position_filter_stays_on_fast_path() {
        let query = parse_and_lower("beer IN LAST 25%");
        assert!(matches!(query, Query::Span { .. }));
    }

    fn match_nothing() -> Query {
        Query::Not(Box::new(Query::MatchAll))
    }

    // NOT of a zero-token term excludes nothing, so the positive side of the
    // conjunction survives untouched.
    #[test]
    fn zero_token_negation_keeps_the_positive_side() {
        assert_eq!(
            parse_and_lower("beer AND NOT ,"),
            Query::Term("beer".into()),
        );
        assert_eq!(
            parse_and_lower("beer AND NOT ..."),
            Query::Term("beer".into()),
        );
        // As a positive conjunct the empty term matches nothing, and so does
        // the conjunction.
        assert_eq!(parse_and_lower("beer AND ,"), match_nothing());
    }

    // Comma-separated alternatives are the documented OR; the separator's
    // empty residue is dropped rather than poisoning the disjunction.
    #[test]
    fn comma_alternatives_lower_to_the_documented_or() {
        let expected = Query::Disjunction {
            min: 1,
            children: vec![Query::Term("beer".into()), Query::Term("wine".into())],
        };
        assert_eq!(parse_and_lower(r#"["beer", "wine"]"#), expected);
        assert_eq!(parse_and_lower("[beer,wine]"), expected);
    }

    // Empty alternatives never reduce an AT LEAST threshold.
    #[test]
    fn at_least_threshold_survives_comma_residue() {
        assert_eq!(
            parse_and_lower("AT LEAST 2 OF [beer , wine , stout]"),
            Query::Disjunction {
                min: 2,
                children: vec![
                    Query::Term("beer".into()),
                    Query::Term("wine".into()),
                    Query::Term("stout".into()),
                ],
            },
        );
        assert_eq!(
            parse_and_lower("AT LEAST 2 OF [beer~1, wine, water]"),
            Query::Disjunction {
                min: 2,
                children: vec![
                    Query::Fuzzy {
                        term: "beer".into(),
                        prefix: 1,
                        distance: 1,
                    },
                    Query::Term("wine".into()),
                    Query::Term("water".into()),
                ],
            },
        );
    }

    // An input that parses or analyzes to the empty query lowers to
    // match-nothing rather than erroring or matching the corpus.
    #[test]
    fn empty_and_zero_token_queries_lower_to_match_nothing() {
        assert_eq!(parse_and_lower(""), match_nothing());
        assert_eq!(parse_and_lower("..."), match_nothing());
        assert_eq!(parse_and_lower(".~1"), match_nothing());
        assert_eq!(parse_and_lower("@*"), match_nothing());
        // A phrase whose every element tokenizes away is an empty span —
        // the span-context spelling of match-nothing, not an error.
        assert!(matches!(
            parse_and_lower("\"...\""),
            Query::Span {
                span_query: boldi_vigna::SpanQuery::Empty,
                ..
            }
        ));
    }

    // Phrase gaps are exact: each adjacency is pinned at its own slot, which
    // one shared max-gaps budget cannot express.
    #[test]
    fn phrase_gaps_lower_to_exact_pinned_pairs() {
        let Query::Span { span_query, .. } = parse_and_lower("\"alpha _ gamma\"") else {
            panic!("expected fast-path span lowering");
        };
        assert_eq!(
            span_query,
            boldi_vigna::SpanQuery::GapsInRange {
                min_gaps: 1,
                max_gaps: 1,
                inner: Box::new(boldi_vigna::SpanQuery::Ordered(vec![
                    boldi_vigna::SpanQuery::Term(0),
                    boldi_vigna::SpanQuery::Term(1),
                ])),
            },
        );

        // Multi-gap phrases pin each slot pairwise.
        let Query::Span { span_query, .. } = parse_and_lower("\"a _ b __ c\"") else {
            panic!("expected fast-path span lowering");
        };
        assert_eq!(
            span_query,
            boldi_vigna::SpanQuery::GapsInRange {
                min_gaps: 2,
                max_gaps: 2,
                inner: Box::new(boldi_vigna::SpanQuery::Ordered(vec![
                    boldi_vigna::SpanQuery::GapsInRange {
                        min_gaps: 1,
                        max_gaps: 1,
                        inner: Box::new(boldi_vigna::SpanQuery::Ordered(vec![
                            boldi_vigna::SpanQuery::Term(0),
                            boldi_vigna::SpanQuery::Term(1),
                        ])),
                    },
                    boldi_vigna::SpanQuery::Term(2),
                ])),
            },
        );

        // Slop keeps its budget meaning: without gaps it is a plain budget,
        // with gaps the pinned total becomes the floor.
        let Query::Span { span_query, .. } = parse_and_lower("\"alpha gamma\"~1") else {
            panic!("expected fast-path span lowering");
        };
        assert!(matches!(
            span_query,
            boldi_vigna::SpanQuery::MaxGaps { max_gaps: 1, .. }
        ));
        let Query::Span { span_query, .. } = parse_and_lower("\"alpha _ gamma\"~1") else {
            panic!("expected fast-path span lowering");
        };
        assert!(matches!(
            span_query,
            boldi_vigna::SpanQuery::GapsInRange {
                min_gaps: 1,
                max_gaps: 2,
                ..
            }
        ));
    }

    #[test]
    fn lowering_rejects_invalid_regex() {
        let err = lower(&crate::Expr::Regex("(".into())).expect_err("regex should fail");
        assert!(matches!(err, LowerError::InvalidRegex(_)));
    }

    #[test]
    fn lowered_regex_carries_compiled_full_term_matcher() {
        let query = parse_and_lower("MATCHES alp[a-z]*");
        let Query::Regex(regex) = query else {
            panic!("expected regex query");
        };

        assert_eq!(regex.source(), "alp[a-z]*");
        assert!(regex.is_match("alpha"));
        assert!(!regex.is_match("xalpha"));
    }

    #[test]
    fn nested_runtime_position_filter_uses_span_expr_fallback() {
        let query = parse_and_lower("(beer IN LAST 25%) BEFORE wine");
        let Query::SpanExpr {
            term_slots,
            span_expr,
        } = query
        else {
            panic!("expected advanced span expression lowering");
        };

        assert_eq!(
            term_slots,
            vec![
                SpanTermSlot::Term("beer".into()),
                SpanTermSlot::Term("wine".into()),
            ]
        );
        assert!(matches!(
            span_expr,
            SpanExpr::Before {
                a,
                b,
            } if matches!(
                a.as_ref(),
                SpanExpr::PositionFilter {
                    filter: SpanPositionFilter::Last(PositionFilterBound::Percent(25)),
                    ..
                }
            ) && matches!(b.as_ref(), SpanExpr::Term(1))
        ));
    }

    #[test]
    fn at_least_inside_span_context_uses_span_expr_fallback() {
        let query = parse_and_lower("(AT LEAST 2 OF [quick, brown, fox]) WITHIN 3");
        let Query::SpanExpr { span_expr, .. } = query else {
            panic!("expected advanced span expression lowering");
        };

        assert!(matches!(
            span_expr,
            SpanExpr::MaxWidth { max_width: 3, inner }
                if matches!(
                    inner.as_ref(),
                    SpanExpr::AtLeast { min: 2, children } if children.len() == 3
                )
        ));
    }
}
