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
use crate::{LongTokenMode, LongTokenSpec, MIN_TOKEN_BYTES, PositionGapMode, Token};
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) struct LongTokenIter<'text, I, const DEFAULT: bool> {
    inner: I,
    spec: LongTokenSpec,
    pending: Option<PendingToken<'text>>,
    position_gaps: PositionGapMode,
    next_collapsed_pos: u32,
    split_position_offset: u32,
}

struct PendingToken<'text> {
    token: Token<'text>,
    byte_pos: usize,
    // Whole next grapheme, or the unconsumed suffix of an oversized one.
    next_grapheme_bytes: Option<usize>,
}

impl<'text, I, const DEFAULT: bool> LongTokenIter<'text, I, DEFAULT>
where
    I: Iterator<Item = Token<'text>>,
{
    /// Extra positions created by split continuations: each continuation
    /// chunk after the first occupies one additional position.
    pub(crate) fn split_position_offset(&self) -> u32 {
        self.split_position_offset
    }

    pub(crate) fn inner(&self) -> &I {
        &self.inner
    }

    pub(crate) fn new(inner: I, spec: LongTokenSpec, position_gaps: PositionGapMode) -> Self {
        Self {
            inner,
            spec,
            pending: None,
            position_gaps,
            next_collapsed_pos: 0,
            split_position_offset: 0,
        }
    }

    #[inline]
    fn next_default(&mut self) -> Option<Token<'text>> {
        loop {
            if self.pending.is_some() {
                return self.next_split_chunk();
            }

            let token = self.inner.next()?;
            if token.text.len() <= 256 {
                return Some(self.position(token, false));
            }
            self.start_split(token);
        }
    }

    #[inline]
    fn next_configured(&mut self) -> Option<Token<'text>> {
        loop {
            if self.pending.is_some() {
                return self.next_split_chunk();
            }

            let mut token = self.inner.next()?;
            if token.text.len() <= self.spec.max_bytes {
                return Some(self.position(token, false));
            }

            match self.spec.mode {
                LongTokenMode::Discard => continue,
                LongTokenMode::Truncate => {
                    let end = truncate_end(&token.text, self.spec.max_bytes);
                    if end == 0 {
                        continue;
                    }
                    match &mut token.text {
                        Cow::Borrowed(text) => *text = &text[..end],
                        Cow::Owned(text) => text.truncate(end),
                    }
                    return Some(self.position(token, false));
                }
                LongTokenMode::Split => self.start_split(token),
            }
        }
    }

    fn start_split(&mut self, token: Token<'text>) {
        debug_assert!(token.text.len() > self.spec.max_bytes);
        debug_assert!(self.spec.max_bytes >= MIN_TOKEN_BYTES);
        self.pending = Some(PendingToken {
            token,
            byte_pos: 0,
            next_grapheme_bytes: None,
        });
    }

    fn next_split_chunk(&mut self) -> Option<Token<'text>> {
        let mut pending = self
            .pending
            .take()
            .expect("split chunk requires a pending token");
        let start = pending.byte_pos;
        let (end, next_grapheme_bytes) = split_end(
            &pending.token.text,
            start,
            self.spec.max_bytes,
            pending.next_grapheme_bytes,
        );
        let continuation = start != 0;
        let was_split = continuation || end < pending.token.text.len();
        let text = match &pending.token.text {
            Cow::Borrowed(text) => Cow::Borrowed(&text[start..end]),
            Cow::Owned(text) => Cow::Owned(text[start..end].to_owned()),
        };
        let mut chunk = Token {
            text,
            pos: pending.token.pos,
            classification: pending.token.classification,
            came_from_split: false,
        };
        if was_split {
            chunk.mark_split();
        }

        if end < pending.token.text.len() {
            pending.byte_pos = end;
            pending.next_grapheme_bytes = next_grapheme_bytes;
            self.pending = Some(pending);
        }

        Some(self.position(chunk, continuation))
    }

    #[inline]
    fn position(&mut self, mut token: Token<'text>, split_continuation: bool) -> Token<'text> {
        if DEFAULT || self.position_gaps == PositionGapMode::Preserve {
            if split_continuation {
                self.split_position_offset = self
                    .split_position_offset
                    .checked_add(1)
                    .expect("token position overflow");
            }
            token.pos = token
                .pos
                .checked_add(self.split_position_offset)
                .expect("token position overflow");
        } else {
            token.pos = self.next_collapsed_pos;
            self.next_collapsed_pos = self
                .next_collapsed_pos
                .checked_add(1)
                .expect("token position overflow");
        }
        token
    }
}

