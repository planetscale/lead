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
use boldi_vigna::Interval;
use std::fmt;

/// Runtime bound shared by `FIRST` and `LAST` positional filters.
#[derive(Debug, Clone, PartialEq)]
pub enum PositionFilterBound {
    Absolute(u32),
    Percent(u32),
}

/// Query-layer positional filter applied to span intervals after the base
/// minimal-interval query has been evaluated.
#[derive(Debug, Clone, PartialEq)]
pub enum SpanPositionFilter {
    First(PositionFilterBound),
    Last(PositionFilterBound),
    Middle { percent: u32 },
    Between { lo: u32, hi: u32 },
}

/// Inclusive token-position window resolved for one document.
#[derive(Debug, PartialEq)]
pub struct ResolvedPositionWindow {
    pub lo: u32,
    pub hi: u32,
}

impl SpanPositionFilter {
    pub fn needs_doc_length(&self) -> bool {
        matches!(
            self,
            Self::First(PositionFilterBound::Percent(_)) | Self::Last(_) | Self::Middle { .. }
        )
    }

    /// Resolve this filter against the given document length.
    ///
    /// Index-backed callers should pass the immutable sidecar's exact token
    /// count so filter boundaries match scored search and runtime evaluation.
    pub fn resolve_window(&self, search_doc_len: u32) -> Option<ResolvedPositionWindow> {
        match *self {
            Self::First(PositionFilterBound::Absolute(count)) => {
                ResolvedPositionWindow::from_start_count(0, count)
            }
            Self::First(PositionFilterBound::Percent(percent)) => {
                ResolvedPositionWindow::from_start_count(0, percent_count(search_doc_len, percent))
            }
            Self::Last(PositionFilterBound::Absolute(count)) => {
                let count = count.min(search_doc_len);
                let start = search_doc_len.saturating_sub(count);
                ResolvedPositionWindow::from_start_count(start, count)
            }
            Self::Last(PositionFilterBound::Percent(percent)) => {
                let count = percent_count(search_doc_len, percent);
                let start = search_doc_len.saturating_sub(count);
                ResolvedPositionWindow::from_start_count(start, count)
            }
            Self::Middle { percent } => {
                // `IN MIDDLE N%` is documented as the complement of the two
                // ends: the first and last (100-N)/2 percent are excluded.
                // The excluded ends round up exactly like FIRST/LAST N% do,
                // so MIDDLE never overlaps the windows those filters resolve.
                let percent = percent.min(100);
                let excluded_per_side = u64::from(search_doc_len)
                    .saturating_mul(u64::from(100 - percent))
                    .div_ceil(200) as u32;
                let hi = search_doc_len
                    .checked_sub(excluded_per_side + 1)
                    .filter(|hi| *hi >= excluded_per_side)?;
                Some(ResolvedPositionWindow {
                    lo: excluded_per_side,
                    hi,
                })
            }
            // `IN WORDS lo TO hi` counts words from 1, like FIRST/LAST do:
            // `IN WORDS 1 TO n` is the same window as `IN FIRST n WORDS`.
            Self::Between { lo, hi } if lo <= hi && hi >= 1 => Some(ResolvedPositionWindow {
                lo: lo.max(1) - 1,
                hi: hi - 1,
            }),
            Self::Between { .. } => None,
        }
    }

    pub fn matches_interval(&self, search_doc_len: u32, interval: Interval) -> bool {
        self.resolve_window(search_doc_len)
            .is_some_and(|window| window.matches_interval(interval))
    }

    pub fn matches_start_width(&self, search_doc_len: u32, start: u32, width: u32) -> bool {
        self.resolve_window(search_doc_len)
            .is_some_and(|window| window.matches_start_width(start, width))
    }
}

impl ResolvedPositionWindow {
    pub fn matches_interval(&self, interval: Interval) -> bool {
        self.matches_start_width(interval.start, interval.width())
    }

    pub fn matches_start_width(&self, start: u32, width: u32) -> bool {
        if width == 0 || start < self.lo {
            return false;
        }
        let Some(end) = start.checked_add(width - 1) else {
            return false;
        };
        end <= self.hi
    }

    fn from_start_count(start: u32, count: u32) -> Option<Self> {
        if count == 0 {
            return None;
        }
        let hi = start.checked_add(count - 1)?;
        Some(Self { lo: start, hi })
    }
}

impl From<&crate::PositionBound> for PositionFilterBound {
    fn from(value: &crate::PositionBound) -> Self {
        match value {
            crate::PositionBound::Absolute(n) => Self::Absolute(*n),
            crate::PositionBound::Percent(n) => Self::Percent(*n),
        }
    }
}

