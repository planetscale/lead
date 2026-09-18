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
use crate::folder::{CompiledFolder, lowercase_ascii_from};
use crate::long_tokens::LongTokenIter;
use crate::spec::{GraphemeMode, TokenizerPipelineSpec, TokenizerSpec};
use crate::tokenizers::{
    DiscardGraphemes, EmojiGraphemes, RetainGraphemes, UnicodeIter, WhitespaceIter,
};
use crate::{Classification, Token, Tokenizer};
use unicode_normalization::char::is_combining_mark;
use unicode_segmentation::UnicodeSegmentation;

pub struct CompiledTokenizerPipeline {
    spec: TokenizerPipelineSpec,
    kind: CompiledPipelineKind,
}

impl CompiledTokenizerPipeline {
    pub(crate) fn from_validated_spec(spec: TokenizerPipelineSpec) -> Self {
        let kind = if spec == TokenizerPipelineSpec::tin_default() {
            CompiledPipelineKind::TinDefault
        } else {
            let stages = CompiledStages {
                folder: CompiledFolder::new(spec.case_folding, spec.accent_folding),
                long_tokens: spec.long_tokens,
                position_gaps: spec.position_gaps,
            };
            match spec.tokenizer {
                TokenizerSpec::Unicode => match spec.graphemes {
                    GraphemeMode::Discard => CompiledPipelineKind::UnicodeDiscard(stages),
                    GraphemeMode::Emoji => CompiledPipelineKind::UnicodeEmoji(stages),
                    GraphemeMode::Retain => CompiledPipelineKind::UnicodeRetain(stages),
                },
                TokenizerSpec::Whitespace => CompiledPipelineKind::Whitespace(stages),
            }
        };
        Self { spec, kind }
    }

    pub fn spec(&self) -> &TokenizerPipelineSpec {
        &self.spec
    }
}

impl Tokenizer for CompiledTokenizerPipeline {
    type Iter<'tokenizer, 'text>
        = CompiledTokenIter<'text>
    where
        Self: 'tokenizer;

    fn tokenize<'tokenizer, 'text>(
        &'tokenizer self,
        text: &'text str,
    ) -> Self::Iter<'tokenizer, 'text> {
        CompiledTokenIter::new(&self.kind, text)
    }
}

#[derive(Clone, Copy)]
struct CompiledStages {
    folder: CompiledFolder,
    long_tokens: crate::LongTokenSpec,
    position_gaps: crate::PositionGapMode,
}

enum CompiledPipelineKind {
    TinDefault,
    UnicodeDiscard(CompiledStages),
    UnicodeEmoji(CompiledStages),
    UnicodeRetain(CompiledStages),
    Whitespace(CompiledStages),
}

struct FolderIter<I, const DEFAULT: bool> {
    inner: I,
    folder: CompiledFolder,
}

impl<I, const DEFAULT: bool> FolderIter<I, DEFAULT> {
    fn new(inner: I, folder: CompiledFolder) -> Self {
        Self { inner, folder }
    }

    fn inner(&self) -> &I {
        &self.inner
    }
}

impl<'text, I, const DEFAULT: bool> Iterator for FolderIter<I, DEFAULT>
where
    I: Iterator<Item = Token<'text>>,
{
    type Item = Token<'text>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let mut token = self.inner.next()?;
            if DEFAULT {
                CompiledFolder::apply_case_and_accent(&mut token);
            } else {
                self.folder.apply(&mut token);
            }
            if !token.text.is_empty() {
                return Some(token);
            }
        }
    }
}

struct PipelineIter<'text, I, const DEFAULT: bool>
where
    I: Iterator<Item = Token<'text>>,
{
    inner: LongTokenIter<'text, FolderIter<I, DEFAULT>, DEFAULT>,
}

impl<'text, I, const DEFAULT: bool> PipelineIter<'text, I, DEFAULT>
where
    I: Iterator<Item = Token<'text>>,
{
    fn new(inner: I, stages: CompiledStages) -> Self {
        Self {
            inner: LongTokenIter::new(
                FolderIter::new(inner, stages.folder),
                stages.long_tokens,
                stages.position_gaps,
            ),
        }
    }
}

