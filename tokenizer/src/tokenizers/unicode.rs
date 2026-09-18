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
use crate::{Classification, Token};
use std::marker::PhantomData;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};
use unicode_segmentation::{GraphemeIndices, UnicodeSegmentation, UnicodeWords};

pub(crate) trait GraphemePolicy {
    const SCAN_GRAPHEMES: bool;
    const SCAN_ASCII_GAPS: bool;

    fn classification(grapheme: &str) -> Option<Classification>;
}

pub(crate) struct DiscardGraphemes;
pub(crate) struct EmojiGraphemes;
pub(crate) struct RetainGraphemes;

// unicode-segmentation can attach delimiter whitespace to a rare
// combining-mark-only word boundary. Keep the common word path to one byte
// check and trim only that upstream edge case.
#[inline]
fn has_leading_whitespace(segment: &str) -> bool {
    let Some(first) = segment.as_bytes().first() else {
        return false;
    };
    if first.is_ascii_whitespace() {
        return true;
    }
    !first.is_ascii() && segment.chars().next().is_some_and(char::is_whitespace)
}

#[cold]
fn cold_path() {}

#[inline]
fn unlikely(condition: bool) -> bool {
    if condition {
        cold_path();
    }
    condition
}

impl GraphemePolicy for DiscardGraphemes {
    const SCAN_GRAPHEMES: bool = false;
    const SCAN_ASCII_GAPS: bool = false;

    #[inline]
    fn classification(_grapheme: &str) -> Option<Classification> {
        None
    }
}

impl GraphemePolicy for EmojiGraphemes {
    const SCAN_GRAPHEMES: bool = true;
    const SCAN_ASCII_GAPS: bool = false;

    #[inline]
    fn classification(grapheme: &str) -> Option<Classification> {
        emojis::get(grapheme).map(|_| Classification::Emoji)
    }
}

impl GraphemePolicy for RetainGraphemes {
    const SCAN_GRAPHEMES: bool = true;
    const SCAN_ASCII_GAPS: bool = true;

    #[inline]
    fn classification(grapheme: &str) -> Option<Classification> {
        if emojis::get(grapheme).is_some() {
            return Some(Classification::Emoji);
        }

        let mut has_mark = false;
        let mut has_symbol = false;
        for c in grapheme.chars() {
            if matches!(c, '\u{200d}' | '\u{fe0e}' | '\u{fe0f}') {
                continue;
            }
            match c.general_category_group() {
                GeneralCategoryGroup::Symbol => has_symbol = true,
                GeneralCategoryGroup::Mark => has_mark = true,
                GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number => {}
                GeneralCategoryGroup::Punctuation
                | GeneralCategoryGroup::Separator
                | GeneralCategoryGroup::Other => return None,
            }
        }
        if has_symbol {
            Some(Classification::Symbol)
        } else {
            has_mark.then_some(Classification::Grapheme)
        }
    }
}

pub(crate) struct UnicodeIter<'a, P> {
    text: &'a str,
    words: UnicodeWords<'a>,
    graphemes: Option<GraphemeIndices<'a>>,
    pending_word: Option<PendingWord<'a>>,
    cursor: usize,
    pos: u32,
    policy: PhantomData<P>,
}

struct PendingWord<'a> {
    text: &'a str,
    raw_end: usize,
    classification: Classification,
}

impl<'a, P> UnicodeIter<'a, P> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self {
            text,
            words: text.unicode_words(),
            graphemes: None,
            pending_word: None,
            cursor: 0,
            pos: 0,
            policy: PhantomData,
        }
    }

    /// Positions handed out so far, including tokens a later stage drops
    /// (e.g. a combining-mark run that folds to empty keeps its gap
    /// position). Final once the iterator returns `None`.
    pub(crate) fn positions_consumed(&self) -> u32 {
        self.pos
    }

    #[inline]
    fn emit(&mut self, text: &'a str, classification: Classification) -> Token<'a> {
        let token = Token::with_classification(text, self.pos, classification);
        self.pos = self.pos.checked_add(1).expect("token position overflow");
        token
    }
}

