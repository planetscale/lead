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
//! Source-span view of a compiled pipeline.
//!
//! Highlighting maps match positions — computed against the pipeline's
//! emitted tokens — back onto the original text. The emitted tokens cannot
//! serve that mapping directly because folding rewrites their bytes, and a
//! fold-disabled twin pipeline drifts: the pipeline applies its long-token
//! policy to the *folded* byte length, so a token whose fold crosses the
//! byte limit chunks differently than its source does, shifting every later
//! position.
//!
//! [`SourceSpanIter`] resolves that by walking the same base tokenizer and
//! folding each token only to *drive policy decisions*, while always emitting
//! borrowed slices of the source. Its contract: one token per position the
//! pipeline consumes, in position order, with `pos` equal to that position —
//! under `position_gaps = preserve` that includes a whole-token placeholder
//! for each gap position (folded-to-empty, discarded, or truncated-to-empty
//! tokens), and a split token yields exactly as many slices as the pipeline
//! emits chunks, cut at source grapheme boundaries mapped through folding.

use crate::folder::CompiledFolder;
use crate::long_tokens::{split_end, truncate_end};
use crate::spec::{
    GraphemeMode, LongTokenMode, LongTokenSpec, PositionGapMode, TokenizerPipelineSpec,
    TokenizerSpec,
};
use crate::tokenizers::{
    DiscardGraphemes, EmojiGraphemes, RetainGraphemes, UnicodeIter, WhitespaceIter,
};
use crate::{CompiledTokenizerPipeline, Token, Tokenizer};
use std::borrow::Cow;
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;

/// Position-aligned source-span view of a [`CompiledTokenizerPipeline`].
pub struct SourceSpans<'pipeline>(&'pipeline CompiledTokenizerPipeline);

impl CompiledTokenizerPipeline {
    /// A [`Tokenizer`] view of this pipeline that always yields borrowed
    /// slices of the input, position-aligned with the pipeline's emission
    /// (see the module docs for the exact contract).
    pub fn source_spans(&self) -> SourceSpans<'_> {
        SourceSpans(self)
    }
}

impl Tokenizer for SourceSpans<'_> {
    type Iter<'tokenizer, 'text>
        = SourceSpanIter<'text>
    where
        Self: 'tokenizer;

    fn tokenize<'tokenizer, 'text>(
        &'tokenizer self,
        text: &'text str,
    ) -> Self::Iter<'tokenizer, 'text> {
        SourceSpanIter::new(*self.0.spec(), text)
    }
}

enum BaseIter<'text> {
    UnicodeDiscard(UnicodeIter<'text, DiscardGraphemes>),
    UnicodeEmoji(UnicodeIter<'text, EmojiGraphemes>),
    UnicodeRetain(UnicodeIter<'text, RetainGraphemes>),
    Whitespace(WhitespaceIter<'text>),
}

impl<'text> Iterator for BaseIter<'text> {
    type Item = Token<'text>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::UnicodeDiscard(iter) => iter.next(),
            Self::UnicodeEmoji(iter) => iter.next(),
            Self::UnicodeRetain(iter) => iter.next(),
            Self::Whitespace(iter) => iter.next(),
        }
    }
}

pub struct SourceSpanIter<'text> {
    base: BaseIter<'text>,
    folder: CompiledFolder,
    long_tokens: LongTokenSpec,
    preserve_gaps: bool,
    pending: VecDeque<Token<'text>>,
    next_pos: u32,
}

impl<'text> SourceSpanIter<'text> {
    fn new(spec: TokenizerPipelineSpec, text: &'text str) -> Self {
        let base = match spec.tokenizer {
            TokenizerSpec::Unicode => match spec.graphemes {
                GraphemeMode::Discard => BaseIter::UnicodeDiscard(UnicodeIter::new(text)),
                GraphemeMode::Emoji => BaseIter::UnicodeEmoji(UnicodeIter::new(text)),
                GraphemeMode::Retain => BaseIter::UnicodeRetain(UnicodeIter::new(text)),
            },
            TokenizerSpec::Whitespace => BaseIter::Whitespace(WhitespaceIter::new(text)),
        };
        Self {
            base,
            folder: CompiledFolder::new(spec.case_folding, spec.accent_folding),
            long_tokens: spec.long_tokens,
            preserve_gaps: spec.position_gaps == PositionGapMode::Preserve,
            pending: VecDeque::new(),
            next_pos: 0,
        }
    }