impl<'text> PipelineIter<'text, UnicodeIter<'text, EmojiGraphemes>, true> {
    /// Positions the exhausted pipeline consumed from its input: the
    /// tokenizer's position counter (which already counts tokens the folder
    /// later drops, so fold-to-empty gaps are included) plus one extra
    /// position per 256-byte split continuation. Only meaningful after the
    /// iterator returns `None`.
    fn positions_consumed(&self) -> u32 {
        self.inner
            .inner()
            .inner()
            .positions_consumed()
            .checked_add(self.inner.split_position_offset())
            .expect("token position overflow")
    }
}

impl<'text, I, const DEFAULT: bool> Iterator for PipelineIter<'text, I, DEFAULT>
where
    I: Iterator<Item = Token<'text>>,
{
    type Item = Token<'text>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

pub struct CompiledTokenIter<'text> {
    inner: CompiledTokenIterKind<'text>,
}

impl<'text> CompiledTokenIter<'text> {
    fn new(kind: &CompiledPipelineKind, text: &'text str) -> Self {
        let inner = match *kind {
            CompiledPipelineKind::TinDefault => {
                if text.is_ascii() {
                    CompiledTokenIterKind::TinDefaultAscii(AsciiDefaultIter::new(text))
                } else {
                    CompiledTokenIterKind::TinDefaultMixed(MixedDefaultIter::new(text))
                }
            }
            CompiledPipelineKind::UnicodeDiscard(stages) => CompiledTokenIterKind::UnicodeDiscard(
                PipelineIter::new(UnicodeIter::<DiscardGraphemes>::new(text), stages),
            ),
            CompiledPipelineKind::UnicodeEmoji(stages) => CompiledTokenIterKind::UnicodeEmoji(
                PipelineIter::new(UnicodeIter::<EmojiGraphemes>::new(text), stages),
            ),
            CompiledPipelineKind::UnicodeRetain(stages) => CompiledTokenIterKind::UnicodeRetain(
                PipelineIter::new(UnicodeIter::<RetainGraphemes>::new(text), stages),
            ),
            CompiledPipelineKind::Whitespace(stages) => CompiledTokenIterKind::Whitespace(
                PipelineIter::new(WhitespaceIter::new(text), stages),
            ),
        };
        Self { inner }
    }
}

const fn default_stages() -> CompiledStages {
    CompiledStages {
        folder: CompiledFolder::CaseAndAccent,
        long_tokens: crate::LongTokenSpec {
            mode: crate::LongTokenMode::Split,
            max_bytes: 256,
        },
        position_gaps: crate::PositionGapMode::Preserve,
    }
}

/// Exact ASCII specialization of the complete tin default pipeline.
///
/// ASCII has no emoji or accents, every scalar is one grapheme, and removing
/// punctuation creates no positions. That reduces the fixed default pipeline
/// to one UAX-compatible word scan, lowercase, and 256-byte chunking.
struct AsciiDefaultIter<'text> {
    text: &'text str,
    scan_pos: usize,
    word_start: usize,
    chunk_pos: usize,
    word_end: usize,
    first_uppercase: usize,
    next_position: u32,
}

impl<'text> AsciiDefaultIter<'text> {
    #[inline]
    fn new(text: &'text str) -> Self {
        Self::with_position(text, 0)
    }

    #[inline]
    fn with_position(text: &'text str, next_position: u32) -> Self {
        Self {
            text,
            scan_pos: 0,
            word_start: 0,
            chunk_pos: 0,
            word_end: 0,
            first_uppercase: usize::MAX,
            next_position,
        }
    }

    #[inline]
    fn find_word(&mut self) -> bool {
        if let Some((start, end, first_uppercase)) = find_ascii_word(self.text, &mut self.scan_pos)
        {
            self.word_start = start;
            self.chunk_pos = start;
            self.word_end = end;
            self.first_uppercase = first_uppercase;
            true
        } else {
            false
        }
    }

