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
use std::borrow::Cow;
use tokenizer::{
    Folding, LongTokenMode, PositionGapMode, Stemmer, Tokenizer, TokenizerPipelineSpec,
    TokenizerPipelineSpecError,
};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

const LANGUAGES: [(&str, rust_stemmers::Algorithm); 18] = [
    ("ar", rust_stemmers::Algorithm::Arabic),
    ("da", rust_stemmers::Algorithm::Danish),
    ("nl", rust_stemmers::Algorithm::Dutch),
    ("en", rust_stemmers::Algorithm::English),
    ("fi", rust_stemmers::Algorithm::Finnish),
    ("fr", rust_stemmers::Algorithm::French),
    ("de", rust_stemmers::Algorithm::German),
    ("el", rust_stemmers::Algorithm::Greek),
    ("hu", rust_stemmers::Algorithm::Hungarian),
    ("it", rust_stemmers::Algorithm::Italian),
    ("no", rust_stemmers::Algorithm::Norwegian),
    ("pt", rust_stemmers::Algorithm::Portuguese),
    ("ro", rust_stemmers::Algorithm::Romanian),
    ("ru", rust_stemmers::Algorithm::Russian),
    ("es", rust_stemmers::Algorithm::Spanish),
    ("sv", rust_stemmers::Algorithm::Swedish),
    ("ta", rust_stemmers::Algorithm::Tamil),
    ("tr", rust_stemmers::Algorithm::Turkish),
];

fn spec(stemmer: Stemmer, mode: LongTokenMode, max_bytes: usize) -> TokenizerPipelineSpec {
    let mut spec = TokenizerPipelineSpec::tin_default();
    spec.stemmer = Some(stemmer);
    spec.long_tokens.mode = mode;
    spec.long_tokens.max_bytes = max_bytes;
    spec
}

#[test]
fn language_codes_and_normalization_match_stock_snowball() {
    let words = [
        "RUNNING",
        "runner",
        "généreusement",
        "créées",
        "canción",
        "hablábamos",
        "HÄUSERN",
        "красивыми",
        "kitapları",
        "0",
    ];
    for (code, algorithm) in LANGUAGES {
        let language = code.parse::<Stemmer>().unwrap();
        assert_eq!(language.as_str(), code);
        assert_eq!(language.to_string(), code);
        let stock = rust_stemmers::Stemmer::create(algorithm);
        for accent_folding in [Folding::Fold, Folding::Preserve] {
            let mut spec = spec(language, LongTokenMode::Split, 256);
            spec.accent_folding = accent_folding;
            let pipeline = spec.compile().unwrap();
            for word in words {
                let lowercase = word.to_lowercase().nfc().collect::<String>();
                let stemmed = stock.stem(&lowercase);
                let expected = match accent_folding {
                    Folding::Preserve => stemmed.into_owned(),
                    Folding::Fold => stemmed
                        .nfd()
                        .filter(|c| !is_combining_mark(*c))
                        .nfc()
                        .collect(),
                };
                let actual: Vec<_> = pipeline
                    .tokenize(word)
                    .map(|t| t.text.into_owned())
                    .collect();
                let expected: Vec<_> = (!expected.is_empty())
                    .then_some(expected)
                    .into_iter()
                    .collect();
                assert_eq!(actual, expected, "language={code} word={word:?}");
            }
        }
    }
    for code in ["", "english", "EN", "xx"] {
        assert!(code.parse::<Stemmer>().is_err(), "{code:?}");
    }
}

