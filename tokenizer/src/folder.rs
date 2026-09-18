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
use crate::{Folding, Token};
use std::borrow::Cow;
use unicode_normalization::char::{canonical_combining_class, is_combining_mark};
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick, is_nfd_quick};

/// Precomputed case-and-accent fold for every scalar value <= U+00FF: the
/// result of lowercasing, canonically decomposing, stripping combining marks,
/// and recomposing (NFC) each character in isolation. Every Latin-1 scalar
/// folds to exactly one char, itself <= U+00FF, so the folded UTF-8 never
/// grows and per-character folding agrees with whole-string folding (no
/// combining marks or composing starter pairs exist in the output). Pinned
/// against the materialized fold pipeline by `fold_latin1_table_matches_
/// materialized_pipeline`.
const FOLD_LATIN1: [char; 256] = [
    '\u{0}', '\u{1}', '\u{2}', '\u{3}', '\u{4}', '\u{5}', '\u{6}', '\u{7}', '\u{8}', '\u{9}',
    '\u{a}', '\u{b}', '\u{c}', '\u{d}', '\u{e}', '\u{f}', '\u{10}', '\u{11}', '\u{12}', '\u{13}',
    '\u{14}', '\u{15}', '\u{16}', '\u{17}', '\u{18}', '\u{19}', '\u{1a}', '\u{1b}', '\u{1c}',
    '\u{1d}', '\u{1e}', '\u{1f}', '\u{20}', '\u{21}', '\u{22}', '\u{23}', '\u{24}', '\u{25}',
    '\u{26}', '\u{27}', '\u{28}', '\u{29}', '\u{2a}', '\u{2b}', '\u{2c}', '\u{2d}', '\u{2e}',
    '\u{2f}', '\u{30}', '\u{31}', '\u{32}', '\u{33}', '\u{34}', '\u{35}', '\u{36}', '\u{37}',
    '\u{38}', '\u{39}', '\u{3a}', '\u{3b}', '\u{3c}', '\u{3d}', '\u{3e}', '\u{3f}', '\u{40}',
    '\u{61}', '\u{62}', '\u{63}', '\u{64}', '\u{65}', '\u{66}', '\u{67}', '\u{68}', '\u{69}',
    '\u{6a}', '\u{6b}', '\u{6c}', '\u{6d}', '\u{6e}', '\u{6f}', '\u{70}', '\u{71}', '\u{72}',
    '\u{73}', '\u{74}', '\u{75}', '\u{76}', '\u{77}', '\u{78}', '\u{79}', '\u{7a}', '\u{5b}',
    '\u{5c}', '\u{5d}', '\u{5e}', '\u{5f}', '\u{60}', '\u{61}', '\u{62}', '\u{63}', '\u{64}',
    '\u{65}', '\u{66}', '\u{67}', '\u{68}', '\u{69}', '\u{6a}', '\u{6b}', '\u{6c}', '\u{6d}',
    '\u{6e}', '\u{6f}', '\u{70}', '\u{71}', '\u{72}', '\u{73}', '\u{74}', '\u{75}', '\u{76}',
    '\u{77}', '\u{78}', '\u{79}', '\u{7a}', '\u{7b}', '\u{7c}', '\u{7d}', '\u{7e}', '\u{7f}',
    '\u{80}', '\u{81}', '\u{82}', '\u{83}', '\u{84}', '\u{85}', '\u{86}', '\u{87}', '\u{88}',
    '\u{89}', '\u{8a}', '\u{8b}', '\u{8c}', '\u{8d}', '\u{8e}', '\u{8f}', '\u{90}', '\u{91}',
    '\u{92}', '\u{93}', '\u{94}', '\u{95}', '\u{96}', '\u{97}', '\u{98}', '\u{99}', '\u{9a}',
    '\u{9b}', '\u{9c}', '\u{9d}', '\u{9e}', '\u{9f}', '\u{a0}', '\u{a1}', '\u{a2}', '\u{a3}',
    '\u{a4}', '\u{a5}', '\u{a6}', '\u{a7}', '\u{a8}', '\u{a9}', '\u{aa}', '\u{ab}', '\u{ac}',
    '\u{ad}', '\u{ae}', '\u{af}', '\u{b0}', '\u{b1}', '\u{b2}', '\u{b3}', '\u{b4}', '\u{b5}',
    '\u{b6}', '\u{b7}', '\u{b8}', '\u{b9}', '\u{ba}', '\u{bb}', '\u{bc}', '\u{bd}', '\u{be}',
    '\u{bf}', '\u{61}', '\u{61}', '\u{61}', '\u{61}', '\u{61}', '\u{61}', '\u{e6}', '\u{63}',
    '\u{65}', '\u{65}', '\u{65}', '\u{65}', '\u{69}', '\u{69}', '\u{69}', '\u{69}', '\u{f0}',
    '\u{6e}', '\u{6f}', '\u{6f}', '\u{6f}', '\u{6f}', '\u{6f}', '\u{d7}', '\u{f8}', '\u{75}',
    '\u{75}', '\u{75}', '\u{75}', '\u{79}', '\u{fe}', '\u{df}', '\u{61}', '\u{61}', '\u{61}',
    '\u{61}', '\u{61}', '\u{61}', '\u{e6}', '\u{63}', '\u{65}', '\u{65}', '\u{65}', '\u{65}',
    '\u{69}', '\u{69}', '\u{69}', '\u{69}', '\u{f0}', '\u{6e}', '\u{6f}', '\u{6f}', '\u{6f}',
    '\u{6f}', '\u{6f}', '\u{f7}', '\u{f8}', '\u{75}', '\u{75}', '\u{75}', '\u{75}', '\u{79}',
    '\u{fe}', '\u{79}',
];

