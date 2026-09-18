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
//! Sub-tokenization rewrite pass for the shared runtime.

use crate::*;
use tokenizer::Tokenizer;

#[derive(Debug, thiserror::Error)]
pub enum SubTokenizeError {
    #[error("range bound \"{bound}\" sub-tokenizes into no tokens")]
    EmptyRangeBound { bound: String },
    #[error("wildcard literal \"{literal}\" is split by the long-token policy")]
    SplitWildcardLiteral { literal: String },
    #[error("fuzzy term \"{term}\" is split by the long-token policy")]
    SplitFuzzyTerm { term: String },
    #[error("range bound \"{bound}\" is split by the long-token policy")]
    SplitRangeBound { bound: String },
}

pub fn sub_tokenize<T: Tokenizer>(expr: Expr, tokenizer: &T) -> Result<Expr, SubTokenizeError> {
    rewrite(expr, tokenizer)
}

struct AnalyzedToken {
    text: String,
    pos: u32,
    came_from_split: bool,
}

fn analyze<T: Tokenizer>(text: &str, tokenizer: &T) -> Vec<AnalyzedToken> {
    tokenizer
        .tokenize(text)
        .map(|token| {
            let pos = token.pos;
            let came_from_split = token.came_from_split();
            AnalyzedToken {
                text: token.text.into_owned(),
                pos,
                came_from_split,
            }
        })
        .collect()
}

