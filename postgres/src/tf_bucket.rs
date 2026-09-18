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
//! Production-compatible term-frequency quantization.

/// Number of bits in a stored term-frequency bucket.
pub(crate) const BUCKET_BITS: u8 = 4;
/// Number of representable term-frequency buckets.
pub(crate) const BUCKET_COUNT: usize = 1 << BUCKET_BITS;
/// Largest valid bucket value.
pub(crate) const BUCKET_MAX: u8 = BUCKET_COUNT as u8 - 1;

/// The representative count is also the first positive count in the bucket.
const REPRESENTATIVE_COUNTS: [u32; BUCKET_COUNT] = [
    1, 2, 3, 5, 10, 20, 41, 85, 177, 371, 777, 1_626, 3_405, 7_132, 14_938, 31_288,
];

/// A validated four-bit term-frequency bucket.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct TfBucket(u8);

impl TfBucket {
    /// Quantizes a raw count using the production PagePack bucket boundaries.
    #[must_use]
    pub(crate) fn from_count(count: u32) -> Self {
        let positive_count = count.max(1);
        let first_greater = REPRESENTATIVE_COUNTS.partition_point(|&start| start <= positive_count);
        Self((first_greater - 1) as u8)
    }

    /// Validates a bucket decoded from an external representation.
    #[must_use]
    pub(crate) const fn new(value: u8) -> Option<Self> {
        if value <= BUCKET_MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub(crate) const fn value(self) -> u8 {
        self.0
    }

    /// Returns the count used by scoring for every raw count in this bucket.
    #[must_use]
    pub(crate) const fn representative_count(self) -> u32 {
        REPRESENTATIVE_COUNTS[self.0 as usize]
    }
}

/// Compatibility helper for callers that store buckets as raw bytes.
#[must_use]
#[cfg(test)]
pub(crate) fn quantize_tf_count(count: u32) -> u8 {
    TfBucket::from_count(count).value()
}

/// Compatibility helper for callers that already validated a stored bucket.
#[must_use]
#[cfg(test)]
pub(crate) fn representative_tf_count(bucket: u8) -> u32 {
    TfBucket::new(bucket)
        .expect("term-frequency bucket must fit in four bits")
        .representative_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RANGES: [(u8, u32, u32); BUCKET_COUNT] = [
        (0, 0, 1),
        (1, 2, 2),
        (2, 3, 4),
        (3, 5, 9),
        (4, 10, 19),
        (5, 20, 40),
        (6, 41, 84),
        (7, 85, 176),
        (8, 177, 370),
        (9, 371, 776),
        (10, 777, 1_625),
        (11, 1_626, 3_404),
        (12, 3_405, 7_131),
        (13, 7_132, 14_937),
        (14, 14_938, 31_287),
        (15, 31_288, u32::MAX),
    ];

    #[test]
    fn every_boundary_maps_to_the_expected_bucket() {
        for &(bucket, first, last) in &RANGES {
            assert_eq!(quantize_tf_count(first), bucket, "first={first}");
            assert_eq!(quantize_tf_count(last), bucket, "last={last}");
            assert_eq!(representative_tf_count(bucket), first.max(1));
        }
    }

    #[test]
    fn quantization_is_monotonic_across_all_non_saturated_counts() {
        let mut previous = 0;
        for count in 0..=u16::MAX as u32 {
            let bucket = quantize_tf_count(count);
            assert!(bucket >= previous, "count={count}");
            previous = bucket;
        }
        assert_eq!(quantize_tf_count(u32::MAX), BUCKET_MAX);
    }

    #[test]
    fn validates_raw_bucket_values() {
        for value in 0..=BUCKET_MAX {
            assert_eq!(TfBucket::new(value).map(TfBucket::value), Some(value));
        }
        assert_eq!(TfBucket::new(BUCKET_MAX + 1), None);
        assert_eq!(TfBucket::new(u8::MAX), None);
    }
}