#[derive(Clone, Copy)]
pub(crate) enum CompiledFolder {
    Preserve,
    Case,
    Accent,
    CaseAndAccent,
}

impl CompiledFolder {
    pub(crate) fn new(case: Folding, accent: Folding) -> Self {
        match (case, accent) {
            (Folding::Preserve, Folding::Preserve) => Self::Preserve,
            (Folding::Fold, Folding::Preserve) => Self::Case,
            (Folding::Preserve, Folding::Fold) => Self::Accent,
            (Folding::Fold, Folding::Fold) => Self::CaseAndAccent,
        }
    }

    #[inline]
    pub(crate) fn apply(self, token: &mut Token<'_>) {
        match self {
            Self::Preserve => {}
            Self::Case => fold_case(token),
            Self::Accent => fold_accent(token),
            Self::CaseAndAccent => fold_case_and_accent(token),
        }
    }

    #[inline]
    pub(crate) fn apply_case_and_accent(token: &mut Token<'_>) {
        fold_case_and_accent(token);
    }
}

#[inline]
fn fold_case(token: &mut Token<'_>) {
    if fold_ascii(token) {
        return;
    }

    if let Some(output) = replace_if_changed(
        token.text.as_ref(),
        token.text.chars().flat_map(char::to_lowercase),
    ) {
        token.text = Cow::Owned(output);
    }
}

#[inline]
fn fold_accent(token: &mut Token<'_>) {
    if token.text.is_ascii() {
        return;
    }

    if let Some(output) = replace_if_changed(
        token.text.as_ref(),
        token
            .text
            .chars()
            .nfd()
            .filter(|c| !is_combining_mark(*c))
            .nfc(),
    ) {
        token.text = Cow::Owned(output);
    }
}

#[inline]
fn fold_case_and_accent(token: &mut Token<'_>) {
    if fold_ascii(token) {
        return;
    }
    match fold_latin1(&token.text) {
        Latin1Fold::Identity => return,
        Latin1Fold::Folded(output) => {
            token.text = Cow::Owned(output);
            return;
        }
        Latin1Fold::NotLatin1 => {}
    }
    if token.text.chars().next().is_some_and(is_combining_mark) {
        // `case_and_accent_unchanged` would fail at the leading mark, so it is
        // skipped; a mark-only token short-circuits to the empty fold instead.
        // Every combining mark folds to nothing: lowercasing is the identity
        // on marks, per-char canonical decomposition of a mark yields only
        // marks (so no cross-char interaction survives), and the accent filter
        // then drops them all — proven exhaustively per scalar by
        // `every_combining_mark_folds_to_empty`, which extends to
        // concatenations because NFD only decomposes per char and reorders.
        // Must be `Cow::Owned`: the fold ("") differs from the input, and the
        // borrowed-iff-unchanged invariant is asserted by the folder proptest.
        // `String::new()` does not allocate.
        if token.text.chars().all(is_combining_mark) {
            token.text = Cow::Owned(String::new());
            return;
        }
    } else if case_and_accent_unchanged(&token.text) {
        return;
    }

    if let Some(output) = replace_if_changed(
        token.text.as_ref(),
        token
            .text
            .chars()
            .flat_map(char::to_lowercase)
            .nfd()
            .filter(|c| !is_combining_mark(*c))
            .nfc(),
    ) {
        token.text = Cow::Owned(output);
    }
}