impl<'text, I, const DEFAULT: bool> Iterator for LongTokenIter<'text, I, DEFAULT>
where
    I: Iterator<Item = Token<'text>>,
{
    type Item = Token<'text>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if DEFAULT {
            self.next_default()
        } else {
            self.next_configured()
        }
    }
}

/// Is every byte in `bytes` a plain ASCII byte other than CR?
///
/// Within such a window every byte is exactly one grapheme cluster: ASCII
/// bytes never combine with each other except CR pairing with a following LF
/// into a single 2-byte CRLF cluster, and CR is excluded here.
#[inline]
fn ascii_without_cr(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b < 0x80 && b != b'\r')
}

/// Does a grapheme cluster boundary provably lie right before `bytes[cap]`,
/// given that `bytes[window_start..cap]` is ASCII and CR-free?
///
/// `cap == bytes.len()` is the end-of-text boundary. An ASCII byte at `cap`
/// always begins a new cluster: the boundary before a CR is always valid, and
/// an LF at `cap` preceded by CR is impossible because that CR would sit
/// inside the CR-free window (or, for an empty window, `cap` is already a
/// known boundary checked by the caller). A non-ASCII byte at `cap` may be a
/// combining mark extending the cluster that starts at `cap - 1`, so it is
/// rejected.
#[inline]
fn ascii_boundary_at(bytes: &[u8], cap: usize) -> bool {
    cap == bytes.len() || bytes[cap] < 0x80
}

pub(crate) fn truncate_end(text: &str, max_bytes: usize) -> usize {
    let bytes = text.as_bytes();
    let cap = max_bytes.min(bytes.len());
    // Fast path: every byte in the window is its own grapheme cluster and the
    // cut at `cap` lands on a cluster boundary, so the grapheme walk below
    // would advance exactly one byte at a time and stop at `cap`.
    if ascii_without_cr(&bytes[..cap]) && ascii_boundary_at(bytes, cap) {
        return cap;
    }

    let mut end = 0;
    for (_, grapheme) in text.grapheme_indices(true) {
        if end + grapheme.len() > max_bytes {
            break;
        }
        end += grapheme.len();
    }
    end
}

pub(crate) fn split_end(
    text: &str,
    start: usize,
    max_bytes: usize,
    first_grapheme_bytes: Option<usize>,
) -> (usize, Option<usize>) {
    let mut end = start;
    if let Some(bytes) = first_grapheme_bytes {
        if bytes > max_bytes {
            end = scalar_end(text, start, max_bytes);
            return (end, Some(bytes - (end - start)));
        }
        end += bytes;
    }

    // Fast path: the byte budget is measured from `start`, so the chunk can
    // extend at most to `cap`. When the remaining window is ASCII and CR-free,
    // every byte in it is its own grapheme cluster, and an ASCII (or
    // end-of-text) boundary at `cap` means the cut is a valid cluster
    // boundary, so the grapheme walk below would stop exactly at `cap`.
    // Returning `None` instead of the walk's lookahead carry is safe: the
    // carry is a pure recomputation-avoidance hint, and a `None` makes the
    // next call rescan from `cap` with identical results.
    let bytes = text.as_bytes();
    let cap = start.saturating_add(max_bytes).min(bytes.len());
    if end <= cap && ascii_without_cr(&bytes[end..cap]) && ascii_boundary_at(bytes, cap) {
        return (cap, None);
    }

    for (_, grapheme) in text[end..].grapheme_indices(true) {
        let bytes = grapheme.len();
        if end == start && bytes > max_bytes {
            end = scalar_end(text, start, max_bytes);
            return (end, Some(bytes - (end - start)));
        }
        if end - start + bytes > max_bytes {
            return (end, Some(bytes));
        }
        end += bytes;
    }
    (end, None)
}