fn rewrite<T: Tokenizer>(expr: Expr, tokenizer: &T) -> Result<Expr, SubTokenizeError> {
    match expr {
        Expr::Term(s) => Ok(rewrite_term(&s, tokenizer)),
        Expr::Fuzzy {
            term,
            prefix,
            distance,
        } => {
            let tokens = analyze(&term, tokenizer);
            if tokens.iter().any(|token| token.came_from_split) {
                return Err(SubTokenizeError::SplitFuzzyTerm { term });
            }
            match tokens.len() {
                0 => Ok(Expr::MatchNone),
                1 => Ok(Expr::Fuzzy {
                    term: tokens.into_iter().next().unwrap().text,
                    prefix,
                    distance,
                }),
                // The written term spans a token boundary (`e-mail~1` -> `e`,
                // `mail`): a phrase whose trailing token carries the fuzzy,
                // mirroring how a trailing `*` decorates the boundary token.
                // `~` is a suffix operator, so it binds to the last token;
                // leading/interior tokens stay exact.
                _ => {
                    let mut elements = phrase_elements(tokens);
                    replace_boundary_term(elements.last_mut().unwrap(), |text| Expr::Fuzzy {
                        term: text,
                        prefix,
                        distance,
                    });
                    Ok(Expr::Phrase {
                        elements,
                        slop: None,
                    })
                }
            }
        }
        Expr::Wildcard(parts) => rewrite_wildcard(parts, tokenizer),
        Expr::Regex(_) => Ok(expr),
        Expr::Range { lower, upper } => Ok(Expr::Range {
            lower: normalize_range_bound(lower, tokenizer)?,
            upper: normalize_range_bound(upper, tokenizer)?,
        }),
        Expr::Phrase { elements, slop } => {
            let elements = rewrite_phrase_elements(elements, tokenizer)?;
            Ok(Expr::Phrase { elements, slop })
        }
        Expr::MatchAll | Expr::MatchNone => Ok(expr),
        Expr::Alternatives(xs) => Ok(Expr::Alternatives(rewrite_vec(xs, tokenizer)?)),
        Expr::AtLeast { threshold, exprs } => Ok(Expr::AtLeast {
            threshold,
            exprs: rewrite_vec(exprs, tokenizer)?,
        }),
        Expr::And(l, r) => Ok(Expr::And(
            Box::new(rewrite(*l, tokenizer)?),
            Box::new(rewrite(*r, tokenizer)?),
        )),
        Expr::Or(l, r) => Ok(Expr::Or(
            Box::new(rewrite(*l, tokenizer)?),
            Box::new(rewrite(*r, tokenizer)?),
        )),
        Expr::AndNot { positive, negative } => Ok(Expr::AndNot {
            positive: Box::new(rewrite(*positive, tokenizer)?),
            negative: Box::new(rewrite(*negative, tokenizer)?),
        }),
        Expr::Then { left, right, gap } => Ok(Expr::Then {
            left: Box::new(rewrite(*left, tokenizer)?),
            right: Box::new(rewrite(*right, tokenizer)?),
            gap,
        }),
        Expr::Near { left, right, gap } => Ok(Expr::Near {
            left: Box::new(rewrite(*left, tokenizer)?),
            right: Box::new(rewrite(*right, tokenizer)?),
            gap,
        }),
        Expr::Encloses { big, little } => Ok(Expr::Encloses {
            big: Box::new(rewrite(*big, tokenizer)?),
            little: Box::new(rewrite(*little, tokenizer)?),
        }),
        Expr::NotEncloses { big, little } => Ok(Expr::NotEncloses {
            big: Box::new(rewrite(*big, tokenizer)?),
            little: Box::new(rewrite(*little, tokenizer)?),
        }),
        Expr::EnclosedBy { little, big } => Ok(Expr::EnclosedBy {
            little: Box::new(rewrite(*little, tokenizer)?),
            big: Box::new(rewrite(*big, tokenizer)?),
        }),
        Expr::NotEnclosedBy { little, big } => Ok(Expr::NotEnclosedBy {
            little: Box::new(rewrite(*little, tokenizer)?),
            big: Box::new(rewrite(*big, tokenizer)?),
        }),
        Expr::Overlapping { a, b } => Ok(Expr::Overlapping {
            a: Box::new(rewrite(*a, tokenizer)?),
            b: Box::new(rewrite(*b, tokenizer)?),
        }),
        Expr::NotOverlapping { a, b } => Ok(Expr::NotOverlapping {
            a: Box::new(rewrite(*a, tokenizer)?),
            b: Box::new(rewrite(*b, tokenizer)?),
        }),
        Expr::Before { a, b } => Ok(Expr::Before {
            a: Box::new(rewrite(*a, tokenizer)?),
            b: Box::new(rewrite(*b, tokenizer)?),
        }),
        Expr::After { a, b } => Ok(Expr::After {
            a: Box::new(rewrite(*a, tokenizer)?),
            b: Box::new(rewrite(*b, tokenizer)?),
        }),
        Expr::First { bound, inner } => Ok(Expr::First {
            bound,
            inner: Box::new(rewrite(*inner, tokenizer)?),
        }),
        Expr::Last { bound, inner } => Ok(Expr::Last {
            bound,
            inner: Box::new(rewrite(*inner, tokenizer)?),
        }),
        Expr::Middle { percent, inner } => Ok(Expr::Middle {
            percent,
            inner: Box::new(rewrite(*inner, tokenizer)?),
        }),
        Expr::Between { lo, hi, inner } => Ok(Expr::Between {
            lo,
            hi,
            inner: Box::new(rewrite(*inner, tokenizer)?),
        }),
        Expr::Within { width, inner } => Ok(Expr::Within {
            width,
            inner: Box::new(rewrite(*inner, tokenizer)?),
        }),
        Expr::Boost { factor, inner } => Ok(Expr::Boost {
            factor,
            inner: Box::new(rewrite(*inner, tokenizer)?),
        }),
    }
}

fn rewrite_vec<T: Tokenizer>(
    exprs: Vec<Expr>,
    tokenizer: &T,
) -> Result<Vec<Expr>, SubTokenizeError> {
    exprs.into_iter().map(|e| rewrite(e, tokenizer)).collect()
}

fn rewrite_term<T: Tokenizer>(s: &str, tokenizer: &T) -> Expr {
    let tokens = analyze(s, tokenizer);
    match tokens.len() {
        // A term whose analyzed token stream is empty (bare punctuation,
        // emoji) can never match a stored token: it matches nothing. Failing
        // open to MatchAll here poisoned every enclosing boolean node.
        0 => Expr::MatchNone,
        1 => Expr::Term(tokens.into_iter().next().unwrap().text),
        _ => Expr::Phrase {
            elements: phrase_elements(tokens),
            slop: None,
        },
    }
}