enum Latin1Fold {
    /// A character above U+00FF appeared; the table does not apply.
    NotLatin1,
    /// Every character folds to itself; the token can stay borrowed.
    Identity,
    /// At least one character changed; the fully folded text.
    Folded(String),
}

/// Single-pass table fold for tokens made entirely of scalars <= U+00FF.
/// Tracks the matched identity prefix so an unchanged token never allocates,
/// and allocates exactly once from the first divergence: `FOLD_LATIN1` never
/// maps a character to a longer UTF-8 encoding, so `with_capacity(text.len())`
/// never reallocates.
#[inline]
fn fold_latin1(text: &str) -> Latin1Fold {
    let mut matched = 0usize;
    let mut output: Option<String> = None;
    for c in text.chars() {
        if c > '\u{00ff}' {
            // Rare mixed token: discard any partial fold and let the caller
            // fall through to the general route.
            return Latin1Fold::NotLatin1;
        }
        let folded = FOLD_LATIN1[c as usize];
        match &mut output {
            Some(output) => output.push(folded),
            None if folded == c => matched += c.len_utf8(),
            None => {
                let mut changed = String::with_capacity(text.len());
                changed.push_str(&text[..matched]);
                changed.push(folded);
                output = Some(changed);
            }
        }
    }
    match output {
        Some(output) => Latin1Fold::Folded(output),
        None => Latin1Fold::Identity,
    }
}

#[cfg(test)]
fn fold_latin1_case_and_accent(text: &str) -> Option<String> {
    if !text.chars().all(|c| c <= '\u{00ff}') {
        return None;
    }
    Some(text.chars().map(|c| FOLD_LATIN1[c as usize]).collect())
}

#[inline]
fn case_and_accent_unchanged(text: &str) -> bool {
    // Fused single-pass equivalent of the original three passes:
    //   (per-char mark + lowercase-identity loop)
    //     && is_nfd_quick(text.chars()) == Yes
    //     && is_nfc_quick(text.chars()) == Yes
    // Each streaming quick-check is itself a conjunction of (a) every char's
    // per-char QC status being Yes and (b) the canonical-combining-class
    // ordering never being violated across adjacent chars. A single-char
    // quick-check call reduces to the per-char QC property lookup — its
    // internal ordering check is vacuous on one char — and the running
    // `last_ccc` tracker below reproduces the cross-char ordering check
    // exactly (including the quick-checks' ASCII fast path, which resets the
    // tracker to zero). The whole predicate is a pure conjunction, so fusing
    // the passes and exiting early computes the same result with one UTF-8
    // decode pass instead of three.
    let mut last_ccc = 0u8;
    for c in text.chars() {
        if c.is_ascii() {
            // ASCII is ccc 0 and quick-check Yes for both forms, and its
            // lowercase is itself exactly when it is not an uppercase letter —
            // skip all table lookups.
            if c.is_ascii_uppercase() {
                return false;
            }
            last_ccc = 0;
            continue;
        }
        if is_combining_mark(c) {
            return false;
        }
        let mut lowercase = c.to_lowercase();
        if lowercase.next() != Some(c) || lowercase.next().is_some() {
            return false;
        }
        let ccc = canonical_combining_class(c);
        if ccc != 0 && ccc < last_ccc {
            return false;
        }
        last_ccc = ccc;
        // Both quick-checks must pass: mark-free NFD text can still change
        // under the closing NFC stage, which composes starter pairs —
        // decomposed Hangul jamo. Jamo vowels and trailing consonants are NFC
        // "maybe" (NFD has no Maybe class), so they take the general path,
        // while unchanged CJK, Arabic, and emoji text keeps the
        // allocation-free early return.
        if is_nfd_quick(std::iter::once(c)) != IsNormalized::Yes {
            return false;
        }
        if is_nfc_quick(std::iter::once(c)) != IsNormalized::Yes {
            return false;
        }
    }
    true
}

#[derive(Clone, Copy)]
enum AsciiCase {
    Lower,
    Upper(usize),
    Unicode,
}