#[inline]
fn scalar_end(text: &str, start: usize, max_bytes: usize) -> usize {
    debug_assert!(max_bytes >= MIN_TOKEN_BYTES);
    let mut end = start.saturating_add(max_bytes).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    debug_assert!(end > start);
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Classification;
    use proptest::prelude::*;

    fn spec(mode: LongTokenMode, max_bytes: usize) -> LongTokenSpec {
        LongTokenSpec { mode, max_bytes }
    }

    fn token<'a>(text: &'a str, pos: u32) -> Token<'a> {
        Token::with_classification(text, pos, Classification::Word)
    }

    #[test]
    fn under_limit_is_zero_copy() {
        let input = "abcdef";
        let mut iter = LongTokenIter::<_, false>::new(
            [token(input, 0)].into_iter(),
            spec(LongTokenMode::Split, 8),
            PositionGapMode::Preserve,
        );
        let output = iter.next().unwrap();
        assert!(matches!(output.text, Cow::Borrowed(_)));
        assert_eq!(output.text.as_ptr(), input.as_ptr());
        assert!(!output.came_from_split());
    }

    #[test]
    fn borrowed_split_chunks_are_source_slices() {
        let input = "abécd";
        let chunks = LongTokenIter::<_, false>::new(
            [token(input, 4)].into_iter(),
            spec(LongTokenMode::Split, 4),
            PositionGapMode::Preserve,
        )
        .collect::<Vec<_>>();
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_ref())
                .collect::<Vec<_>>(),
            ["abé", "cd"]
        );
        assert_eq!(
            chunks.iter().map(|chunk| chunk.pos).collect::<Vec<_>>(),
            [4, 5]
        );
        assert!(chunks.iter().all(Token::came_from_split));
        assert_eq!(chunks[0].text.as_ptr(), input.as_ptr());
        assert_eq!(chunks[1].text.as_ptr(), input[4..].as_ptr());
    }

    #[test]
    fn split_keeps_fitting_grapheme_clusters_whole() {
        let input = "a\u{301}b\u{301}c";
        let chunks = LongTokenIter::<_, false>::new(
            [token(input, 0)].into_iter(),
            spec(LongTokenMode::Split, 4),
            PositionGapMode::Preserve,
        )
        .map(|token| token.text.into_owned())
        .collect::<Vec<_>>();
        assert_eq!(chunks, ["a\u{301}", "b\u{301}c"]);
    }

    #[test]
    fn oversized_cluster_falls_back_to_bounded_scalar_chunks() {
        let input = "👨‍👩‍👧‍👦";
        let output = LongTokenIter::<_, false>::new(
            [token(input, 0)].into_iter(),
            spec(LongTokenMode::Split, 4),
            PositionGapMode::Preserve,
        )
        .collect::<Vec<_>>();
        assert_eq!(
            output
                .iter()
                .map(|token| token.text.as_ref())
                .collect::<Vec<_>>(),
            ["👨", "\u{200d}", "👩", "\u{200d}", "👧", "\u{200d}", "👦"]
        );
        assert_eq!(
            output
                .iter()
                .map(|token| token.text.as_ref())
                .collect::<String>(),
            input
        );
        assert!(output.iter().all(|token| token.text.len() <= 4));
        assert!(output.iter().all(Token::came_from_split));
    }

    #[test]
    fn multi_kilobyte_cluster_never_exceeds_the_default_ceiling() {
        let input = format!("a{}", "\u{301}".repeat(4_000));
        let output = LongTokenIter::<_, false>::new(
            [token(&input, 0)].into_iter(),
            spec(LongTokenMode::Split, 256),
            PositionGapMode::Preserve,
        )
        .collect::<Vec<_>>();

        assert_eq!(
            output
                .iter()
                .map(|token| token.text.as_ref())
                .collect::<String>(),
            input
        );
        assert!(output.len() > 1);
        assert!(output.iter().all(|token| token.text.len() <= 256));
        assert!(output.iter().all(Token::came_from_split));
    }

    #[test]
    fn truncate_stops_before_an_oversized_cluster() {
        let output = LongTokenIter::<_, false>::new(
            [token("ab👨‍👩‍👧‍👦", 0)].into_iter(),
            spec(LongTokenMode::Truncate, 4),
            PositionGapMode::Preserve,
        )
        .collect::<Vec<_>>();
        assert_eq!(output[0].text, "ab");

        let output = LongTokenIter::<_, false>::new(
            [token("👨‍👩‍👧‍👦", 0)].into_iter(),
            spec(LongTokenMode::Truncate, 4),
            PositionGapMode::Preserve,
        )
        .collect::<Vec<_>>();
        assert!(output.is_empty());
    }

    #[test]
    fn preserve_keeps_discard_gaps_and_split_offsets() {
        let output = LongTokenIter::<_, false>::new(
            [token("ok", 0), token("abcde", 1), token("z", 2)].into_iter(),
            spec(LongTokenMode::Split, 4),
            PositionGapMode::Preserve,
        )
        .collect::<Vec<_>>();
        assert_eq!(
            output.iter().map(|token| token.pos).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );

        let output = LongTokenIter::<_, false>::new(
            [token("ok", 0), token("discard", 1), token("z", 2)].into_iter(),
            spec(LongTokenMode::Discard, 4),
            PositionGapMode::Preserve,
        )
        .collect::<Vec<_>>();
        assert_eq!(
            output.iter().map(|token| token.pos).collect::<Vec<_>>(),
            [0, 2]
        );
    }

    #[test]
    fn collapse_numbers_only_emitted_chunks() {
        let output = LongTokenIter::<_, false>::new(
            [token("abcde", 8)].into_iter(),
            spec(LongTokenMode::Split, 4),
            PositionGapMode::Collapse,
        )
        .collect::<Vec<_>>();
        assert_eq!(
            output.iter().map(|token| token.pos).collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn owned_split_allocates_owned_chunks() {
        let source = Token {
            text: Cow::Owned("abcdef".to_owned()),
            pos: 0,
            classification: Classification::Word,
            came_from_split: false,
        };
        let chunks = LongTokenIter::<_, false>::new(
            [source].into_iter(),
            spec(LongTokenMode::Split, 4),
            PositionGapMode::Preserve,
        )
        .collect::<Vec<_>>();
        assert!(
            chunks
                .iter()
                .all(|chunk| matches!(chunk.text, Cow::Owned(_)))
        );
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text.as_ref())
                .collect::<Vec<_>>(),
            ["abcd", "ef"]
        );
    }

    fn materialized_long_tokens(text: &str, mode: LongTokenMode, max: usize) -> Vec<String> {
        if text.len() <= max {
            return vec![text.to_owned()];
        }
        match mode {
            LongTokenMode::Discard => Vec::new(),
            LongTokenMode::Truncate => {
                let mut output = String::new();
                for grapheme in text.graphemes(true) {
                    if output.len() + grapheme.len() > max {
                        break;
                    }
                    output.push_str(grapheme);
                }
                (!output.is_empty()).then_some(output).into_iter().collect()
            }
            LongTokenMode::Split => {
                let mut chunks = Vec::new();
                let mut chunk = String::new();
                for grapheme in text.graphemes(true) {
                    if grapheme.len() > max {
                        if !chunk.is_empty() {
                            chunks.push(std::mem::take(&mut chunk));
                        }
                        for scalar in grapheme.chars() {
                            if !chunk.is_empty() && chunk.len() + scalar.len_utf8() > max {
                                chunks.push(std::mem::take(&mut chunk));
                            }
                            chunk.push(scalar);
                        }
                    } else {
                        if !chunk.is_empty() && chunk.len() + grapheme.len() > max {
                            chunks.push(std::mem::take(&mut chunk));
                        }
                        chunk.push_str(grapheme);
                    }
                }
                if !chunk.is_empty() {
                    chunks.push(chunk);
                }
                chunks
            }
        }
    }

    fn nonempty_unicode() -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 1..80).prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        #[test]
        fn every_long_token_mode_matches_a_materialized_grapheme_oracle(
            text in nonempty_unicode(),
            mode_index in 0u8..3,
            max in MIN_TOKEN_BYTES..33,
        ) {
            let mode = match mode_index {
                0 => LongTokenMode::Truncate,
                1 => LongTokenMode::Discard,
                _ => LongTokenMode::Split,
            };
            let expected = materialized_long_tokens(&text, mode, max);
            let actual = LongTokenIter::<_, false>::new(
                [token(&text, 7)].into_iter(),
                spec(mode, max),
                PositionGapMode::Preserve,
            )
            .collect::<Vec<_>>();

            prop_assert_eq!(
                actual.iter().map(|token| token.text.as_ref()).collect::<Vec<_>>(),
                expected.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            prop_assert!(actual.iter().all(|token| matches!(token.text, Cow::Borrowed(_))));
            prop_assert_eq!(
                actual.iter().map(|token| token.pos).collect::<Vec<_>>(),
                (7..7 + actual.len() as u32).collect::<Vec<_>>(),
            );
            if mode == LongTokenMode::Split {
                prop_assert_eq!(
                    actual.iter().map(|token| token.text.as_ref()).collect::<String>(),
                    text.as_str(),
                );
                prop_assert!(actual.iter().all(|token| token.text.len() <= max));
                let was_split = actual.len() > 1;
                prop_assert!(actual.iter().all(|token| token.came_from_split() == was_split));
            }
        }
    }
}