impl fmt::Display for SpanPositionFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::First(PositionFilterBound::Absolute(n)) => write!(f, "IN FIRST {n} WORDS"),
            Self::First(PositionFilterBound::Percent(n)) => write!(f, "IN FIRST {n}%"),
            Self::Last(PositionFilterBound::Absolute(n)) => write!(f, "IN LAST {n} WORDS"),
            Self::Last(PositionFilterBound::Percent(n)) => write!(f, "IN LAST {n}%"),
            Self::Middle { percent } => write!(f, "IN MIDDLE {percent}%"),
            Self::Between { lo, hi } => write!(f, "IN WORDS {lo} TO {hi}"),
        }
    }
}

fn percent_count(doc_len: u32, percent: u32) -> u32 {
    let percent = percent.min(100);
    doc_len.saturating_mul(percent).div_ceil(100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_and_between_windows_use_inclusive_bounds() {
        let first = SpanPositionFilter::First(PositionFilterBound::Absolute(3));
        assert_eq!(
            first.resolve_window(0),
            Some(ResolvedPositionWindow { lo: 0, hi: 2 })
        );

        // User-facing word numbers count from 1; the engine window is 0-based.
        let between = SpanPositionFilter::Between { lo: 4, hi: 9 };
        assert_eq!(
            between.resolve_window(0),
            Some(ResolvedPositionWindow { lo: 3, hi: 8 })
        );

        let first_word = SpanPositionFilter::Between { lo: 1, hi: 1 };
        assert_eq!(
            first_word.resolve_window(0),
            Some(ResolvedPositionWindow { lo: 0, hi: 0 })
        );
    }

    #[test]
    fn relative_windows_resolve_from_search_doc_length() {
        let first = SpanPositionFilter::First(PositionFilterBound::Percent(25));
        assert_eq!(
            first.resolve_window(10),
            Some(ResolvedPositionWindow { lo: 0, hi: 2 })
        );

        let last = SpanPositionFilter::Last(PositionFilterBound::Absolute(4));
        assert_eq!(
            last.resolve_window(10),
            Some(ResolvedPositionWindow { lo: 6, hi: 9 })
        );

        // MIDDLE 50% of 10 tokens excludes the first and last 25% windows
        // (positions 0..2 and 7..9, both rounded up like FIRST/LAST 25%).
        let middle = SpanPositionFilter::Middle { percent: 50 };
        assert_eq!(
            middle.resolve_window(10),
            Some(ResolvedPositionWindow { lo: 3, hi: 6 })
        );
    }

    // IN WORDS counts words from 1 like FIRST/LAST do: 1 TO n is the same
    // window as FIRST n WORDS, n TO n is the n-th word, and there is no
    // word 0.
    #[test]
    fn in_words_counts_words_from_one() {
        assert_eq!(
            SpanPositionFilter::Between { lo: 1, hi: 3 }.resolve_window(0),
            SpanPositionFilter::First(PositionFilterBound::Absolute(3)).resolve_window(0),
        );
        assert_eq!(
            SpanPositionFilter::Between { lo: 3, hi: 3 }.resolve_window(0),
            Some(ResolvedPositionWindow { lo: 2, hi: 2 })
        );
        assert_eq!(
            SpanPositionFilter::Between { lo: 0, hi: 0 }.resolve_window(0),
            None
        );
        assert_eq!(
            SpanPositionFilter::Between { lo: 0, hi: 2 }.resolve_window(0),
            SpanPositionFilter::Between { lo: 1, hi: 2 }.resolve_window(0),
        );
    }

    // IN MIDDLE N% is the complement of the two ends, which round up exactly
    // like FIRST/LAST N% do — so MIDDLE 50% on 10 tokens excludes FIRST 25%
    // (0..2) and LAST 25% (7..9) entirely.
    #[test]
    fn middle_excludes_the_rounded_ends() {
        let middle = SpanPositionFilter::Middle { percent: 50 }
            .resolve_window(10)
            .unwrap();
        let first = SpanPositionFilter::First(PositionFilterBound::Percent(25))
            .resolve_window(10)
            .unwrap();
        let last = SpanPositionFilter::Last(PositionFilterBound::Percent(25))
            .resolve_window(10)
            .unwrap();
        assert_eq!(middle, ResolvedPositionWindow { lo: 3, hi: 6 });
        assert!(middle.lo > first.hi);
        assert!(middle.hi < last.lo);

        // Evenly divisible lengths keep the historical window.
        assert_eq!(
            SpanPositionFilter::Middle { percent: 50 }.resolve_window(100),
            Some(ResolvedPositionWindow { lo: 25, hi: 74 })
        );
        // A window narrower than its excluded ends resolves to nothing.
        assert_eq!(
            SpanPositionFilter::Middle { percent: 0 }.resolve_window(10),
            None
        );
    }

    #[test]
    fn resolved_windows_filter_intervals_and_phrase_starts() {
        let window = ResolvedPositionWindow { lo: 2, hi: 5 };
        assert!(window.matches_interval(Interval::new(2, 5)));
        assert!(!window.matches_interval(Interval::new(1, 5)));
        assert!(window.matches_start_width(2, 3));
        assert!(!window.matches_start_width(4, 3));
    }
}