fn phrase_elements(tokens: Vec<AnalyzedToken>) -> Vec<PhraseElement> {
    let mut elements = Vec::with_capacity(tokens.len());
    let mut previous_pos = None;
    for token in tokens {
        if let Some(previous_pos) = previous_pos {
            let gap = token.pos.saturating_sub(previous_pos).saturating_sub(1);
            if gap != 0 {
                elements.push(PhraseElement::Gap(gap));
            }
        }
        previous_pos = Some(token.pos);
        elements.push(PhraseElement::Term(token.text));
    }
    elements
}

fn rewrite_phrase_elements<T: Tokenizer>(
    elements: Vec<PhraseElement>,
    tokenizer: &T,
) -> Result<Vec<PhraseElement>, SubTokenizeError> {
    let mut out = Vec::with_capacity(elements.len());
    let mut term_run = Vec::new();
    let mut pending_gap = 0u32;
    let mut has_anchor = false;
    for elem in elements {
        match elem {
            PhraseElement::Term(s) => term_run.push(s),
            PhraseElement::Gap(gap) => {
                flush_phrase_term_run(
                    &mut term_run,
                    tokenizer,
                    &mut out,
                    &mut pending_gap,
                    &mut has_anchor,
                );
                pending_gap = pending_gap.saturating_add(gap);
            }
            PhraseElement::Alternatives(exprs) => {
                flush_phrase_term_run(
                    &mut term_run,
                    tokenizer,
                    &mut out,
                    &mut pending_gap,
                    &mut has_anchor,
                );
                push_pending_gap(&mut out, &mut pending_gap, has_anchor);
                out.push(PhraseElement::Alternatives(rewrite_vec(exprs, tokenizer)?));
                has_anchor = true;
            }
        }
    }
    flush_phrase_term_run(
        &mut term_run,
        tokenizer,
        &mut out,
        &mut pending_gap,
        &mut has_anchor,
    );
    Ok(out)
}

fn flush_phrase_term_run<T: Tokenizer>(
    terms: &mut Vec<String>,
    tokenizer: &T,
    out: &mut Vec<PhraseElement>,
    pending_gap: &mut u32,
    has_anchor: &mut bool,
) {
    if terms.is_empty() {
        return;
    }

    let text_bytes = terms.iter().map(String::len).sum::<usize>() + terms.len() + 1;
    let mut text = String::with_capacity(text_bytes);
    for term in terms.drain(..) {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&term);
    }
    // A one-byte word survives every valid pipeline. Its final position makes
    // positions consumed by discarded trailing terms observable without
    // widening Tokenizer's streaming interface.
    text.push_str(" a");

    let mut tokens = analyze(&text, tokenizer);
    let sentinel = tokens
        .pop()
        .expect("the phrase position sentinel must survive tokenization");
    debug_assert_eq!(sentinel.text, "a");
    debug_assert!(!sentinel.came_from_split);

    let mut previous_pos = None;
    for token in tokens {
        let gap = previous_pos.map_or(token.pos, |previous: u32| {
            token.pos.saturating_sub(previous).saturating_sub(1)
        });
        *pending_gap = pending_gap.saturating_add(gap);
        push_pending_gap(out, pending_gap, *has_anchor);
        previous_pos = Some(token.pos);
        out.push(PhraseElement::Term(token.text));
        *has_anchor = true;
    }

    let trailing_gap = previous_pos.map_or(sentinel.pos, |previous| {
        sentinel.pos.saturating_sub(previous).saturating_sub(1)
    });
    *pending_gap = pending_gap.saturating_add(trailing_gap);
}

fn push_pending_gap(out: &mut Vec<PhraseElement>, pending_gap: &mut u32, has_anchor: bool) {
    if has_anchor && *pending_gap != 0 {
        out.push(PhraseElement::Gap(*pending_gap));
    }
    *pending_gap = 0;
}

/// One within-token piece of a wildcard pattern. Token boundaries inside a
/// split literal (`e-mail` -> `e`, `mail`) partition the pattern into
/// segments; every piece of a segment matches inside a single index term.
enum SegmentPiece {
    Text(String),
    Any,
    Single,
}