    #[inline]
    fn emit_chunk(&mut self) -> Token<'text> {
        let start = self.chunk_pos;
        let end = (start + 256).min(self.word_end);
        self.chunk_pos = end;

        let source = &self.text[start..end];
        let first_uppercase = if self.word_end - self.word_start <= 256 {
            if self.first_uppercase < end {
                Some(self.first_uppercase - start)
            } else {
                None
            }
        } else {
            source.as_bytes().iter().position(u8::is_ascii_uppercase)
        };
        let text = match first_uppercase {
            Some(first_uppercase) => {
                std::borrow::Cow::Owned(lowercase_ascii_from(source, first_uppercase))
            }
            None => std::borrow::Cow::Borrowed(source),
        };
        let mut token = Token {
            text,
            pos: self.next_position,
            classification: Classification::Word,
            came_from_split: false,
        };
        if self.word_end - self.word_start > 256 {
            token.mark_split();
        }
        self.next_position = self
            .next_position
            .checked_add(1)
            .expect("token position overflow");
        token
    }
}

#[inline]
fn find_ascii_word(text: &str, scan_pos: &mut usize) -> Option<(usize, usize, usize)> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut pos = *scan_pos;

    loop {
        while pos < len && !bytes[pos].is_ascii_alphanumeric() && bytes[pos] != b'_' {
            pos += 1;
        }
        if pos == len {
            *scan_pos = pos;
            return None;
        }

        let start = pos;
        let mut has_alphanumeric = false;
        let mut first_uppercase = usize::MAX;
        while pos < len {
            let byte = bytes[pos];
            if byte == b'_' {
                pos += 1;
                continue;
            }
            if byte.is_ascii_alphanumeric() {
                has_alphanumeric = true;
                if first_uppercase == usize::MAX && byte.is_ascii_uppercase() {
                    first_uppercase = pos;
                }
                pos += 1;
                continue;
            }

            if pos + 1 < len {
                let previous = bytes[pos - 1];
                let next = bytes[pos + 1];
                let bridges = match byte {
                    b':' => previous.is_ascii_alphabetic() && next.is_ascii_alphabetic(),
                    b'\'' | b'.' => {
                        (previous.is_ascii_alphabetic() && next.is_ascii_alphabetic())
                            || (previous.is_ascii_digit() && next.is_ascii_digit())
                    }
                    b',' | b';' => previous.is_ascii_digit() && next.is_ascii_digit(),
                    _ => false,
                };
                if bridges {
                    pos += 1;
                    continue;
                }
            }
            break;
        }

        *scan_pos = pos;
        if has_alphanumeric {
            return Some((start, pos, first_uppercase));
        }
    }
}

/// Streams maximal ASCII regions through the cheap word scanner and confines
/// Unicode word/grapheme work to the whitespace-delimited fields that need it.
struct MixedDefaultIter<'text> {
    text: &'text str,
    scan_pos: usize,
    pending_unicode: Option<(usize, usize)>,
    active: DefaultRegionIter<'text>,
    position_base: u32,
}

#[inline]
fn folded_token(text: &str, classification: Classification) -> Option<Token<'_>> {
    let mut token = Token::with_classification(text, 0, classification);
    CompiledFolder::apply_case_and_accent(&mut token);
    (!token.text.is_empty()).then_some(token)
}

/// Bit `i` is set iff `char::from_u32(i).is_alphabetic()` for i <= 0xFF.
/// Pinned against `char::is_alphabetic` by
/// `latin1_alphabetic_mask_matches_is_alphabetic`.
const LATIN1_ALPHABETIC: [u64; 4] = [
    0x0000000000000000,
    0x07fffffe07fffffe,
    0x0420040000000000,
    0xff7fffffff7fffff,
];