    fn emit(&mut self, mut token: Token<'text>) -> Token<'text> {
        token.pos = self.next_pos;
        self.next_pos = self
            .next_pos
            .checked_add(1)
            .expect("token position overflow");
        token
    }

    /// The folded byte length of one source slice, without keeping the fold.
    fn folded_len(&self, source: &str) -> usize {
        let mut probe = Token::new(source, 0);
        self.folder.apply(&mut probe);
        probe.text.len()
    }

    /// Queue one source slice per chunk the pipeline emits for this token.
    /// Chunk-end targets come from running the production splitter over the
    /// actually-folded text, so the slot count matches the pipeline exactly.
    /// Chunk `k` then spans from the source cluster containing its first
    /// folded byte to the first cluster boundary whose folded prefix reaches
    /// its end target. When every folded chunk edge lands on the image of a
    /// source boundary — the ordinary case — the spans are disjoint and
    /// contiguous; when folding fissions one source cluster into several
    /// folded clusters (accent folding stripping an Indic conjunct's linker,
    /// for example), the chunks sharing that cluster get overlapping spans,
    /// which the renderer's range merging already handles.
    fn queue_split_chunks(&mut self, token: &Token<'text>, source: &'text str, folded: &str) {
        let max_bytes = self.long_tokens.max_bytes;
        let mut targets = Vec::new();
        let mut start = 0;
        let mut carry = None;
        while start < folded.len() {
            let (end, next_grapheme_bytes) = split_end(folded, start, max_bytes, carry);
            targets.push(end);
            carry = next_grapheme_bytes;
            start = end;
        }

        let split = targets.len() > 1;
        let chunk = |text: &'text str| {
            let mut chunk = Token::with_classification(text, 0, token.classification);
            if split {
                chunk.mark_split();
            }
            chunk
        };

        // Source cluster boundaries with the folded byte length of everything
        // before them, closed by the end-of-token boundary.
        let mut folded_prefix = 0usize;
        let mut boundaries = Vec::new();
        for (offset, grapheme) in source.grapheme_indices(true) {
            boundaries.push((offset, folded_prefix));
            folded_prefix += self.folded_len(grapheme);
        }
        boundaries.push((source.len(), folded_prefix));

        let mut cursor = 0usize;
        let mut span_start = 0usize;
        for (index, &target) in targets.iter().enumerate() {
            if index + 1 == targets.len() {
                self.pending.push_back(chunk(&source[span_start..]));
                break;
            }
            while cursor < boundaries.len() && boundaries[cursor].1 < target {
                cursor += 1;
            }
            let (end, end_prefix) = boundaries
                .get(cursor)
                .copied()
                .unwrap_or((source.len(), folded_prefix));
            self.pending.push_back(chunk(&source[span_start..end]));
            // The next chunk starts at this boundary on a clean edge, and at
            // the start of the straddled cluster when the edge fell inside it.
            span_start = if end_prefix == target || cursor == 0 {
                end
            } else {
                boundaries[cursor - 1].0
            };
        }
    }
}