#[test]
fn canonical_spellings_share_stems_and_preserve_original_spans() {
    for (language, text) in [
        (Stemmer::French, "créées mangées café"),
        (Stemmer::Spanish, "hablábamos canción"),
        (Stemmer::Turkish, "görüşler kitapları"),
    ] {
        for accent_folding in [Folding::Fold, Folding::Preserve] {
            for max_bytes in [4, 256] {
                let mut spec = spec(language, LongTokenMode::Split, max_bytes);
                spec.accent_folding = accent_folding;
                let pipeline = spec.compile().unwrap();
                let expected: Vec<_> = pipeline.tokenize(text).collect();
                for spelling in [text.to_owned(), text.nfd().collect()] {
                    let actual: Vec<_> = pipeline.tokenize(&spelling).collect();
                    assert_eq!(actual, expected, "{language:?} {accent_folding:?}");
                    let spans: Vec<_> = pipeline.source_spans().tokenize(&spelling).collect();
                    for token in &actual {
                        let source = &spans[token.pos as usize];
                        assert_eq!(source.pos, token.pos);
                        assert!(matches!(source.text, Cow::Borrowed(_)));
                        assert!(spelling.split_whitespace().any(|word| {
                            word.as_ptr() == source.text.as_ptr() && word.len() == source.text.len()
                        }));
                        assert!(
                            pipeline
                                .tokenize(&source.text)
                                .any(|word| word.text == token.text)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn already_normalized_unicode_keeps_borrowed_text() {
    let mut spec = spec(Stemmer::English, LongTokenMode::Split, 256);
    spec.accent_folding = Folding::Preserve;
    let pipeline = spec.compile().unwrap();
    // The combining mark makes NFC's quick check inconclusive, but no
    // composition exists for q + acute and the exact check preserves the borrow.
    for word in ["café", "q\u{0301}"] {
        let token = pipeline.tokenize(word).next().unwrap();
        assert_eq!(token.text, word);
        assert!(matches!(token.text, Cow::Borrowed(_)));
        assert_eq!(token.text.as_ptr(), word.as_ptr());
    }
}

#[test]
fn stemming_rejects_preserved_case_and_defaults_stay_disabled() {
    let mut spec = spec(Stemmer::English, LongTokenMode::Split, 256);
    spec.case_folding = Folding::Preserve;
    assert!(matches!(
        spec.validate(),
        Err(TokenizerPipelineSpecError::StemmerRequiresCaseFolding)
    ));
    assert_eq!(TokenizerPipelineSpec::tin_default().stemmer, None);
    assert_eq!(TokenizerPipelineSpec::legacy_tin_default().stemmer, None);
}

#[test]
fn borrowing_and_literal_analysis_keep_their_ownership_and_semantics() {
    let pipeline = spec(Stemmer::English, LongTokenMode::Split, 256)
        .compile()
        .unwrap();
    assert!(matches!(
        pipeline.tokenize("run").next().unwrap().text,
        Cow::Borrowed("run")
    ));
    let actual: Vec<_> = pipeline
        .tokenize("runs running runner")
        .map(|t| t.text.into_owned())
        .collect();
    assert_eq!(actual, ["run", "run", "runner"]);

    // Exercise the &T blanket implementation as well as the concrete method.
    fn literals<T: Tokenizer>(pipeline: T) -> Vec<String> {
        pipeline
            .tokenize_literal("RÚNNING runners")
            .map(|t| t.text.into_owned())
            .collect()
    }
    assert_eq!(literals(&pipeline), ["running", "runners"]);
}

#[test]
fn source_spans_follow_stemming_before_long_token_policy() {
    for mode in [
        LongTokenMode::Split,
        LongTokenMode::Discard,
        LongTokenMode::Truncate,
    ] {
        for position_gaps in [PositionGapMode::Preserve, PositionGapMode::Collapse] {
            let mut spec = spec(Stemmer::English, mode, 4);
            spec.position_gaps = position_gaps;
            let pipeline = spec.compile().unwrap();
            let text = "RUNNING relational cat";
            let tokens: Vec<_> = pipeline.tokenize(text).collect();
            let spans: Vec<_> = pipeline.source_spans().tokenize(text).collect();
            assert_eq!(tokens[0].text, "run");
            assert_eq!(spans[0].text, "RUNNING");
            for token in tokens {
                let span = &spans[token.pos as usize];
                assert_eq!(span.pos, token.pos);
                assert!(matches!(span.text, Cow::Borrowed(_)));
                let source = if token.text == "run" {
                    "RUNNING"
                } else if token.text == "cat" {
                    "cat"
                } else {
                    "relational"
                };
                assert_eq!(span.text, source, "{mode:?} {position_gaps:?}");
            }
        }
    }
}

#[test]
fn empty_normalized_tokens_preserve_or_collapse_positions_with_stemming() {
    let word = "\u{0301}";
    for gaps in [PositionGapMode::Preserve, PositionGapMode::Collapse] {
        let mut spec = spec(Stemmer::English, LongTokenMode::Split, 256);
        spec.tokenizer = tokenizer::TokenizerSpec::Whitespace;
        spec.position_gaps = gaps;
        let pipeline = spec.compile().unwrap();
        let text = format!("{word} 0");
        let tokens: Vec<_> = pipeline.tokenize(&text).collect();
        let spans: Vec<_> = pipeline.source_spans().tokenize(&text).collect();
        assert_eq!(tokens.len(), 1);
        assert_eq!(spans[tokens[0].pos as usize].text, "0");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn stemmed_positions_and_full_spans_match_independent_ascii_policy(
        words in prop::collection::vec("[A-Za-z]{1,40}", 0..20),
        max_bytes in 4usize..32,
        mode_index in 0u8..3,
        preserve in any::<bool>(),
    ) {
        let mode = [LongTokenMode::Split, LongTokenMode::Truncate, LongTokenMode::Discard][mode_index as usize];
        let mut spec = spec(Stemmer::English, mode, max_bytes);
        spec.position_gaps = if preserve { PositionGapMode::Preserve } else { PositionGapMode::Collapse };
        let pipeline = spec.compile().unwrap();
        let stock = rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English);
        let text = words.join(" ");
        let spans: Vec<_> = pipeline.source_spans().tokenize(&text).collect();
        let actual: Vec<_> = pipeline.tokenize(&text).map(|token| {
            let source = &spans[token.pos as usize];
            (token.text.into_owned(), token.pos, source.text.to_string())
        }).collect();
        let mut expected = Vec::new();
        let mut next_position = 0;
        for word in &words {
            let lowercase = word.to_lowercase();
            let stemmed = stock.stem(&lowercase);
            let chunks: Vec<_> = match mode {
                LongTokenMode::Split => stemmed.as_bytes().chunks(max_bytes).map(|chunk| std::str::from_utf8(chunk).unwrap()).collect(),
                LongTokenMode::Truncate => vec![&stemmed[..stemmed.len().min(max_bytes)]],
                LongTokenMode::Discard if stemmed.len() > max_bytes => vec![],
                LongTokenMode::Discard => vec![stemmed.as_ref()],
            };
            let emitted = chunks.len();
            for chunk in chunks {
                expected.push((chunk.to_owned(), next_position, word.clone()));
                next_position += 1;
            }
            if emitted == 0 && preserve {
                next_position += 1;
            }
        }
        prop_assert_eq!(actual, expected);
    }
}
