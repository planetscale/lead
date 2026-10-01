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
use proptest::prelude::*;
use tokenizer::{
    Classification, Folding, GraphemeMode, LongTokenMode, LongTokenSpec, PositionGapMode,
    TokenStream, Tokenizer, TokenizerPipelineSpec, TokenizerSpec,
};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, PartialEq, Eq)]
struct ReferenceToken {
    text: String,
    pos: u32,
    classification: Classification,
    split: bool,
}

fn reference_pipeline(text: &str, spec: TokenizerPipelineSpec) -> (Vec<ReferenceToken>, u32) {
    let base = match spec.tokenizer {
        TokenizerSpec::Unicode => reference_unicode(text, spec.graphemes),
        TokenizerSpec::Whitespace => text
            .split_whitespace()
            .enumerate()
            .map(|(pos, text)| ReferenceToken {
                text: text.to_owned(),
                pos: pos as u32,
                classification: Classification::Unknown,
                split: false,
            })
            .collect(),
    };

    let base_count = base.len() as u32;
    let mut output = Vec::new();
    let mut split_offset = 0u32;
    for mut token in base {
        token.text = reference_fold(&token.text, spec.case_folding, spec.accent_folding);
        if token.text.is_empty() {
            continue;
        }
        let chunks = reference_long_token(&token.text, spec.long_tokens);
        let was_split = spec.long_tokens.mode == LongTokenMode::Split && chunks.len() > 1;
        let chunk_count = chunks.len();
        for (continuation, chunk) in chunks.into_iter().enumerate() {
            let pos = match spec.position_gaps {
                PositionGapMode::Collapse => output.len() as u32,
                PositionGapMode::Preserve => token.pos + split_offset + continuation as u32,
            };
            output.push(ReferenceToken {
                text: chunk,
                pos,
                classification: token.classification,
                split: was_split,
            });
        }
        if was_split {
            split_offset += chunk_count as u32 - 1;
        }
    }
    let positions_consumed = match spec.position_gaps {
        PositionGapMode::Collapse => output.len() as u32,
        PositionGapMode::Preserve => base_count + split_offset,
    };
    (output, positions_consumed)
}

fn reference_unicode(text: &str, graphemes: GraphemeMode) -> Vec<ReferenceToken> {
    let mut output = Vec::new();
    for (_, segment) in text.split_word_bound_indices() {
        // unicode-segmentation can attach delimiter whitespace to a rare
        // combining-mark-only word boundary; production trims that edge, so
        // the reference must classify the trimmed segment too.
        let segment = if segment.chars().next().is_some_and(char::is_whitespace) {
            segment.trim_matches(char::is_whitespace)
        } else {
            segment
        };
        if segment.is_empty() {
            continue;
        }
        if graphemes != GraphemeMode::Discard
            && segment.as_bytes().first().is_some_and(u8::is_ascii_digit)
            && !segment.is_ascii()
            && emojis::get(segment).is_some()
        {
            push_reference(&mut output, segment, Classification::Emoji);
            continue;
        }
        if segment.chars().any(char::is_alphanumeric) {
            push_reference(&mut output, segment, Classification::Word);
            continue;
        }
        if graphemes == GraphemeMode::Discard {
            continue;
        }
        for grapheme in segment.trim_matches(char::is_whitespace).graphemes(true) {
            let classification = if emojis::get(grapheme).is_some() {
                Some(Classification::Emoji)
            } else if graphemes == GraphemeMode::Retain {
                reference_retained_grapheme(grapheme)
            } else {
                None
            };
            if let Some(classification) = classification {
                push_reference(&mut output, grapheme, classification);
            }
        }
    }
    output
}