impl<'text> Iterator for SourceSpanIter<'text> {
    type Item = Token<'text>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(slot) = self.pending.pop_front() {
                return Some(self.emit(slot));
            }

            let token = self.base.next()?;
            let source = match &token.text {
                Cow::Borrowed(source) => *source,
                Cow::Owned(_) => unreachable!("base tokenizers yield borrowed source slices"),
            };

            let mut folded = Token::with_classification(source, 0, token.classification);
            self.folder.apply(&mut folded);
            let consumes_gap_position = if folded.text.is_empty() {
                true
            } else if folded.text.len() <= self.long_tokens.max_bytes {
                return Some(self.emit(token));
            } else {
                match self.long_tokens.mode {
                    LongTokenMode::Discard => true,
                    LongTokenMode::Truncate => {
                        if truncate_end(&folded.text, self.long_tokens.max_bytes) == 0 {
                            true
                        } else {
                            // The pipeline emits the folded prefix; the whole
                            // source token is its span.
                            return Some(self.emit(token));
                        }
                    }
                    LongTokenMode::Split => {
                        self.queue_split_chunks(&token, source, &folded.text);
                        continue;
                    }
                }
            };

            if consumes_gap_position && self.preserve_gaps {
                // The pipeline drops this token but its position remains a
                // gap; a placeholder keeps later slots position-aligned. Gap
                // positions never appear in match results, so the slice is
                // never rendered as a match.
                return Some(self.emit(token));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Classification, Folding};

    fn collect_spans(spec: TokenizerPipelineSpec, text: &str) -> Vec<(String, u32, bool)> {
        let pipeline = spec.compile().expect("spec should compile");
        pipeline
            .source_spans()
            .tokenize(text)
            .map(|token| {
                assert!(
                    matches!(token.text, Cow::Borrowed(_)),
                    "source spans must borrow from the input"
                );
                let split = token.came_from_split();
                (token.text.into_owned(), token.pos, split)
            })
            .collect()
    }

    fn pipeline_positions(spec: TokenizerPipelineSpec, text: &str) -> Vec<(String, u32)> {
        let pipeline = spec.compile().expect("spec should compile");
        pipeline
            .tokenize(text)
            .map(|token| (token.text.into_owned(), token.pos))
            .collect()
    }

    #[test]
    fn fold_that_shrinks_under_the_limit_keeps_one_slot() {
        // 130 x U+0130 = 260 source bytes, but the default fold contracts the
        // run to 130 bytes, so the pipeline never splits it. A fold-blind
        // source splitter would emit two chunks and shift `tail` by one.
        let run = "İ".repeat(130);
        let text = format!("{run} tail");
        let spec = TokenizerPipelineSpec::tin_default();

        assert_eq!(
            pipeline_positions(spec, &text),
            vec![("i".repeat(130), 0), ("tail".to_owned(), 1)]
        );
        assert_eq!(
            collect_spans(spec, &text),
            vec![(run, 0, false), ("tail".to_owned(), 1, false)]
        );
    }

    #[test]
    fn fold_that_grows_past_the_limit_splits_the_source_in_step() {
        // 100 x U+023A = 200 source bytes; the fold ("ⱥ", 3 bytes each) is
        // 300 bytes, so the pipeline splits it 85/15. The source spans must
        // cut at the same grapheme counts so `tail` stays at position 2.
        let run = "Ⱥ".repeat(100);
        let text = format!("{run} tail");
        let spec = TokenizerPipelineSpec::tin_default();

        assert_eq!(
            pipeline_positions(spec, &text),
            vec![
                ("ⱥ".repeat(85), 0),
                ("ⱥ".repeat(15), 1),
                ("tail".to_owned(), 2),
            ]
        );
        assert_eq!(
            collect_spans(spec, &text),
            vec![
                ("Ⱥ".repeat(85), 0, true),
                ("Ⱥ".repeat(15), 1, true),
                ("tail".to_owned(), 2, false),
            ]
        );
    }

    #[test]
    fn folded_empty_token_holds_its_gap_position_under_preserve() {
        // A combining-mark-only word survives UAX segmentation as a word
        // token, then accent folding erases it: the pipeline leaves a gap at
        // its position, and the span stream must hold a placeholder slot
        // there so `omega` stays at position 2.
        const MARKS: &str = "\u{0351}\u{034c}\u{0369}\u{0314}\u{0357}\u{0305}";
        let text = format!("alpha {MARKS} omega");
        let spec = TokenizerPipelineSpec::tin_default();

        assert_eq!(
            pipeline_positions(spec, &text),
            vec![("alpha".to_owned(), 0), ("omega".to_owned(), 2)]
        );
        assert_eq!(
            collect_spans(spec, &text),
            vec![
                ("alpha".to_owned(), 0, false),
                (MARKS.to_owned(), 1, false),
                ("omega".to_owned(), 2, false),
            ]
        );
    }

    #[test]
    fn collapse_mode_skips_dropped_tokens_entirely() {
        let mut spec = TokenizerPipelineSpec::legacy_tin_default();
        spec.long_tokens.max_bytes = 4;
        spec.long_tokens.mode = LongTokenMode::Discard;

        let text = "abc toolong bb";
        assert_eq!(
            pipeline_positions(spec, text),
            vec![("abc".to_owned(), 0), ("bb".to_owned(), 1)]
        );
        assert_eq!(
            collect_spans(spec, text),
            vec![("abc".to_owned(), 0, false), ("bb".to_owned(), 1, false)]
        );
    }

    #[test]
    fn truncated_token_spans_its_whole_source_word() {
        let mut spec = TokenizerPipelineSpec::tin_default();
        spec.case_folding = Folding::Preserve;
        spec.accent_folding = Folding::Preserve;
        spec.long_tokens.mode = LongTokenMode::Truncate;
        spec.long_tokens.max_bytes = 4;

        assert_eq!(
            pipeline_positions(spec, "abcdef gg"),
            vec![("abcd".to_owned(), 0), ("gg".to_owned(), 1)]
        );
        assert_eq!(
            collect_spans(spec, "abcdef gg"),
            vec![("abcdef".to_owned(), 0, false), ("gg".to_owned(), 1, false)]
        );
    }

    #[test]
    fn whitespace_tokenizer_spans_follow_the_same_contract() {
        let mut spec = TokenizerPipelineSpec::tin_default();
        spec.tokenizer = TokenizerSpec::Whitespace;
        spec.case_folding = Folding::Preserve;
        spec.accent_folding = Folding::Preserve;
        spec.long_tokens.max_bytes = 4;

        assert_eq!(
            collect_spans(spec, "abcde f"),
            vec![
                ("abcd".to_owned(), 0, true),
                ("e".to_owned(), 1, true),
                ("f".to_owned(), 2, false),
            ]
        );
    }

    #[test]
    fn split_spans_keep_classification() {
        let spec = TokenizerPipelineSpec::tin_default();
        let pipeline = spec.compile().expect("spec should compile");
        let token = pipeline
            .source_spans()
            .tokenize("word")
            .next()
            .expect("one token");
        assert_eq!(token.classification, Classification::Word);
    }
}