/// A wildcard rewrites segment by segment. Token boundaries come only from
/// literals the tokenizer splits (`e-mail*tail` -> segments `e` and
/// `mail*tail`); wildcard characters always stay within one token position.
/// A segment with no wildcard characters is an exact `Term`; any other
/// segment normalizes to an anchored single-token `Regex` — `*` -> `.*`
/// (zero or more) and `?` -> `.` (exactly one), literal fragments escaped —
/// so `email*` -> `email.*`, `e*mail` -> `e.*mail`, and `e-mail*tail` ->
/// `"e" mail.*tail`. One canonical expansion form means the lowered query
/// never shows an ambiguous bare `*` inside a term.
///
/// One segment yields the expression bare; several yield a phrase with the
/// literal's position gaps preserved. Nothing fails open: an erased literal
/// is `MatchNone` and long-token-policy splits error.
fn rewrite_wildcard<T: Tokenizer>(
    parts: Vec<WildcardPart>,
    tokenizer: &T,
) -> Result<Expr, SubTokenizeError> {
    // Gap-annotated segments: `gap_before` is the position gap crossing the
    // token boundary that opened the segment (0 for the first).
    let mut segments: Vec<(u32, Vec<SegmentPiece>)> = vec![(0, Vec::new())];
    for part in parts {
        match part {
            WildcardPart::Literal(s) => {
                let tokens = analyze(&s, tokenizer);
                if tokens.iter().any(|token| token.came_from_split) {
                    return Err(SubTokenizeError::SplitWildcardLiteral { literal: s });
                }
                if tokens.is_empty() {
                    // Analysis erased user-written literal content (`@*`, `.*`).
                    // Expanding the bare wildcard would match the whole corpus;
                    // the erased pattern matches nothing instead.
                    return Ok(Expr::MatchNone);
                }
                let mut previous_pos = None;
                for token in tokens {
                    if let Some(previous_pos) = previous_pos {
                        let gap = token.pos.saturating_sub(previous_pos).saturating_sub(1);
                        segments.push((gap, Vec::new()));
                    }
                    previous_pos = Some(token.pos);
                    segments
                        .last_mut()
                        .unwrap()
                        .1
                        .push(SegmentPiece::Text(token.text));
                }
            }
            WildcardPart::Any => segments.last_mut().unwrap().1.push(SegmentPiece::Any),
            WildcardPart::Single => segments.last_mut().unwrap().1.push(SegmentPiece::Single),
        }
    }

    if segments.len() == 1 {
        let (_, segment) = segments.pop().unwrap();
        return Ok(segment_expr(segment));
    }

    let mut elements = Vec::with_capacity(segments.len() * 2);
    for (gap_before, segment) in segments {
        if gap_before != 0 {
            elements.push(PhraseElement::Gap(gap_before));
        }
        elements.push(match segment_expr(segment) {
            Expr::Term(text) => PhraseElement::Term(text),
            expr => PhraseElement::Alternatives(vec![expr]),
        });
    }
    Ok(Expr::Phrase {
        elements,
        slop: None,
    })
}

/// Collapses one within-token segment: exact `Term` when no wildcard
/// character is present, anchored single-token `Regex` otherwise.
fn segment_expr(segment: Vec<SegmentPiece>) -> Expr {
    let has_wildcard = segment
        .iter()
        .any(|piece| !matches!(piece, SegmentPiece::Text(_)));
    if !has_wildcard {
        let mut text = String::new();
        for piece in segment {
            if let SegmentPiece::Text(t) = piece {
                text.push_str(&t);
            }
        }
        return Expr::Term(text);
    }

    let mut pattern = String::new();
    for piece in &segment {
        match piece {
            SegmentPiece::Text(text) => pattern.push_str(&regex_syntax::escape(text)),
            SegmentPiece::Any => pattern.push_str(".*"),
            SegmentPiece::Single => pattern.push('.'),
        }
    }
    Expr::Regex(pattern)
}