impl<'a, P: GraphemePolicy> Iterator for UnicodeIter<'a, P> {
    type Item = Token<'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(graphemes) = &mut self.graphemes {
                for (_, grapheme) in graphemes.by_ref() {
                    let grapheme = if unlikely(has_leading_whitespace(grapheme)) {
                        grapheme.trim_matches(char::is_whitespace)
                    } else {
                        grapheme
                    };
                    if grapheme.is_empty() {
                        continue;
                    }
                    if let Some(classification) = P::classification(grapheme) {
                        return Some(self.emit(grapheme, classification));
                    }
                }
                self.graphemes = None;
            }

            if let Some(word) = self.pending_word.take() {
                self.cursor = word.raw_end;
                return Some(self.emit(word.text, word.classification));
            }

            let Some(raw_word) = self.words.next() else {
                if P::SCAN_GRAPHEMES && self.cursor < self.text.len() {
                    let suffix = &self.text[self.cursor..];
                    self.cursor = self.text.len();
                    if P::SCAN_ASCII_GAPS || !suffix.is_ascii() {
                        self.graphemes = Some(suffix.grapheme_indices(true));
                    }
                    continue;
                }
                return None;
            };
            let raw_start = raw_word.as_ptr() as usize - self.text.as_ptr() as usize;
            let raw_end = raw_start + raw_word.len();
            let word = if unlikely(has_leading_whitespace(raw_word)) {
                raw_word.trim_matches(char::is_whitespace)
            } else {
                raw_word
            };
            if word.is_empty() {
                self.cursor = raw_end;
                continue;
            }
            let classification = if P::SCAN_GRAPHEMES
                && word.as_bytes().first().is_some_and(u8::is_ascii_digit)
                && !word.is_ascii()
                && emojis::get(word).is_some()
            {
                Classification::Emoji
            } else {
                Classification::Word
            };
            let word_start = word.as_ptr() as usize - self.text.as_ptr() as usize;

            if P::SCAN_GRAPHEMES && self.cursor < word_start {
                let gap = &self.text[self.cursor..word_start];
                if P::SCAN_ASCII_GAPS || !gap.is_ascii() {
                    self.pending_word = Some(PendingWord {
                        text: word,
                        raw_end,
                        classification,
                    });
                    self.graphemes = Some(gap.grapheme_indices(true));
                    continue;
                }
            }

            self.cursor = raw_end;
            return Some(self.emit(word, classification));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::borrow::Cow;

    fn collect<P: GraphemePolicy>(text: &str) -> Vec<(String, Classification, u32, bool)> {
        UnicodeIter::<P>::new(text)
            .map(|token| {
                let borrowed = matches!(token.text, Cow::Borrowed(_));
                (
                    token.text.into_owned(),
                    token.classification,
                    token.pos,
                    borrowed,
                )
            })
            .collect()
    }

    #[test]
    fn words_follow_uax_boundaries() {
        assert_eq!(
            collect::<DiscardGraphemes>("can't 29.3 foo_bar 東京 مرحبا"),
            vec![
                ("can't".into(), Classification::Word, 0, true),
                ("29.3".into(), Classification::Word, 1, true),
                ("foo_bar".into(), Classification::Word, 2, true),
                ("東".into(), Classification::Word, 3, true),
                ("京".into(), Classification::Word, 4, true),
                ("مرحبا".into(), Classification::Word, 5, true),
            ]
        );
    }

    #[test]
    fn policies_form_the_expected_subsets() {
        let text = "alpha 😀 + ! \u{0301} omega";
        assert_eq!(
            collect::<DiscardGraphemes>(text),
            vec![
                ("alpha".into(), Classification::Word, 0, true),
                ("omega".into(), Classification::Word, 1, true),
            ]
        );
        assert_eq!(
            collect::<EmojiGraphemes>(text),
            vec![
                ("alpha".into(), Classification::Word, 0, true),
                ("😀".into(), Classification::Emoji, 1, true),
                ("omega".into(), Classification::Word, 2, true),
            ]
        );
        assert_eq!(
            collect::<RetainGraphemes>(text),
            vec![
                ("alpha".into(), Classification::Word, 0, true),
                ("😀".into(), Classification::Emoji, 1, true),
                ("+".into(), Classification::Symbol, 2, true),
                ("\u{0301}".into(), Classification::Grapheme, 3, true),
                ("omega".into(), Classification::Word, 4, true),
            ]
        );
    }

    #[test]
    fn emoji_clusters_stay_whole() {
        let text = "👨‍👩‍👧‍👦 👍🏽 🇺🇳 1️⃣ ❤️";
        let tokens = collect::<EmojiGraphemes>(text);
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.0.as_str())
                .collect::<Vec<_>>(),
            ["👨‍👩‍👧‍👦", "👍🏽", "🇺🇳", "1️⃣", "❤️"]
        );
        assert!(tokens.iter().all(|token| token.3));
    }

    #[test]
    fn punctuation_whitespace_and_controls_are_never_tokens() {
        assert!(collect::<RetainGraphemes>(" ,.!\t\n\u{0000}\u{200d}\u{fe0f}").is_empty());
    }

    #[test]
    fn combining_only_word_boundaries_never_retain_delimiter_whitespace() {
        const MARKS: &str = "\u{0351}\u{034c}\u{0369}\u{0314}\u{0357}\u{0305}";
        assert_eq!(
            collect::<DiscardGraphemes>(&format!("alpha {MARKS} omega")),
            vec![
                ("alpha".into(), Classification::Word, 0, true),
                (MARKS.into(), Classification::Word, 1, true),
                ("omega".into(), Classification::Word, 2, true),
            ]
        );
    }

    #[test]
    fn words_cover_rtl_and_indic_conjunct_boundaries() {
        assert_eq!(
            collect::<DiscardGraphemes>("שלום क्\u{200d}ष বাংলা"),
            vec![
                ("שלום".into(), Classification::Word, 0, true),
                ("क्\u{200d}ष".into(), Classification::Word, 1, true),
                ("বাংলা".into(), Classification::Word, 2, true),
            ]
        );
    }

    #[derive(Clone, Copy)]
    enum ReferenceMode {
        Discard,
        Emoji,
        Retain,
    }

    fn reference(text: &str, mode: ReferenceMode) -> Vec<(String, Classification, u32)> {
        let mut output = Vec::new();
        for (_, segment) in text.split_word_bound_indices() {
            let segment = if segment.chars().next().is_some_and(char::is_whitespace) {
                segment.trim_matches(char::is_whitespace)
            } else {
                segment
            };
            if segment.is_empty() {
                continue;
            }
            if !matches!(mode, ReferenceMode::Discard)
                && segment.as_bytes().first().is_some_and(u8::is_ascii_digit)
                && !segment.is_ascii()
                && emojis::get(segment).is_some()
            {
                output.push((
                    segment.to_owned(),
                    Classification::Emoji,
                    output.len() as u32,
                ));
                continue;
            }
            if segment.chars().any(char::is_alphanumeric) {
                output.push((
                    segment.to_owned(),
                    Classification::Word,
                    output.len() as u32,
                ));
                continue;
            }
            if matches!(mode, ReferenceMode::Discard) {
                continue;
            }

            for grapheme in segment.trim_matches(char::is_whitespace).graphemes(true) {
                let classification = if emojis::get(grapheme).is_some() {
                    Some(Classification::Emoji)
                } else if matches!(mode, ReferenceMode::Emoji) {
                    None
                } else {
                    reference_retained_grapheme(grapheme)
                };
                if let Some(classification) = classification {
                    output.push((grapheme.to_owned(), classification, output.len() as u32));
                }
            }
        }
        output
    }

    fn reference_retained_grapheme(grapheme: &str) -> Option<Classification> {
        let mut mark = false;
        let mut symbol = false;
        for c in grapheme.chars() {
            if matches!(c, '\u{200d}' | '\u{fe0e}' | '\u{fe0f}') {
                continue;
            }
            match c.general_category_group() {
                GeneralCategoryGroup::Symbol => symbol = true,
                GeneralCategoryGroup::Mark => mark = true,
                GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number => {}
                GeneralCategoryGroup::Punctuation
                | GeneralCategoryGroup::Separator
                | GeneralCategoryGroup::Other => return None,
            }
        }
        if symbol {
            Some(Classification::Symbol)
        } else {
            mark.then_some(Classification::Grapheme)
        }
    }

    fn production<P: GraphemePolicy>(text: &str) -> Vec<(String, Classification, u32)> {
        UnicodeIter::<P>::new(text)
            .map(|token| (token.text.into_owned(), token.classification, token.pos))
            .collect()
    }

    fn assert_reference_parity(text: &str) {
        assert_eq!(
            production::<DiscardGraphemes>(text),
            reference(text, ReferenceMode::Discard)
        );
        assert_eq!(
            production::<EmojiGraphemes>(text),
            reference(text, ReferenceMode::Emoji)
        );
        assert_eq!(
            production::<RetainGraphemes>(text),
            reference(text, ReferenceMode::Retain)
        );
    }

    fn targeted_unicode() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                "[A-Za-z0-9_]{0,12}",
                Just("can't".to_owned()),
                Just("29.3".to_owned()),
                Just("東京".to_owned()),
                Just("مرحبا".to_owned()),
                Just("क्\u{200d}ष".to_owned()),
                Just("a\u{0301}".to_owned()),
                Just("\u{0301}\u{0327}".to_owned()),
                Just("😀".to_owned()),
                Just("👨‍👩‍👧‍👦".to_owned()),
                Just("👍🏽".to_owned()),
                Just("🇺🇳".to_owned()),
                Just("1️⃣".to_owned()),
                Just(" +—©™!\t\n".to_owned()),
                Just("\u{0000}\u{200d}\u{fe0f}".to_owned()),
            ],
            0..40,
        )
        .prop_map(|parts| parts.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        #[test]
        fn arbitrary_unicode_matches_materialized_reference(text in any::<String>()) {
            assert_reference_parity(&text);
        }

        #[test]
        fn targeted_unicode_matches_materialized_reference(text in targeted_unicode()) {
            assert_reference_parity(&text);
        }
    }
}
