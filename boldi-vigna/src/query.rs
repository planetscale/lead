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
use crate::error::SpanError;

/// The operator tree for a span query.
///
/// Built by the caller, then compiled into a [`SpanSolver`](crate::SpanSolver)
/// for repeated evaluation against different documents' position data.
///
/// Every `Term(i)` leaf references a term by index. These indices correspond
/// to the [`TermPositions`](crate::TermPositions) provided at evaluation time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SpanQuery {
    // -- Leaf --
    /// Empty interval stream: matches nothing.
    Empty,

    /// Positions for the term at this index: produces `[pos, pos]` intervals.
    Term(usize),

    // -- Core composition (BV paper operators) --
    /// Ordered sequence (BV BLOCK / AND<): children must appear in document
    /// order, non-overlapping. Produces minimal spanning intervals.
    Ordered(Vec<SpanQuery>),

    /// Unordered conjunction (BV AND): minimal intervals containing one
    /// interval from each child, in any order. Structurally repeated children
    /// consume distinct intervals.
    Unordered(Vec<SpanQuery>),

    /// Disjunction (BV OR): union of child interval streams, keeping only
    /// minimal intervals (antichain-filtered).
    Or(Vec<SpanQuery>),

    // -- Filters --
    /// Keep only intervals from `inner` whose gap count ≤ `max_gaps`.
    /// Gaps are uncovered positions between non-overlapping sub-intervals;
    /// overlapping sub-intervals do not pass this filter.
    MaxGaps {
        max_gaps: u32,
        inner: Box<SpanQuery>,
    },

    /// Keep only intervals from `inner` whose gap count is within
    /// `min_gaps..=max_gaps`. The exact case (`min_gaps == max_gaps`)
    /// implements phrase gaps (`"a _ b"`), which require a pinned number of
    /// intervening positions rather than a budget.
    GapsInRange {
        min_gaps: u32,
        max_gaps: u32,
        inner: Box<SpanQuery>,
    },

    /// Keep only intervals from `inner` with width ≤ `max_width` (BV LOWPASS).
    MaxWidth {
        max_width: u32,
        inner: Box<SpanQuery>,
    },

    /// Keep only intervals where `start >= lo` and `end <= hi`.
    WithinPositions {
        inner: Box<SpanQuery>,
        lo: u32,
        hi: u32,
    },

    // -- Binary relation operators --
    /// Intervals from `big` that contain at least one interval from `little`.
    Containing {
        big: Box<SpanQuery>,
        little: Box<SpanQuery>,
    },

    /// Intervals from `little` that are contained by at least one from `big`.
    ContainedBy {
        little: Box<SpanQuery>,
        big: Box<SpanQuery>,
    },

    /// Intervals from `big` that do NOT contain any interval from `little`.
    NotContaining {
        big: Box<SpanQuery>,
        little: Box<SpanQuery>,
    },

    /// Intervals from `little` NOT contained by any interval from `big`.
    NotContainedBy {
        little: Box<SpanQuery>,
        big: Box<SpanQuery>,
    },

    /// Intervals from `a` that overlap at least one interval from `b`.
    Overlapping {
        a: Box<SpanQuery>,
        b: Box<SpanQuery>,
    },

    /// Intervals from `a` that do NOT overlap any interval from `b`.
    NonOverlapping {
        a: Box<SpanQuery>,
        b: Box<SpanQuery>,
    },

    /// Intervals from `a` for which some interval from `b` starts at a later
    /// position. The intervals may overlap.
    Before {
        a: Box<SpanQuery>,
        b: Box<SpanQuery>,
    },

    /// Intervals from `a` for which some interval from `b` starts at an
    /// earlier position. The intervals may overlap.
    After {
        a: Box<SpanQuery>,
        b: Box<SpanQuery>,
    },
}

/// A display wrapper that renders a span query with caller-provided term labels
/// substituted for `Term(i)` leaves.
pub struct SpanQueryWithTerms<'a, T> {
    pub(crate) query: &'a SpanQuery,
    pub(crate) terms: &'a [T],
}

impl SpanQuery {
    /// Exact phrase: `ordered(terms, max_gaps=0)`.
    pub fn phrase(term_indices: impl IntoIterator<Item = usize>) -> Self {
        SpanQuery::MaxGaps {
            max_gaps: 0,
            inner: Box::new(SpanQuery::Ordered(
                term_indices.into_iter().map(SpanQuery::Term).collect(),
            )),
        }
    }

    /// Sloppy phrase: `ordered(terms)` filtered by `max_gaps`.
    pub fn sloppy_phrase(term_indices: impl IntoIterator<Item = usize>, max_gaps: u32) -> Self {
        SpanQuery::MaxGaps {
            max_gaps,
            inner: Box::new(SpanQuery::Ordered(
                term_indices.into_iter().map(SpanQuery::Term).collect(),
            )),
        }
    }