/// Swaps a boundary `Term` of a `phrase_elements` output for an expression
/// slot built from its text. `phrase_elements` only ever places `Term`s at the
/// boundaries (gaps sit between tokens), which is what makes this total.
fn replace_boundary_term(element: &mut PhraseElement, slot: impl FnOnce(String) -> Expr) {
    let PhraseElement::Term(text) = element else {
        unreachable!("phrase_elements boundaries are terms");
    };
    *element = PhraseElement::Alternatives(vec![slot(std::mem::take(text))]);
}

fn normalize_range_bound<T: Tokenizer>(
    bound: RangeBound,
    tokenizer: &T,
) -> Result<RangeBound, SubTokenizeError> {
    match bound {
        RangeBound::Open => Ok(RangeBound::Open),
        RangeBound::Term(s) => {
            let tokens = analyze(&s, tokenizer);
            if tokens.iter().any(|token| token.came_from_split) {
                return Err(SubTokenizeError::SplitRangeBound { bound: s });
            }
            let normalized: String = tokens.into_iter().map(|token| token.text).collect();
            if normalized.is_empty() {
                // An empty bound would silently widen the range to one end of
                // the whole term dictionary; fail closed like fuzzy bounds do.
                return Err(SubTokenizeError::EmptyRangeBound { bound: s });
            }
            Ok(RangeBound::Term(normalized))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SubTokenizeError, sub_tokenize};
    use crate::{Expr, PhraseElement, RangeBound, WildcardPart};
    use tokenizer::presets::default_pipeline;
    use tokenizer::{
        Folding, GraphemeMode, LongTokenMode, PositionGapMode, TokenizerPipelineSpec, TokenizerSpec,
    };

    fn whitespace_pipeline(
        mode: LongTokenMode,
        max_bytes: usize,
        position_gaps: PositionGapMode,
    ) -> tokenizer::CompiledTokenizerPipeline {
        let mut spec = TokenizerPipelineSpec::tin_default();
        spec.tokenizer = TokenizerSpec::Whitespace;
        spec.case_folding = Folding::Preserve;
        spec.accent_folding = Folding::Preserve;
        spec.graphemes = GraphemeMode::Retain;
        spec.long_tokens.mode = mode;
        spec.long_tokens.max_bytes = max_bytes;
        spec.position_gaps = position_gaps;
        spec.compile().unwrap()
    }

    // A term whose analyzed token stream is empty matches nothing; it never
    // fails open to MatchAll.
    #[test]
    fn zero_token_term_rewrites_to_match_nothing() {
        for term in [",", "...", "@#$"] {
            assert_eq!(
                sub_tokenize(Expr::Term(term.into()), default_pipeline()).unwrap(),
                Expr::MatchNone,
                "term {term:?}",
            );
        }
    }

    #[test]
    fn default_emoji_term_survives_subtokenization() {
        assert_eq!(
            sub_tokenize(Expr::Term("😀".into()), default_pipeline()).unwrap(),
            Expr::Term("😀".into())
        );
    }

    // Zero-token fuzzy obeys the same rule.
    #[test]
    fn zero_token_fuzzy_rewrites_to_match_nothing() {
        assert_eq!(
            sub_tokenize(
                Expr::Fuzzy {
                    term: ".".into(),
                    prefix: 1,
                    distance: 1,
                },
                default_pipeline(),
            )
            .unwrap(),
            Expr::MatchNone,
        );
    }

    // A fuzzy term that spans a token boundary becomes a phrase whose trailing
    // token carries the fuzzy: `wi-fi~1` -> `"wi fi~1"`. The `~` suffix binds
    // to the last token, like a trailing `*`.
    #[test]
    fn boundary_spanning_fuzzy_becomes_a_phrase() {
        assert_eq!(
            sub_tokenize(
                Expr::Fuzzy {
                    term: "wi-fi".into(),
                    prefix: 2,
                    distance: 1,
                },
                default_pipeline(),
            )
            .unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("wi".into()),
                    PhraseElement::Alternatives(vec![Expr::Fuzzy {
                        term: "fi".into(),
                        prefix: 2,
                        distance: 1,
                    }]),
                ],
                slop: None,
            },
        );
    }

    // A wildcard whose written literal content tokenizes away matches
    // nothing; a literal that survives analysis keeps its wildcard, in the
    // canonical regex form.
    #[test]
    fn wildcard_with_erased_literal_rewrites_to_match_nothing() {
        let erased = Expr::Wildcard(vec![WildcardPart::Literal("@".into()), WildcardPart::Any]);
        assert_eq!(
            sub_tokenize(erased, default_pipeline()).unwrap(),
            Expr::MatchNone,
        );

        let survives = Expr::Wildcard(vec![
            WildcardPart::Literal("user@".into()),
            WildcardPart::Any,
        ]);
        assert_eq!(
            sub_tokenize(survives, default_pipeline()).unwrap(),
            Expr::Regex("user.*".into()),
        );
    }

    // Every wildcard normalizes to the anchored regex form, so a bare `*`
    // inside a lowered query is never ambiguous with token text: `*cia*` is
    // `.*cia.*` and `email*` is `email.*`.
    #[test]
    fn single_token_wildcards_normalize_to_regex() {
        let infix = Expr::Wildcard(vec![
            WildcardPart::Any,
            WildcardPart::Literal("cia".into()),
            WildcardPart::Any,
        ]);
        assert_eq!(
            sub_tokenize(infix, default_pipeline()).unwrap(),
            Expr::Regex(".*cia.*".into()),
        );

        let prefix = Expr::Wildcard(vec![
            WildcardPart::Literal("email".into()),
            WildcardPart::Any,
        ]);
        assert_eq!(
            sub_tokenize(prefix, default_pipeline()).unwrap(),
            Expr::Regex("email.*".into()),
        );
    }

    // A hyphenated literal splits across two index terms, so a trailing `*`
    // becomes a phrase whose last position prefix-expands: `wi-fi*` -> `wi`
    // THEN `fi.*`. The default pipeline splits `wi-fi` the same way the term
    // path does, so the two forms stay consistent.
    #[test]
    fn trailing_wildcard_over_split_literal_becomes_a_phrase() {
        let pattern = Expr::Wildcard(vec![
            WildcardPart::Literal("wi-fi".into()),
            WildcardPart::Any,
        ]);
        assert_eq!(
            sub_tokenize(pattern, default_pipeline()).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("wi".into()),
                    PhraseElement::Alternatives(vec![Expr::Regex("fi.*".into())]),
                ],
                slop: None,
            },
        );
    }

    // A leading `*` expands the FIRST boundary token; the interior stays
    // exact.
    #[test]
    fn leading_wildcard_over_split_literal_decorates_the_first_token() {
        let pattern = Expr::Wildcard(vec![
            WildcardPart::Any,
            WildcardPart::Literal("wi-fi".into()),
        ]);
        assert_eq!(
            sub_tokenize(pattern, default_pipeline()).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Alternatives(vec![Expr::Regex(".*wi".into())]),
                    PhraseElement::Term("fi".into()),
                ],
                slop: None,
            },
        );
    }

    // A flanking `?` binds to the boundary token exactly like a flanking `*`,
    // translating to `.` (exactly one character): `wi-fi?` -> `"wi fi."` and
    // `?wi-fi` -> `".wi fi"`. Mixed flank runs keep their order.
    #[test]
    fn single_char_wildcard_flanks_bind_to_the_boundary_token() {
        let trailing = Expr::Wildcard(vec![
            WildcardPart::Literal("wi-fi".into()),
            WildcardPart::Single,
        ]);
        assert_eq!(
            sub_tokenize(trailing, default_pipeline()).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("wi".into()),
                    PhraseElement::Alternatives(vec![Expr::Regex("fi.".into())]),
                ],
                slop: None,
            },
        );

        let leading = Expr::Wildcard(vec![
            WildcardPart::Single,
            WildcardPart::Literal("wi-fi".into()),
        ]);
        assert_eq!(
            sub_tokenize(leading, default_pipeline()).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Alternatives(vec![Expr::Regex(".wi".into())]),
                    PhraseElement::Term("fi".into()),
                ],
                slop: None,
            },
        );

        let mixed_run = Expr::Wildcard(vec![
            WildcardPart::Literal("wi-fi".into()),
            WildcardPart::Any,
            WildcardPart::Single,
        ]);
        assert_eq!(
            sub_tokenize(mixed_run, default_pipeline()).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("wi".into()),
                    PhraseElement::Alternatives(vec![Expr::Regex("fi.*.".into())]),
                ],
                slop: None,
            },
        );
    }

    // An interior wildcard fuses its adjacent literal fragments into one
    // anchored single-token regex: `wi-fi*home` -> `"wi" MATCHES fi.*home`.
    #[test]
    fn split_literal_beside_a_second_literal_becomes_a_regex_segment() {
        let pattern = Expr::Wildcard(vec![
            WildcardPart::Literal("wi-fi".into()),
            WildcardPart::Any,
            WildcardPart::Literal("home".into()),
        ]);
        assert_eq!(
            sub_tokenize(pattern, default_pipeline()).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("wi".into()),
                    PhraseElement::Alternatives(vec![Expr::Regex("fi.*home".into())]),
                ],
                slop: None,
            },
        );
    }

    // A single-token interior wildcard converts to a regex outright:
    // `e*mail` -> `MATCHES e.*mail` (zero or more), `e?mail` -> `MATCHES
    // e.mail` (exactly one character, matching the flanking `?` semantics).
    #[test]
    fn single_token_interior_wildcard_becomes_a_regex() {
        let star = Expr::Wildcard(vec![
            WildcardPart::Literal("e".into()),
            WildcardPart::Any,
            WildcardPart::Literal("mail".into()),
        ]);
        assert_eq!(
            sub_tokenize(star, default_pipeline()).unwrap(),
            Expr::Regex("e.*mail".into()),
        );

        let question = Expr::Wildcard(vec![
            WildcardPart::Literal("e".into()),
            WildcardPart::Single,
            WildcardPart::Literal("mail".into()),
        ]);
        assert_eq!(
            sub_tokenize(question, default_pipeline()).unwrap(),
            Expr::Regex("e.mail".into()),
        );
    }

    // Flanking wildcard characters on a regex segment fold into the pattern,
    // and literal fragments are regex-escaped (the whitespace tokenizer keeps
    // `l.a` as one token whose dot must not become a metacharacter).
    #[test]
    fn regex_segments_fold_flanks_and_escape_literal_fragments() {
        let trailing_fold = Expr::Wildcard(vec![
            WildcardPart::Literal("wi-fi".into()),
            WildcardPart::Any,
            WildcardPart::Literal("home".into()),
            WildcardPart::Any,
        ]);
        assert_eq!(
            sub_tokenize(trailing_fold, default_pipeline()).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("wi".into()),
                    PhraseElement::Alternatives(vec![Expr::Regex("fi.*home.*".into())]),
                ],
                slop: None,
            },
        );

        let tokenizer =
            whitespace_pipeline(LongTokenMode::Truncate, 256, PositionGapMode::Preserve);
        let escaped = Expr::Wildcard(vec![
            WildcardPart::Literal("l.a".into()),
            WildcardPart::Any,
            WildcardPart::Literal("b".into()),
        ]);
        assert_eq!(
            sub_tokenize(escaped, &tokenizer).unwrap(),
            Expr::Regex(r"l\.a.*b".into()),
        );
    }

    // A range bound that tokenizes to nothing would silently widen the range
    // to one end of the term dictionary; it fails closed instead.
    #[test]
    fn empty_range_bound_is_rejected() {
        let range = Expr::Range {
            lower: RangeBound::Term("...".into()),
            upper: RangeBound::Term("zzz".into()),
        };
        assert!(matches!(
            sub_tokenize(range, default_pipeline()),
            Err(SubTokenizeError::EmptyRangeBound { .. }),
        ));
    }

    #[test]
    fn combining_only_term_is_not_a_default_emoji_token() {
        let term = "\u{0351}\u{034c}\u{0369}\u{0314}\u{0357}\u{0305}";
        assert_eq!(
            sub_tokenize(Expr::Term(term.into()), default_pipeline()).unwrap(),
            Expr::MatchNone
        );
    }

    #[test]
    fn split_exact_term_becomes_a_phrase() {
        let tokenizer = whitespace_pipeline(LongTokenMode::Split, 4, PositionGapMode::Preserve);
        assert_eq!(
            sub_tokenize(Expr::Term("abcdefghij".into()), &tokenizer).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("abcd".into()),
                    PhraseElement::Term("efgh".into()),
                    PhraseElement::Term("ij".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn preserved_positions_become_explicit_phrase_gaps() {
        let tokenizer = whitespace_pipeline(LongTokenMode::Discard, 4, PositionGapMode::Preserve);
        assert_eq!(
            sub_tokenize(Expr::Term("aa toolong cc".into()), &tokenizer).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("aa".into()),
                    PhraseElement::Gap(1),
                    PhraseElement::Term("cc".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn quoted_phrase_preserves_a_discarded_term_position() {
        let tokenizer = whitespace_pipeline(LongTokenMode::Discard, 4, PositionGapMode::Preserve);
        let phrase = Expr::Phrase {
            elements: vec![
                PhraseElement::Term("aa".into()),
                PhraseElement::Term("toolong".into()),
                PhraseElement::Term("cc".into()),
            ],
            slop: None,
        };
        assert_eq!(
            sub_tokenize(phrase, &tokenizer).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("aa".into()),
                    PhraseElement::Gap(1),
                    PhraseElement::Term("cc".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn quoted_phrase_carries_a_trailing_discard_gap_across_alternatives() {
        let tokenizer = whitespace_pipeline(LongTokenMode::Discard, 4, PositionGapMode::Preserve);
        let phrase = Expr::Phrase {
            elements: vec![
                PhraseElement::Term("aa".into()),
                PhraseElement::Term("toolong".into()),
                PhraseElement::Alternatives(vec![Expr::Term("cc".into())]),
            ],
            slop: None,
        };
        assert_eq!(
            sub_tokenize(phrase, &tokenizer).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("aa".into()),
                    PhraseElement::Gap(1),
                    PhraseElement::Alternatives(vec![Expr::Term("cc".into())]),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn split_phrase_term_expands_in_place() {
        let tokenizer = whitespace_pipeline(LongTokenMode::Split, 4, PositionGapMode::Preserve);
        let phrase = Expr::Phrase {
            elements: vec![PhraseElement::Term("abcdefghij".into())],
            slop: None,
        };
        assert_eq!(
            sub_tokenize(phrase, &tokenizer).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("abcd".into()),
                    PhraseElement::Term("efgh".into()),
                    PhraseElement::Term("ij".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn wildcard_range_and_fuzzy_reject_split_terms() {
        let tokenizer = whitespace_pipeline(LongTokenMode::Split, 4, PositionGapMode::Preserve);
        let wildcard = Expr::Wildcard(vec![
            WildcardPart::Literal("abcdef".into()),
            WildcardPart::Any,
        ]);
        assert!(matches!(
            sub_tokenize(wildcard, &tokenizer),
            Err(SubTokenizeError::SplitWildcardLiteral { .. })
        ));

        let range = Expr::Range {
            lower: RangeBound::Term("abcdef".into()),
            upper: RangeBound::Open,
        };
        assert!(matches!(
            sub_tokenize(range, &tokenizer),
            Err(SubTokenizeError::SplitRangeBound { .. })
        ));

        let fuzzy = Expr::Fuzzy {
            term: "abcdef".into(),
            prefix: 1,
            distance: 1,
        };
        assert!(matches!(
            sub_tokenize(fuzzy, &tokenizer),
            Err(SubTokenizeError::SplitFuzzyTerm { .. })
        ));
    }

    #[test]
    fn regex_pattern_is_not_rewritten_during_subtokenization() {
        let rewritten = sub_tokenize(Expr::Regex("Beer.*".into()), default_pipeline())
            .expect("sub-tokenization should succeed");

        assert_eq!(rewritten, Expr::Regex("Beer.*".into()));
    }
}