#[inline]
fn is_latin1_alphabetic(c: char) -> bool {
    let index = c as u32;
    index <= 0xff && (LATIN1_ALPHABETIC[(index >> 6) as usize] >> (index & 63)) & 1 != 0
}

#[inline]
fn is_simple_latin_word(text: &str) -> bool {
    // Latin-1 alphabetic coincides exactly with UAX word-break ALetter;
    // is_alphanumeric would also admit the superscripts and vulgar fractions,
    // whose word-break property is Other, fusing segments the general
    // pipeline breaks apart.
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || is_latin1_alphabetic(c))
}

#[inline]
fn is_basic_han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

#[inline]
fn trim_ascii_delimiter_edges(text: &str) -> &str {
    let bytes = text.as_bytes();
    let mut start = 0;
    while start < bytes.len()
        && bytes[start].is_ascii()
        && !bytes[start].is_ascii_alphanumeric()
        && bytes[start] != b'_'
    {
        start += 1;
    }

    let mut end = bytes.len();
    while end > start
        && bytes[end - 1].is_ascii()
        && !bytes[end - 1].is_ascii_alphanumeric()
        && bytes[end - 1] != b'_'
    {
        end -= 1;
    }
    &text[start..end]
}

#[inline]
fn first_non_ascii(bytes: &[u8]) -> Option<usize> {
    const HIGH_BITS: u64 = 0x8080_8080_8080_8080;
    let mut offset = 0;
    let (chunks, remainder) = bytes.as_chunks::<{ size_of::<u64>() }>();
    for chunk in chunks {
        let word = u64::from_le_bytes(*chunk);
        let high_bits = word & HIGH_BITS;
        if high_bits != 0 {
            return Some(offset + high_bits.trailing_zeros() as usize / u8::BITS as usize);
        }
        offset += size_of::<u64>();
    }
    remainder
        .iter()
        .position(|byte| !byte.is_ascii())
        .map(|pos| offset + pos)
}