    /// Match within the first `n` token positions of the document.
    pub fn first_n(inner: SpanQuery, n: u32) -> Self {
        debug_assert!(n >= 1, "first_n requires n >= 1");
        SpanQuery::WithinPositions {
            inner: Box::new(inner),
            lo: 0,
            hi: n - 1,
        }
    }

    /// Returns a display wrapper that renders `Term(i)` leaves using the
    /// caller-provided labels instead of numeric indices.
    pub fn display_with_terms<'a, T>(&'a self, terms: &'a [T]) -> SpanQueryWithTerms<'a, T> {
        SpanQueryWithTerms { query: self, terms }
    }

    /// Maximum term index referenced anywhere in this query tree.
    pub fn max_term_index(&self) -> Option<usize> {
        match self {
            SpanQuery::Empty => None,
            SpanQuery::Term(i) => Some(*i),
            SpanQuery::Ordered(children)
            | SpanQuery::Unordered(children)
            | SpanQuery::Or(children) => children.iter().filter_map(|c| c.max_term_index()).max(),
            SpanQuery::MaxGaps { inner, .. }
            | SpanQuery::GapsInRange { inner, .. }
            | SpanQuery::MaxWidth { inner, .. }
            | SpanQuery::WithinPositions { inner, .. } => inner.max_term_index(),
            SpanQuery::Containing { big, little } | SpanQuery::NotContaining { big, little } => {
                max_opt(big.max_term_index(), little.max_term_index())
            }
            SpanQuery::ContainedBy { little, big } | SpanQuery::NotContainedBy { little, big } => {
                max_opt(little.max_term_index(), big.max_term_index())
            }
            SpanQuery::Overlapping { a, b }
            | SpanQuery::NonOverlapping { a, b }
            | SpanQuery::Before { a, b }
            | SpanQuery::After { a, b } => max_opt(a.max_term_index(), b.max_term_index()),
        }
    }

    /// Number of distinct term indices needed (max_term_index + 1, or 0 if no terms).
    pub fn num_terms(&self) -> usize {
        self.max_term_index().map_or(0, |i| i + 1)
    }

    /// Validate the query tree against the expected number of terms.
    pub fn validate(&self, num_terms: usize) -> Result<(), SpanError> {
        match self {
            SpanQuery::Empty => {}
            SpanQuery::Term(i) => {
                if *i >= num_terms {
                    return Err(SpanError::TermIndexOutOfRange {
                        index: *i,
                        num_terms,
                    });
                }
            }
            SpanQuery::Ordered(children) | SpanQuery::Unordered(children) => {
                if children.len() < 2 {
                    return Err(SpanError::TooFewChildren {
                        min: 2,
                        got: children.len(),
                    });
                }
                for child in children {
                    child.validate(num_terms)?;
                }
            }
            SpanQuery::Or(children) => {
                if children.is_empty() {
                    return Err(SpanError::EmptyQuery);
                }
                for child in children {
                    child.validate(num_terms)?;
                }
            }
            SpanQuery::MaxGaps { inner, .. }
            | SpanQuery::GapsInRange { inner, .. }
            | SpanQuery::MaxWidth { inner, .. }
            | SpanQuery::WithinPositions { inner, .. } => {
                inner.validate(num_terms)?;
            }
            SpanQuery::Containing { big, little } | SpanQuery::NotContaining { big, little } => {
                big.validate(num_terms)?;
                little.validate(num_terms)?;
            }
            SpanQuery::ContainedBy { little, big } | SpanQuery::NotContainedBy { little, big } => {
                little.validate(num_terms)?;
                big.validate(num_terms)?;
            }
            SpanQuery::Overlapping { a, b }
            | SpanQuery::NonOverlapping { a, b }
            | SpanQuery::Before { a, b }
            | SpanQuery::After { a, b } => {
                a.validate(num_terms)?;
                b.validate(num_terms)?;
            }
        }
        Ok(())
    }
}

fn max_opt(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_sugar() {
        let q = SpanQuery::phrase([0, 1, 2]);
        assert!(matches!(q, SpanQuery::MaxGaps { max_gaps: 0, .. }));
        assert_eq!(q.num_terms(), 3);
    }

    #[test]
    fn validate_out_of_range() {
        let q = SpanQuery::Term(5);
        assert!(q.validate(3).is_err());
        assert!(q.validate(6).is_ok());
    }

    #[test]
    fn validate_too_few_children() {
        let q = SpanQuery::Ordered(vec![SpanQuery::Term(0)]);
        assert!(q.validate(1).is_err());
    }

    #[test]
    fn num_terms_nested() {
        let q = SpanQuery::Ordered(vec![
            SpanQuery::Term(0),
            SpanQuery::Or(vec![SpanQuery::Term(3), SpanQuery::Term(1)]),
        ]);
        assert_eq!(q.num_terms(), 4);
    }
}
