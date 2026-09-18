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
//! Shared pure-Rust tinql runtime.
//!
//! Owns the lowered query AST, sub-tokenization, lowering, capability
//! classification, and arbitrary-text evaluation used by both tin and qfuzz.

pub mod capability;
mod display;
pub mod eval;
pub mod lower;
pub mod position_filter;
pub mod regex;
pub mod simplify;
pub mod span_expr;
pub mod subtokenize;

pub use capability::{CapabilityClass, LoweringIssue, classify_expr, classify_query_text};
pub use eval::{
    EvalError, FuzzyMatcher, HighlightMatch, MatchResult, TokenizedDoc, evaluate,
    evaluate_for_highlight, range_matches, tokenize_doc,
};
pub use position_filter::{PositionFilterBound, ResolvedPositionWindow, SpanPositionFilter};
pub use regex::{CompiledRegex, RegexError};
pub use simplify::{SimplificationProfile, simplify};
pub use span_expr::SpanExpr;
use tokenizer::presets::default_pipeline;

/// A lowered query expression over normalized terms.
///
/// Boolean operators compose at the document level. Span queries carry a
/// positional constraint evaluated against token-position maps. Expansion nodes
/// (Regex, Range, Fuzzy) are retained in the lowered AST so each backend can
/// expand them against its own dictionary; wildcards normalize to Regex
/// during sub-tokenization.
/// PostgreSQL's DEFAULT_MATCH_SEL (selfuncs.h) — the planner-wide prior for
/// pattern-match selectivity when no statistic can resolve it. Core parity,
/// shared by every expansion query shape below.
pub const DEFAULT_EXPANSION_SELECTIVITY: f64 = 0.005;

#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    Term(String),
    And(Box<Query>, Box<Query>),
    Or(Box<Query>, Box<Query>),
    Conjunction(Vec<Query>),
    Disjunction {
        min: u32,
        children: Vec<Query>,
    },
    Not(Box<Query>),
    Span {
        term_slots: Vec<SpanTermSlot>,
        span_query: boldi_vigna::SpanQuery,
        position_filter: Option<SpanPositionFilter>,
    },
    SpanExpr {
        term_slots: Vec<SpanTermSlot>,
        span_expr: SpanExpr,
    },
    MatchAll,
    Regex(CompiledRegex),
    Range {
        lower: RangeBound,
        upper: RangeBound,
    },
    Fuzzy {
        term: String,
        prefix: u32,
        distance: u32,
    },
    Boost {
        factor: f32,
        inner: Box<Query>,
    },
    /// Compatibility form kept for older callers. New lowering emits
    /// [`Query::Disjunction`] instead.
    AtLeast {
        min: u32,
        children: Vec<Query>,
    },
}

/// A single slot in a span query's term list.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SpanTermSlot {
    Term(String),
    Regex(CompiledRegex),
    Range {
        lower: RangeBound,
        upper: RangeBound,
    },
    Fuzzy {
        term: String,
        prefix: u32,
        distance: u32,
    },
}

/// A bound in a term range expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RangeBound {
    Open,
    Term(String),
}

/// Threshold for min-should-match, resolved to an absolute count.
pub struct MinShouldMatch(pub u32);

impl Query {
    pub fn has_positive(&self) -> bool {
        match self {
            Query::Term(_)
            | Query::Span { .. }
            | Query::SpanExpr { .. }
            | Query::MatchAll
            | Query::Regex(_)
            | Query::Range { .. }
            | Query::Fuzzy { .. } => true,
            Query::Not(_) => false,
            Query::And(l, r) | Query::Or(l, r) => l.has_positive() || r.has_positive(),
            Query::Conjunction(children) => children.iter().any(Query::has_positive),
            Query::Disjunction { children, .. } => children.iter().any(Query::has_positive),
            Query::Boost { inner, .. } => inner.has_positive(),
            Query::AtLeast { children, .. } => children.iter().any(|c| c.has_positive()),
        }
    }