enum DefaultRegionIter<'text> {
    Empty,
    Ascii(AsciiDefaultIter<'text>),
    Single(Option<Token<'text>>),
    Han {
        text: &'text str,
        chars: std::str::CharIndices<'text>,
        pos: u32,
    },
    // Boxed: the fallback pipeline dwarfs every other variant. It is built
    // once per field the cheaper tiers reject, so the allocation stays on
    // the already-slow path.
    Fallback(Box<PipelineIter<'text, UnicodeIter<'text, EmojiGraphemes>, true>>),
}

impl<'text> MixedDefaultIter<'text> {
    #[inline]
    fn new(text: &'text str) -> Self {
        Self {
            text,
            scan_pos: 0,
            pending_unicode: None,
            active: DefaultRegionIter::Empty,
            position_base: 0,
        }
    }

    #[inline]
    fn install_unicode(&mut self, field_start: usize, field_end: usize) {
        let field = &self.text[field_start..field_end];
        let core = trim_ascii_delimiter_edges(field);
        if core.len() <= 256 && is_simple_latin_word(core) {
            self.active = DefaultRegionIter::Single(folded_token(core, Classification::Word));
            return;
        }
        if core.chars().all(is_basic_han) {
            self.active = DefaultRegionIter::Han {
                text: core,
                chars: core.char_indices(),
                pos: 0,
            };
            return;
        }
        if core.len() <= 256 && emojis::get(core).is_some() {
            self.active = DefaultRegionIter::Single(folded_token(core, Classification::Emoji));
            return;
        }
        // A whitespace-delimited field made purely of combining marks
        // normally forms a single UAX#29 segment: marks are word-break
        // Extend, and any delimiter whitespace the segment absorbs is
        // trimmed back off by `UnicodeIter`. The general pipeline emits that
        // segment only when the word source's own has-alphanumeric filter
        // accepts it; the emitted token then folds to empty, leaving exactly
        // one gap position. A filtered segment consumes zero positions.
        // Both the segmentation and the emit-vs-filter decision are
        // delegated to the word source itself rather than recomputed via
        // `char::is_alphanumeric`, so this tier cannot disagree with the
        // fallback pipeline when the segmentation crate's Unicode tables lag
        // or lead the toolchain's (e.g. U+10EFA: a combining mark that is
        // alphabetic in Unicode 17 std tables but unassigned in Unicode 15.1
        // word tables). The `core.len() == field.len()` guard is essential:
        // a mark run touching trimmed ASCII punctuation (e.g. "(\u{0301})")
        // attaches to that punctuation under WB4 and segments differently,
        // so such fields must keep taking the fallback.
        if core.len() == field.len() && core.chars().all(is_combining_mark) {
            let mut words = core.unicode_words();
            self.active = match (words.next(), words.next()) {
                // The word source filters every segment: the reference emits
                // nothing and consumes zero positions. `scan_pos` is already
                // at `field_end`, so the iterator loop simply moves on to
                // the next region.
                (None, _) => DefaultRegionIter::Empty,
                // Exactly one emitted segment spanning the whole field. Run
                // the real fold rather than hardcoding the empty result:
                // all-mark text folds to empty (its NFD yields only combining
                // marks, which accent folding drops), but going through
                // `folded_token` keeps this tier honest if some exotic mark
                // ever folded non-empty. `finish_region` then advances
                // `position_base` by exactly one for `Single` — the gap. No
                // length cap is needed: a >256-byte mark run folds to empty
                // before the long-token stage would see it, still one
                // position.
                (Some(word), None) if word.len() == core.len() => {
                    DefaultRegionIter::Single(folded_token(core, Classification::Word))
                }
                // The word source disagrees with the one-whole-segment model
                // (some mark its tables do not treat as Extend split the
                // run): defer to the fallback pipeline, which matches the
                // reference by construction.
                _ => DefaultRegionIter::Fallback(Box::new(PipelineIter::new(
                    UnicodeIter::new(field),
                    default_stages(),
                ))),
            };
            return;
        }
        // Fields are maximal ASCII-whitespace-free runs, so UAX#29 word
        // segmentation is field-local here: no segment containing a word
        // character crosses ASCII whitespace, and UnicodeIter itself trims
        // the one combining-mark-led segment that could attach leading
        // whitespace. Confining the fallback to this field keeps the rest
        // of the document on the fast tiers; scan_pos already sits at
        // field_end.
        self.active = DefaultRegionIter::Fallback(Box::new(PipelineIter::new(
            UnicodeIter::new(field),
            default_stages(),
        )));
    }

    #[inline]
    fn next_region(&mut self) -> bool {
        if let Some((field_start, field_end)) = self.pending_unicode.take() {
            self.install_unicode(field_start, field_end);
            return true;
        }

        let bytes = self.text.as_bytes();
        let len = bytes.len();
        let region_start = self.scan_pos;
        if region_start == len {
            return false;
        }

        let Some(non_ascii) = first_non_ascii(&bytes[region_start..]) else {
            self.scan_pos = len;
            self.active = DefaultRegionIter::Ascii(AsciiDefaultIter::with_position(
                &self.text[region_start..],
                self.position_base,
            ));
            return true;
        };
        let non_ascii = region_start + non_ascii;

        let mut field_start = non_ascii;
        while field_start > region_start && !bytes[field_start - 1].is_ascii_whitespace() {
            field_start -= 1;
        }
        let mut field_end = non_ascii + 1;
        while field_end < len && !bytes[field_end].is_ascii_whitespace() {
            field_end += 1;
        }

        let ascii_prefix = &self.text[region_start..field_start];
        self.scan_pos = field_end;
        if ascii_prefix.is_empty() {
            self.install_unicode(field_start, field_end);
        } else {
            self.active = DefaultRegionIter::Ascii(AsciiDefaultIter::with_position(
                ascii_prefix,
                self.position_base,
            ));
            self.pending_unicode = Some((field_start, field_end));
        }
        true
    }

    #[inline]
    fn offset_position(&self, mut token: Token<'text>) -> Token<'text> {
        token.pos = token
            .pos
            .checked_add(self.position_base)
            .expect("token position overflow");
        token
    }

    #[inline]
    fn finish_region(&mut self) {
        match &self.active {
            DefaultRegionIter::Empty => {}
            DefaultRegionIter::Ascii(iter) => self.position_base = iter.next_position,
            DefaultRegionIter::Single(_) => {
                self.position_base = self
                    .position_base
                    .checked_add(1)
                    .expect("token position overflow");
            }
            DefaultRegionIter::Han { pos, .. } => {
                self.position_base = self
                    .position_base
                    .checked_add(*pos)
                    .expect("token position overflow");
            }
            DefaultRegionIter::Fallback(iter) => {
                // Advance by the positions the field consumed, not the
                // tokens it emitted: a token that folds to empty (e.g. a
                // combining-mark run) is dropped after the tokenizer already
                // assigned it a position, leaving a gap that must be kept.
                self.position_base = self
                    .position_base
                    .checked_add(iter.positions_consumed())
                    .expect("token position overflow");
            }
        }
        self.active = DefaultRegionIter::Empty;
    }
}

impl<'text> Iterator for MixedDefaultIter<'text> {
    type Item = Token<'text>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match &mut self.active {
                DefaultRegionIter::Empty => {}
                DefaultRegionIter::Ascii(iter) => {
                    if let Some(token) = iter.next() {
                        return Some(token);
                    }
                }
                DefaultRegionIter::Single(token) => {
                    if let Some(token) = token.take() {
                        return Some(self.offset_position(token));
                    }
                }
                DefaultRegionIter::Han { text, chars, pos } => {
                    if let Some((start, c)) = chars.next() {
                        let token = Token::with_classification(
                            &text[start..start + c.len_utf8()],
                            *pos,
                            Classification::Word,
                        );
                        *pos = pos.checked_add(1).expect("token position overflow");
                        return Some(self.offset_position(token));
                    }
                }
                DefaultRegionIter::Fallback(iter) => {
                    if let Some(token) = iter.next() {
                        return Some(self.offset_position(token));
                    }
                }
            }
            self.finish_region();
            if !self.next_region() {
                return None;
            }
        }
    }
}