fn push_reference(output: &mut Vec<ReferenceToken>, text: &str, classification: Classification) {
    output.push(ReferenceToken {
        text: text.to_owned(),
        pos: output.len() as u32,
        classification,
        split: false,
    });
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

fn reference_fold(text: &str, case: Folding, accent: Folding) -> String {
    let case_folded = match case {
        Folding::Preserve => text.to_owned(),
        Folding::Fold => text.chars().flat_map(char::to_lowercase).collect(),
    };
    match accent {
        Folding::Preserve => case_folded,
        Folding::Fold => case_folded
            .chars()
            .nfd()
            .filter(|c| !is_combining_mark(*c))
            .nfc()
            .collect(),
    }
}

fn reference_long_token(text: &str, spec: LongTokenSpec) -> Vec<String> {
    if text.len() <= spec.max_bytes {
        return vec![text.to_owned()];
    }
    match spec.mode {
        LongTokenMode::Discard => Vec::new(),
        LongTokenMode::Truncate => {
            let mut output = String::new();
            for grapheme in text.graphemes(true) {
                if output.len() + grapheme.len() > spec.max_bytes {
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
                if grapheme.len() > spec.max_bytes {
                    if !chunk.is_empty() {
                        chunks.push(std::mem::take(&mut chunk));
                    }
                    for scalar in grapheme.chars() {
                        if !chunk.is_empty() && chunk.len() + scalar.len_utf8() > spec.max_bytes {
                            chunks.push(std::mem::take(&mut chunk));
                        }
                        chunk.push(scalar);
                    }
                } else {
                    if !chunk.is_empty() && chunk.len() + grapheme.len() > spec.max_bytes {
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

fn targeted_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            "[A-Za-z0-9_]{0,16}",
            Just("can't 29.3".to_owned()),
            Just("CAFÉ Straße İ".to_owned()),
            Just("東京 مرحبا क्\u{200d}ष".to_owned()),
            Just("a\u{0301}\u{0327}".to_owned()),
            Just("\u{0301}\u{0327}".to_owned()),
            Just(" \u{0351}\u{034c}\u{0369}\u{0314}\u{0357}\u{0305} ".to_owned()),
            Just("😀 👨‍👩‍👧‍👦 👍🏽 🇺🇳 1️⃣".to_owned()),
            Just(" +—©™!\t\n".to_owned()),
            Just("\u{0000}\u{200d}\u{fe0f}".to_owned()),
            Just("x² 3½ a¹b ¼¾".to_owned()),
            Just("\u{1100}\u{1161}\u{11a8} 한글".to_owned()),
        ],
        0..30,
    )
    .prop_map(|parts| parts.concat())
}

fn spec_from_indexes(
    tokenizer_index: u8,
    case_index: u8,
    accent_index: u8,
    long_index: u8,
    max_bytes: usize,
    grapheme_index: u8,
    position_index: u8,
) -> TokenizerPipelineSpec {
    TokenizerPipelineSpec {
        stemmer: None,
        tokenizer: if tokenizer_index == 0 {
            TokenizerSpec::Unicode
        } else {
            TokenizerSpec::Whitespace
        },
        case_folding: if case_index == 0 {
            Folding::Preserve
        } else {
            Folding::Fold
        },
        accent_folding: if accent_index == 0 {
            Folding::Preserve
        } else {
            Folding::Fold
        },
        long_tokens: LongTokenSpec {
            mode: match long_index {
                0 => LongTokenMode::Truncate,
                1 => LongTokenMode::Discard,
                _ => LongTokenMode::Split,
            },
            max_bytes,
        },
        graphemes: match grapheme_index {
            0 => GraphemeMode::Discard,
            1 => GraphemeMode::Emoji,
            _ => GraphemeMode::Retain,
        },
        position_gaps: if position_index == 0 {
            PositionGapMode::Collapse
        } else {
            PositionGapMode::Preserve
        },
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    // The fixed default tuple compiles to its own specialized iterators, so
    // it needs the same independent-oracle coverage as the configured
    // pipelines: the generated specs below never land on the default tuple
    // (their max_bytes stays under 17), which would otherwise leave the
    // specialized default path tested only against in-crate references that
    // share its folding code.
    #[test]
    fn default_pipeline_matches_independent_materialized_reference(text in targeted_text()) {
        let spec = TokenizerPipelineSpec::tin_default();
        let expected = reference_pipeline(&text, spec);
        let pipeline = spec.compile().unwrap();
        let mut stream = pipeline.tokenize(&text);
        let actual = stream
            .by_ref()
            .map(|token| {
                let split = token.came_from_split();
                ReferenceToken {
                    text: token.text.into_owned(),
                    pos: token.pos,
                    classification: token.classification,
                    split,
                }
            })
            .collect::<Vec<_>>();
        prop_assert_eq!((actual, stream.positions_consumed()), expected);
    }

    // The source-span view walks the general tokenizer, so its alignment
    // with the specialized default pipeline holds only while the two emit
    // identical position streams; pin that alignment directly.
    #[test]
    fn source_spans_stay_position_aligned_with_the_default_pipeline(text in targeted_text()) {
        let spec = TokenizerPipelineSpec::tin_default();
        let pipeline = spec.compile().unwrap();
        let source_spans = pipeline.source_spans();
        let mut spans = source_spans.tokenize(&text);
        let slots = spans
            .by_ref()
            .map(|token| {
                prop_assert!(
                    matches!(token.text, std::borrow::Cow::Borrowed(_)),
                    "source spans must borrow from the input"
                );
                Ok(token.text.into_owned())
            })
            .collect::<Result<Vec<_>, TestCaseError>>()?;
        let mut stream = pipeline.tokenize(&text);
        for token in stream.by_ref() {
            let slot = slots.get(token.pos as usize);
            prop_assert!(
                slot.is_some(),
                "pipeline emitted position {} but only {} slots exist",
                token.pos,
                slots.len(),
            );
            let folded_slot =
                reference_fold(slot.unwrap(), spec.case_folding, spec.accent_folding);
            prop_assert!(
                folded_slot.contains(token.text.as_ref()),
                "slot at position {} folds to {:?}, which does not cover pipeline token {:?}",
                token.pos,
                folded_slot,
                token.text,
            );
        }
        prop_assert_eq!(spans.positions_consumed() as usize, slots.len());
        prop_assert_eq!(stream.positions_consumed(), spans.positions_consumed());
    }

    // The source-span view must stay position-aligned with the pipeline for
    // every configuration: dense slot ordinals, monotonic borrowed spans, and
    // for every emitted pipeline token a slot at its position whose folded
    // source covers the token's text.
    #[test]
    fn source_spans_stay_position_aligned_with_the_pipeline(
        text in targeted_text(),
        tokenizer_index in 0u8..2,
        case_index in 0u8..2,
        accent_index in 0u8..2,
        long_index in 0u8..3,
        max_bytes in tokenizer::MIN_TOKEN_BYTES..17,
        grapheme_index in 0u8..3,
        position_index in 0u8..2,
    ) {
        let spec = spec_from_indexes(
            tokenizer_index,
            case_index,
            accent_index,
            long_index,
            max_bytes,
            grapheme_index,
            position_index,
        );
        let pipeline = spec.compile().unwrap();
        let mut stream = pipeline.tokenize(&text);
        let tokens = stream
            .by_ref()
            .map(|token| (token.text.into_owned(), token.pos))
            .collect::<Vec<_>>();

        let base = text.as_ptr() as usize;
        let mut spans = Vec::new();
        let source_spans = pipeline.source_spans();
        let mut span_stream = source_spans.tokenize(&text);
        for (ordinal, token) in span_stream.by_ref().enumerate() {
            prop_assert_eq!(token.pos as usize, ordinal, "slot ordinals must be dense");
            prop_assert!(
                matches!(token.text, std::borrow::Cow::Borrowed(_)),
                "source spans must borrow from the input"
            );
            let slice = match &token.text {
                std::borrow::Cow::Borrowed(slice) => *slice,
                std::borrow::Cow::Owned(owned) => owned.as_str(),
            };
            let start = slice.as_ptr() as usize - base;
            prop_assert!(start + slice.len() <= text.len(), "span must stay inside the input");
            // Starts are non-decreasing; slots may overlap only when folding
            // fissions one indivisible source cluster into several chunks.
            if let Some(&(_, previous_start)) = spans.last() {
                prop_assert!(start >= previous_start, "span starts must be monotonic");
            }
            spans.push((slice.to_owned(), start));
        }
        prop_assert_eq!(span_stream.positions_consumed() as usize, spans.len());
        prop_assert_eq!(stream.positions_consumed(), span_stream.positions_consumed());

        for (token_text, pos) in &tokens {
            prop_assert!(
                (*pos as usize) < spans.len(),
                "pipeline emitted position {} but only {} slots exist",
                pos,
                spans.len(),
            );
            let (span_text, _) = &spans[*pos as usize];
            let folded_span = reference_fold(span_text, spec.case_folding, spec.accent_folding);
            prop_assert!(
                folded_span.contains(token_text.as_str()),
                "slot at position {} folds to {:?}, which does not cover pipeline token {:?}",
                pos,
                folded_span,
                token_text,
            );
        }
    }

    #[test]
    fn compiled_pipeline_matches_independent_materialized_reference(
        text in targeted_text(),
        tokenizer_index in 0u8..2,
        case_index in 0u8..2,
        accent_index in 0u8..2,
        long_index in 0u8..3,
        max_bytes in tokenizer::MIN_TOKEN_BYTES..17,
        grapheme_index in 0u8..3,
        position_index in 0u8..2,
    ) {
        let spec = spec_from_indexes(
            tokenizer_index,
            case_index,
            accent_index,
            long_index,
            max_bytes,
            grapheme_index,
            position_index,
        );
        let expected = reference_pipeline(&text, spec);
        let pipeline = spec.compile().unwrap();
        let mut stream = pipeline.tokenize(&text);
        let actual = stream
            .by_ref()
            .map(|token| {
                let split = token.came_from_split();
                ReferenceToken {
                    text: token.text.into_owned(),
                    pos: token.pos,
                    classification: token.classification,
                    split,
                }
            })
            .collect::<Vec<_>>();
        prop_assert_eq!((actual, stream.positions_consumed()), expected);
    }
}