    pub fn estimate_tuples(&self, total_tuples: u64, lookup: &dyn Fn(&str) -> u64) -> u64 {
        let n = total_tuples as f64;
        self.estimate_selectivity(n, lookup, total_tuples) as u64
    }

    fn estimate_selectivity(&self, n: f64, lookup: &dyn Fn(&str) -> u64, total_tuples: u64) -> f64 {
        if n <= 0.0 {
            return 0.0;
        }
        match self {
            Query::Term(s) => lookup(s) as f64,
            Query::Span { term_slots, .. } | Query::SpanExpr { term_slots, .. } => {
                let dfs = term_slots.iter().filter_map(|slot| match slot {
                    SpanTermSlot::Term(s) => Some(lookup(s) as f64),
                    _ => None,
                });
                conjunction_estimate(n, dfs)
            }
            Query::And(..) => {
                // Flatten nested ANDs into a single list so we call
                // conjunction_estimate once on all terms. Recursive binary
                // estimation compounds the damping and collapses to zero.
                let mut positive = Vec::new();
                Self::collect_and_children(self, &mut positive);
                let dfs = positive
                    .iter()
                    .map(|c| c.estimate_selectivity(n, lookup, total_tuples));
                conjunction_estimate(n, dfs)
            }
            Query::Conjunction(children) => {
                let dfs = children
                    .iter()
                    .filter(|child| !matches!(child, Query::Not(_)))
                    .map(|child| child.estimate_selectivity(n, lookup, total_tuples));
                conjunction_estimate(n, dfs)
            }
            Query::Or(l, r) => disjunction_estimate(
                [l, r]
                    .iter()
                    .map(|c| c.estimate_selectivity(n, lookup, total_tuples)),
            ),
            Query::Disjunction { min, children } => {
                if *min == 1 {
                    disjunction_estimate(
                        children
                            .iter()
                            .map(|child| child.estimate_selectivity(n, lookup, total_tuples)),
                    )
                } else {
                    let mut estimates: Vec<u64> = children
                        .iter()
                        .map(|child| child.estimate_tuples(total_tuples, lookup))
                        .collect();
                    estimates.sort_unstable();
                    at_least_estimate(&estimates, *min)
                }
            }
            Query::Not(inner) => inner.estimate_selectivity(n, lookup, total_tuples),
            Query::MatchAll => n,
            Query::Regex(_) | Query::Range { .. } | Query::Fuzzy { .. } => {
                // Expansion shapes have no per-term df until execution
                // resolves them against the termdict. Use PostgreSQL's own
                // convention for an unknowable match selectivity
                // (DEFAULT_MATCH_SEL = 0.005, selfuncs.h) instead of the
                // former n/4 — which claimed a quarter of every corpus
                // matches any wildcard (48x over on measured cells).
                n * DEFAULT_EXPANSION_SELECTIVITY
            }
            Query::Boost { inner, .. } => inner.estimate_selectivity(n, lookup, total_tuples),
            Query::AtLeast { children, min, .. } => {
                let mut estimates: Vec<u64> = children
                    .iter()
                    .map(|c| c.estimate_tuples(total_tuples, lookup))
                    .collect();
                estimates.sort_unstable();
                at_least_estimate(&estimates, *min)
            }
        }
    }