impl<'text> Iterator for AsciiDefaultIter<'text> {
    type Item = Token<'text>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.chunk_pos < self.word_end || self.find_word() {
            Some(self.emit_chunk())
        } else {
            None
        }
    }
}

impl<'text> Iterator for CompiledTokenIter<'text> {
    type Item = Token<'text>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            CompiledTokenIterKind::TinDefaultAscii(iter) => iter.next(),
            CompiledTokenIterKind::TinDefaultMixed(iter) => iter.next(),
            CompiledTokenIterKind::UnicodeDiscard(iter) => iter.next(),
            CompiledTokenIterKind::UnicodeEmoji(iter) => iter.next(),
            CompiledTokenIterKind::UnicodeRetain(iter) => iter.next(),
            CompiledTokenIterKind::Whitespace(iter) => iter.next(),
        }
    }
}

enum CompiledTokenIterKind<'text> {
    TinDefaultAscii(AsciiDefaultIter<'text>),
    TinDefaultMixed(MixedDefaultIter<'text>),
    UnicodeDiscard(PipelineIter<'text, UnicodeIter<'text, DiscardGraphemes>, false>),
    UnicodeEmoji(PipelineIter<'text, UnicodeIter<'text, EmojiGraphemes>, false>),
    UnicodeRetain(PipelineIter<'text, UnicodeIter<'text, RetainGraphemes>, false>),
    Whitespace(PipelineIter<'text, WhitespaceIter<'text>, false>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn latin1_alphabetic_mask_matches_is_alphabetic() {
        for i in 0..=255u32 {
            let c = char::from_u32(i).expect("every Latin-1 scalar is a char");
            assert_eq!(is_latin1_alphabetic(c), c.is_alphabetic(), "U+{i:04X}");
        }
        // Characters above the table are never Latin-1 alphabetic.
        for c in ['\u{100}', 'μ', '東', '😀'] {
            assert!(!is_latin1_alphabetic(c), "{c:?}");
        }
    }

    fn token_shape(token: Token<'_>) -> (String, u32, Classification, bool) {
        let came_from_split = token.came_from_split();
        (
            token.text.into_owned(),
            token.pos,
            token.classification,
            came_from_split,
        )
    }

    fn specialized(text: &str) -> Vec<(String, u32, Classification, bool)> {
        AsciiDefaultIter::new(text).map(token_shape).collect()
    }

    fn unicode_reference(text: &str) -> Vec<(String, u32, Classification, bool)> {
        PipelineIter::<_, true>::new(UnicodeIter::<EmojiGraphemes>::new(text), default_stages())
            .map(token_shape)
            .collect()
    }

    fn mixed(text: &str) -> Vec<(String, u32, Classification, bool)> {
        MixedDefaultIter::new(text).map(token_shape).collect()
    }

    #[test]
    fn ascii_default_matches_uax_bridges_and_split_positions() {
        let text = format!(
            "can't 29.3 1,000 foo_bar ___ left:right {} tail",
            "A".repeat(300)
        );
        assert_eq!(specialized(&text), unicode_reference(&text));
    }

    #[test]
    fn mixed_fast_regions_and_fallback_suffix_match_reference() {
        let text = format!(
            "UPPER José, 東京 (😀) क़ alpha \u{0351}\u{034c} omega {}",
            "A".repeat(300)
        );
        assert_eq!(mixed(&text), unicode_reference(&text));
    }

    #[test]
    fn ascii_default_matches_reference_exhaustively_over_boundary_alphabet() {
        // Every 4-piece string over an alphabet that forces the hand-rolled
        // scanner's decisions: word characters, ExtendNumLet underscores, the
        // three mid-word bridge classes (colon, apostrophe/period, and
        // comma/semicolon), CRLF, and plain delimiters.
        let alphabet = [
            "a", "B", "0", "9", "_", "'", ".", ":", ";", ",", " ", "\r\n", "\t", "!", "-", "z",
        ];
        for case in 0..alphabet.len().pow(4) {
            let mut index = case;
            let mut text = String::new();
            for _ in 0..4 {
                text.push_str(alphabet[index % alphabet.len()]);
                index /= alphabet.len();
            }
            assert_eq!(
                specialized(&text),
                unicode_reference(&text),
                "input {text:?}"
            );
        }
    }

    #[test]
    fn mixed_long_token_boundaries_match_reference() {
        let mut cases = Vec::new();
        for run in [127usize, 128, 129, 255, 256, 257] {
            cases.push(format!("x {} y", "é".repeat(run)));
            cases.push(format!("x {} y", "a".repeat(run)));
            cases.push(format!("x {} y", "東".repeat(run)));
            cases.push(format!("x {}é y", "A".repeat(run)));
        }
        // Folding shrinks İ and grows Ⱥ across the 256-byte split limit, a
        // combining-mark run folds to empty but must keep its gap position,
        // and an emoji run must stay grapheme-whole through splitting.
        cases.push(format!("x {} y", "İ".repeat(130)));
        cases.push(format!("x {} y", "Ⱥ".repeat(100)));
        cases.push(format!("x {} y", "\u{0301}".repeat(300)));
        cases.push(format!("x {} y", "😀".repeat(90)));
        for text in &cases {
            assert_eq!(mixed(text), unicode_reference(text), "input {text:?}");
        }
    }

    #[test]
    fn mixed_degenerate_documents_match_reference() {
        for text in [
            "",
            " \t\r\n ",
            // Unicode-only whitespace islands: fields with no word content.
            "\u{00a0}\u{2003}\u{3000}",
            // NBSP is not ASCII whitespace, so it fuses two words into one
            // non-simple field that must take the fallback.
            "é\u{00a0}x",
            // VT is char::is_whitespace but not ASCII whitespace.
            "abc\x0bé",
            // Bidi controls around an island.
            "\u{202e}txet\u{202c} é",
            "\u{0000}é\u{0000}",
            // Decomposed Hangul jamo field.
            "x \u{1100}\u{1161} y",
            // Indic nukta and a ZWJ conjunct.
            "क़ x क्\u{200d}ष",
            // Mark-only field whose classification is Unicode-version
            // sensitive: U+10EFA (Arabic Extended-C) is a combining mark
            // that Unicode 17 std tables call alphabetic but Unicode 15.1
            // word tables leave unassigned. Pins that the mark-only tier
            // defers to the word source's own tables for the emit-vs-filter
            // decision instead of `char::is_alphanumeric` (CI coverage
            // failure on PR #222).
            "a \u{10efa} z",
            "a \u{10efa}\u{10efa} z",
        ] {
            assert_eq!(mixed(text), unicode_reference(text), "input {text:?}");
        }
    }

    #[test]
    fn mark_only_fields_match_reference_for_every_combining_mark() {
        // Empirically pins, for every combining mark on this exact
        // toolchain: (a) the mark-only tier agrees with the reference on
        // segmentation, (b) its emit-vs-filter decision matches the general
        // pipeline's word filter — guaranteed by construction now that the
        // tier delegates both to the word source itself, whatever Unicode
        // version its tables carry — and (c) an emitted mark-only word folds
        // to empty and consumes exactly one position. The parenthesized case
        // pins that fields with trimmed ASCII punctuation keep taking the
        // fallback.
        for c in '\0'..=char::MAX {
            if !is_combining_mark(c) {
                continue;
            }
            for text in [
                format!("a {c} z"),
                format!("a {c}{c} z"),
                format!("{c}"),
                format!("a {c}\u{0301} z"),
                format!("a ({c}) z"),
            ] {
                assert_eq!(mixed(&text), unicode_reference(&text), "input {text:?}");
            }
        }
    }

    fn targeted_mixed() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                "[A-Za-z0-9_'.:;, ]{0,12}",
                Just("can't 29.3 left:right 1,000".to_owned()),
                Just("José ÅΩ µservice ﬃn ẞ İı".to_owned()),
                Just("東京 中文 \u{20000}".to_owned()),
                Just("שלום עֲלֵיכֶם مرحبا".to_owned()),
                Just("क्\u{200d}ष क़ বাংলা".to_owned()),
                Just("\u{1100}\u{1161}\u{11a8} 한글".to_owned()),
                Just("x² 3½ a¹b ¼¾ 0¾9".to_owned()),
                Just("😀 👨‍👩‍👧‍👦 👍🏽 🇺🇳 1️⃣ ❤️".to_owned()),
                Just("a\u{0301}\u{0327} \u{0351}\u{034c}".to_owned()),
                Just(" \u{00a0}\u{2003}\t\r\n".to_owned()),
                Just("\u{0000}\u{200d}\u{fe0f}\u{202e}".to_owned()),
                Just("Ⱥ".repeat(100)),
                Just("A".repeat(300)),
            ],
            0..16,
        )
        .prop_map(|parts| parts.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        #[test]
        fn targeted_mixed_families_match_whole_document_unicode_pipeline(
            text in targeted_mixed()
        ) {
            prop_assert_eq!(mixed(&text), unicode_reference(&text));
        }

        #[test]
        fn arbitrary_ascii_default_matches_unicode_pipeline(
            text in prop::collection::vec(0u8..=127, 0..2048)
                .prop_map(|bytes| String::from_utf8(bytes).expect("ASCII is UTF-8"))
        ) {
            prop_assert_eq!(specialized(&text), unicode_reference(&text));
        }

        #[test]
        fn mixed_regions_match_whole_document_unicode_pipeline(text in any::<String>()) {
            prop_assert_eq!(mixed(&text), unicode_reference(&text));
        }
    }
}
