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
/// A positional match: contiguous region [start, end] in a document's
/// token stream. Both bounds are inclusive 0-indexed token positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval {
    pub start: u32,
    pub end: u32,
}

impl Interval {
    pub const fn new(start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "interval start must be <= end");
        Self { start, end }
    }

    /// Single-position interval [pos, pos].
    pub const fn point(pos: u32) -> Self {
        Self {
            start: pos,
            end: pos,
        }
    }

    /// Number of token positions spanned (inclusive).
    pub const fn width(self) -> u32 {
        self.end - self.start + 1
    }

    /// True if `self` fully contains `other`.
    pub const fn contains(self, other: Interval) -> bool {
        self.start <= other.start && self.end >= other.end
    }

    /// True if `self` and `other` share at least one position.
    pub const fn overlaps(self, other: Interval) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    /// True if every position in `self` is strictly before every position in `other`.
    pub const fn strictly_before(self, other: Interval) -> bool {
        self.end < other.start
    }
}

/// Natural ordering: by start position ascending, then by end position ascending.
/// This is the order in which intervals are produced by all operators.
impl Ord for Interval {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.start.cmp(&other.start).then(self.end.cmp(&other.end))
    }
}

impl PartialOrd for Interval {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_interval() {
        let p = Interval::point(5);
        assert_eq!(p.start, 5);
        assert_eq!(p.end, 5);
        assert_eq!(p.width(), 1);
    }

    #[test]
    fn width() {
        assert_eq!(Interval::new(2, 8).width(), 7);
        assert_eq!(Interval::new(0, 0).width(), 1);
    }

    #[test]
    fn contains() {
        let big = Interval::new(2, 8);
        let small = Interval::new(3, 6);
        assert!(big.contains(small));
        assert!(!small.contains(big));
        assert!(big.contains(big)); // self-containment
    }

    #[test]
    fn overlaps() {
        assert!(Interval::new(2, 5).overlaps(Interval::new(4, 8)));
        assert!(Interval::new(4, 8).overlaps(Interval::new(2, 5)));
        assert!(!Interval::new(2, 3).overlaps(Interval::new(5, 8)));
        assert!(Interval::new(2, 5).overlaps(Interval::new(5, 8))); // touching
    }

    #[test]
    fn strictly_before() {
        assert!(Interval::new(2, 3).strictly_before(Interval::new(5, 8)));
        assert!(!Interval::new(2, 5).strictly_before(Interval::new(5, 8)));
    }

    #[test]
    fn ordering() {
        let mut intervals = vec![
            Interval::new(5, 8),
            Interval::new(2, 4),
            Interval::new(2, 3),
            Interval::new(5, 6),
        ];
        intervals.sort();
        assert_eq!(
            intervals,
            vec![
                Interval::new(2, 3),
                Interval::new(2, 4),
                Interval::new(5, 6),
                Interval::new(5, 8),
            ]
        );
    }
}