    /// Flatten a binary AND tree into a list of positive (non-NOT) children.
    /// NOT children are dropped — they don't materially reduce the estimate
    /// for same-field text search.
    fn collect_and_children<'a>(query: &'a Query, out: &mut Vec<&'a Query>) {
        match query {
            Query::And(l, r) => {
                Self::collect_and_children(l, out);
                Self::collect_and_children(r, out);
            }
            Query::Conjunction(children) => {
                for child in children {
                    Self::collect_and_children(child, out);
                }
            }
            Query::Not(_) => { /* NOT children don't reduce the positive estimate */ }
            other => out.push(other),
        }
    }

    pub fn terms(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_terms(&mut out);
        out
    }

    fn collect_terms<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Query::Term(s) => out.push(s),
            Query::Span { term_slots, .. } | Query::SpanExpr { term_slots, .. } => {
                for slot in term_slots {
                    if let SpanTermSlot::Term(s) = slot {
                        out.push(s);
                    }
                }
            }
            Query::And(l, r) | Query::Or(l, r) => {
                l.collect_terms(out);
                r.collect_terms(out);
            }
            Query::Conjunction(children)
            | Query::Disjunction { children, .. }
            | Query::AtLeast { children, .. } => {
                for child in children {
                    child.collect_terms(out);
                }
            }
            Query::Not(inner) | Query::Boost { inner, .. } => inner.collect_terms(out),
            Query::Fuzzy { term, .. } => out.push(term),
            Query::MatchAll | Query::Regex(_) | Query::Range { .. } => {}
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("parse error: {0}")]
    Parse(#[from] crate::ParseError),
    #[error("sub-tokenize error: {0}")]
    SubTokenize(#[from] subtokenize::SubTokenizeError),
    #[error("lowering error: {0}")]
    Lower(#[from] lower::LowerError),
}

/// Conjunction (AND) estimate using exponential backoff on selectivities.
///
/// Sorts child selectivities from most to least selective, then applies
/// diminishing exponents: `N × S₁ × S₂^(1/2) × S₃^(1/4) × ...`. Each
/// successive term's filtering effect is halved, reflecting the reality
/// that once a few selective terms narrow the result set, additional
/// terms (especially common ones) contribute less incremental filtering.
///
/// This is the model SQL Server CE120+ uses for correlated predicates,
/// validated at scale since 2014. The result is clamped to `min(df_i)`
/// as an upper bound.
pub fn conjunction_estimate(total_tuples: f64, dfs: impl Iterator<Item = f64>) -> f64 {
    if total_tuples <= 0.0 {
        return 0.0;
    }
    let mut sels: Vec<f64> = dfs.map(|df| (df / total_tuples).clamp(0.0, 1.0)).collect();
    if sels.is_empty() {
        return 0.0;
    }
    // Sort ascending: most selective (smallest) first.
    sels.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut estimate = total_tuples * sels[0];
    let mut exponent = 1.0_f64;
    for &sel in &sels[1..] {
        exponent *= 0.5;
        estimate *= sel.powf(exponent);
    }
    // Clamp: never exceed the rarest child's df (upper bound).
    estimate.min(sels[0] * total_tuples).max(1.0)
}

/// Disjunction (OR) estimate: sum of document frequencies.
///
/// An upper bound on the union size. Overestimates when terms co-occur,
/// but cheap and directionally correct.
pub fn disjunction_estimate(dfs: impl Iterator<Item = f64>) -> f64 {
    dfs.sum()
}

/// AT LEAST `min` OF n estimate: the `min`-th largest child df from the
/// ascending-sorted `estimates`. Under the perfectly-correlated model
/// (each rarer term's documents nested inside the next commoner term's),
/// the documents matching at least `min` terms are exactly the `min`-th
/// most frequent term's — and the statistic is monotone: raising `min`
/// never raises the estimate, and no estimate exceeds the densest child.
/// `min` beyond the child count is unsatisfiable and estimates zero.
fn at_least_estimate(estimates: &[u64], min: u32) -> f64 {
    estimates
        .len()
        .checked_sub(min as usize)
        .and_then(|index| estimates.get(index))
        .copied()
        .unwrap_or(0) as f64
}

pub fn parse_tinql_to_query<T>(query_str: &str, tokenizer: &T) -> Result<Query, QueryError>
where
    T: tokenizer::Tokenizer,
{
    let expr = crate::parse(query_str, crate::ImplicitOp::And)?;
    let expr = subtokenize::sub_tokenize(expr, tokenizer)?;
    Ok(lower::lower(&expr)?)
}

pub fn parse_tinql_to_query_default(query_str: &str) -> Result<Query, QueryError> {
    parse_tinql_to_query(query_str, default_pipeline())
}