#[inline]
fn ascii_case(text: &str) -> AsciiCase {
    let mut first_uppercase = None;
    for (index, &byte) in text.as_bytes().iter().enumerate() {
        if !byte.is_ascii() {
            return AsciiCase::Unicode;
        }
        if first_uppercase.is_none() && byte.is_ascii_uppercase() {
            first_uppercase = Some(index);
        }
    }
    match first_uppercase {
        Some(index) => AsciiCase::Upper(index),
        None => AsciiCase::Lower,
    }
}

#[inline]
pub(crate) fn lowercase_ascii_from(text: &str, first_uppercase: usize) -> String {
    let mut output = Vec::with_capacity(text.len());
    output.extend_from_slice(&text.as_bytes()[..first_uppercase]);
    output.extend(
        text.as_bytes()[first_uppercase..]
            .iter()
            .map(u8::to_ascii_lowercase),
    );
    // SAFETY: every byte came from ASCII input and ASCII lowercasing.
    unsafe { String::from_utf8_unchecked(output) }
}

#[inline]
fn fold_ascii(token: &mut Token<'_>) -> bool {
    match ascii_case(&token.text) {
        AsciiCase::Lower => true,
        AsciiCase::Upper(first_uppercase) => {
            match &mut token.text {
                Cow::Borrowed(text) => {
                    token.text = Cow::Owned(lowercase_ascii_from(text, first_uppercase));
                }
                Cow::Owned(text) => text.make_ascii_lowercase(),
            }
            true
        }
        AsciiCase::Unicode => false,
    }
}

fn replace_if_changed<I>(text: &str, transformed: I) -> Option<String>
where
    I: Iterator<Item = char>,
{
    let source = text.as_bytes();
    let mut matched = 0usize;
    let mut output: Option<String> = None;
    let mut encoded = [0u8; 4];

    for c in transformed {
        let bytes = c.encode_utf8(&mut encoded).as_bytes();
        match &mut output {
            Some(output) => output.push(c),
            None if source[matched..].starts_with(bytes) => matched += bytes.len(),
            None => {
                let mut changed = String::with_capacity(source.len());
                changed.push_str(
                    std::str::from_utf8(&source[..matched])
                        .expect("matched UTF-8 output must end on a character boundary"),
                );
                changed.push(c);
                output = Some(changed);
            }
        }
    }

    if output.is_some() {
        output
    } else if matched != source.len() {
        Some(
            std::str::from_utf8(&source[..matched])
                .expect("matched UTF-8 output must end on a character boundary")
                .to_owned(),
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Classification;
    use proptest::prelude::*;

    fn apply<'a>(folder: CompiledFolder, text: &'a str) -> Token<'a> {
        let mut token = Token::new(text, 0);
        folder.apply(&mut token);
        token
    }

    #[test]
    fn all_modes_have_fixed_semantics() {
        assert_eq!(apply(CompiledFolder::Preserve, "CAFÉ").text, "CAFÉ");
        assert_eq!(apply(CompiledFolder::Case, "CAFÉ").text, "café");
        assert_eq!(apply(CompiledFolder::Accent, "CAFÉ").text, "CAFE");
        assert_eq!(apply(CompiledFolder::CaseAndAccent, "CAFÉ").text, "cafe");
    }

    #[test]
    fn combined_folder_matches_lowercase_then_accent_semantics() {
        let cases = [
            ("Straße", "straße"),
            ("ΟΣ", "οσ"),
            ("İ", "i"),
            ("I", "i"),
            ("ﬁ", "ﬁ"),
            ("Cafe\u{301}", "cafe"),
            ("\u{301}", ""),
            ("क़", "क"),
            // Decomposed Hangul jamo compose under the closing NFC stage.
            ("\u{1100}\u{1161}", "\u{ac00}"),
            ("\u{1100}\u{1161}\u{11a8}", "\u{ac01}"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                apply(CompiledFolder::CaseAndAccent, input).text,
                expected,
                "input {input:?}"
            );
        }
    }

    #[test]
    fn unchanged_borrowed_text_stays_borrowed() {
        let token = apply(CompiledFolder::CaseAndAccent, "plain123");
        assert!(matches!(token.text, Cow::Borrowed(_)));

        let token = apply(CompiledFolder::Case, "über");
        assert!(matches!(token.text, Cow::Borrowed(_)));

        for text in ["東京", "مرحبا", "😀"] {
            let token = apply(CompiledFolder::CaseAndAccent, text);
            assert!(matches!(token.text, Cow::Borrowed(_)), "input {text:?}");
        }
    }

    #[test]
    fn changed_borrowed_text_owns_only_the_result() {
        let token = apply(CompiledFolder::CaseAndAccent, "CAFÉ");
        assert!(matches!(token.text, Cow::Owned(_)));
        assert_eq!(token.text, "cafe");
    }

    #[test]
    fn direct_latin1_folder_matches_materialized_pipeline() {
        let text = (0..=u8::MAX)
            .filter_map(|byte| char::from_u32(byte as u32))
            .collect::<String>();
        assert_eq!(
            fold_latin1_case_and_accent(&text).unwrap(),
            materialized_fold(&text, Folding::Fold, Folding::Fold)
        );
    }

    #[test]
    fn fold_latin1_table_matches_materialized_pipeline() {
        for i in 0..=255u32 {
            let c = char::from_u32(i).expect("every Latin-1 scalar is a char");
            let folded: Vec<char> = c
                .to_string()
                .chars()
                .flat_map(char::to_lowercase)
                .nfd()
                .filter(|c| !is_combining_mark(*c))
                .nfc()
                .collect();
            assert_eq!(
                folded.len(),
                1,
                "U+{i:04X} must fold to exactly one char, got {folded:?}"
            );
            assert_eq!(FOLD_LATIN1[i as usize], folded[0], "U+{i:04X}");
            assert!(
                folded[0].len_utf8() <= c.len_utf8(),
                "U+{i:04X} must not grow when folded"
            );
        }
    }

    #[test]
    fn latin1_identity_fold_stays_borrowed_and_divergence_allocates_once() {
        // Identity Latin-1 (non-ASCII, mixed 1- and 2-byte chars).
        let token = apply(CompiledFolder::CaseAndAccent, "straße");
        assert!(matches!(token.text, Cow::Borrowed(_)));
        assert_eq!(token.text, "straße");

        // Divergence after an identity prefix keeps the prefix.
        let token = apply(CompiledFolder::CaseAndAccent, "déjà");
        assert!(matches!(token.text, Cow::Owned(_)));
        assert_eq!(token.text, "deja");

        // Fold that keeps two-byte characters still fits the capacity.
        let token = apply(CompiledFolder::CaseAndAccent, "Æther-æß");
        assert_eq!(token.text, "æther-æß");
    }

    #[test]
    fn owned_ascii_case_fold_reuses_its_allocation() {
        let mut token = Token {
            text: Cow::Owned("HELLO".to_owned()),
            pos: 0,
            classification: Classification::Word,
            came_from_split: false,
        };
        let before = token.text.as_ptr();
        CompiledFolder::CaseAndAccent.apply(&mut token);
        assert_eq!(token.text.as_ptr(), before);
        assert_eq!(token.text, "hello");
    }

    #[test]
    fn every_combining_mark_folds_to_empty() {
        // Proves the mark-only-token shortcut in `fold_case_and_accent`: every
        // combining mark's materialized fold is empty. This extends to
        // concatenations of marks because per-char canonical decomposition of
        // a mark yields only marks and NFD's canonical reordering merely
        // permutes them, so the accent filter drops every char and the
        // closing NFC stage receives nothing.
        for c in (0..=char::MAX as u32)
            .filter_map(char::from_u32)
            .filter(|c| is_combining_mark(*c))
        {
            let folded = materialized_fold(&c.to_string(), Folding::Fold, Folding::Fold);
            assert!(
                folded.is_empty(),
                "U+{:04X} folds to {folded:?}, expected empty",
                c as u32
            );
        }
    }

    fn materialized_fold(text: &str, case: Folding, accent: Folding) -> String {
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

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(500))]

        #[test]
        fn every_folder_matches_independent_materialized_stages(text in any::<String>()) {
            for case in [Folding::Preserve, Folding::Fold] {
                for accent in [Folding::Preserve, Folding::Fold] {
                    let expected = materialized_fold(&text, case, accent);
                    let actual = apply(CompiledFolder::new(case, accent), &text);
                    prop_assert_eq!(actual.text.as_ref(), expected.as_str());
                    prop_assert_eq!(
                        matches!(actual.text, Cow::Borrowed(_)),
                        expected == text,
                    );
                }
            }
        }
    }
}
