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
mod phrase_grammar;

use pest::Parser;
use pest_derive::Parser;

use crate::ImplicitOp;
use crate::ast::*;
use crate::error::ParseError;
use crate::util::{classify_term, unescape_phrase_term};

use phrase_grammar::{PhraseContentParser, PhraseRule};

#[derive(Parser)]
#[grammar = "parser/pest_parser/grammar.pest"]
struct ExprParser;

/// Parse a query string using the pest parser backend.
pub(crate) fn parse(input: &str, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    let pairs = ExprParser::parse(Rule::query, input).map_err(convert_error)?;
    let query_pair = pairs.into_iter().next().unwrap();
    build_query(query_pair, implicit_op)
}

fn convert_error(e: pest::error::Error<Rule>) -> ParseError {
    match e.location {
        pest::error::InputLocation::Pos(pos) => ParseError::Expected {
            expected: format!("{}", e.variant.message()),
            pos,
            found: "unexpected input".into(),
        },
        pest::error::InputLocation::Span((start, _)) => ParseError::Expected {
            expected: format!("{}", e.variant.message()),
            pos: start,
            found: "unexpected input".into(),
        },
    }
}

type Pair<'a> = pest::iterators::Pair<'a, Rule>;

/// Parse a digit run from the grammar into `u32`, failing cleanly when the
/// value does not fit. The grammar puts no length cap on digit runs, so this
/// is reachable from every numeric surface (`THEN/N`, `~N`, `IN FIRST N`, ...).
fn parse_u32(text: &str, pos: usize) -> Result<u32, ParseError> {
    text.parse().map_err(|_| ParseError::NumberOutOfRange {
        text: text.to_string(),
        pos,
    })
}

fn parse_u32_pair(pair: &Pair<'_>) -> Result<u32, ParseError> {
    parse_u32(pair.as_str(), pair.as_span().start())
}

// ── Top-level ───────────────────────────────────────────────────────

fn build_query(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::query);
    let or_expr = pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::or_expr)
        .unwrap();
    build_or_expr(or_expr, implicit_op)
}

// ── Boolean tier ────────────────────────────────────────────────────

fn build_or_expr(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::or_expr);
    let children = pair.into_inner().filter(|p| p.as_rule() == Rule::and_expr);

    // Collect all OR-level items. In ImplicitOp::Or mode, and_expr may
    // produce multiple items (the implicit-OR groups) which we flatten
    // into the OR chain for correct left-associativity.
    let mut or_items: Vec<Expr> = Vec::new();

    for child in children {
        let items = build_and_expr_items(child, implicit_op)?;
        or_items.extend(items);
    }

    let mut iter = or_items.into_iter();
    let mut left = iter.next().unwrap();
    for right in iter {
        left = Expr::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn build_alt_or_expr(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::alt_or_expr);
    let mut children = pair
        .into_inner()
        .filter(|p| p.as_rule() == Rule::alt_and_expr);
    let first = children.next().unwrap();
    let mut left = build_alt_and_expr(first, implicit_op)?;
    for child in children {
        let right = build_alt_and_expr(child, implicit_op)?;
        left = Expr::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
}

/// Build an and_expr node. Returns a list of OR-level items.
///
/// For ImplicitOp::And: always returns a single-element vec (the AND chain).
/// For ImplicitOp::Or: returns multiple items when implicit juxtaposition
/// is present — these become OR operands at the or_expr level.
fn build_and_expr_items(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Vec<Expr>, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::and_expr);

    let mut inner = pair.into_inner();

    let first = inner.next().unwrap();
    debug_assert_eq!(first.as_rule(), Rule::andnot_expr);
    let first_expr = build_andnot_expr(first, implicit_op)?;

    // Collect (separator_is_explicit_and, expr) pairs
    let mut items: Vec<(bool, Expr)> = vec![(false, first_expr)];

    while let Some(sep) = inner.next() {
        if sep.as_rule() != Rule::and_sep {
            continue;
        }
        let is_explicit = sep.into_inner().any(|p| p.as_rule() == Rule::kw_AND);
        let operand = inner.next().unwrap();
        debug_assert_eq!(operand.as_rule(), Rule::andnot_expr);
        let expr = build_andnot_expr(operand, implicit_op)?;
        items.push((is_explicit, expr));
    }

    if items.len() == 1 {
        return Ok(vec![items.into_iter().next().unwrap().1]);
    }

    match implicit_op {
        ImplicitOp::And => {
            let mut iter = items.into_iter();
            let mut left = iter.next().unwrap().1;
            for (_, right) in iter {
                left = Expr::And(Box::new(left), Box::new(right));
            }
            Ok(vec![left])
        }
        ImplicitOp::Or => {
            // Group by explicit AND boundaries.
            // Consecutive implicit items become separate OR groups.
            // Explicit AND sub-chains become single AND groups.
            //
            // Example: [a, IMPLICIT, b, AND, c, IMPLICIT, d]
            // Returns: [a, (b AND c), d]  ← these become OR operands
            let mut groups: Vec<Expr> = Vec::new();
            let mut and_chain: Vec<Expr> = Vec::new();

            for (is_explicit_and, expr) in items {
                if is_explicit_and {
                    and_chain.push(expr);
                } else {
                    if !and_chain.is_empty() {
                        groups.push(fold_left_and(and_chain));
                        and_chain = Vec::new();
                    }
                    and_chain.push(expr);
                }
            }
            if !and_chain.is_empty() {
                groups.push(fold_left_and(and_chain));
            }

            Ok(groups)
        }
    }
}

fn fold_left_and(items: Vec<Expr>) -> Expr {
    let mut iter = items.into_iter();
    let mut left = iter.next().unwrap();
    for right in iter {
        left = Expr::And(Box::new(left), Box::new(right));
    }
    left
}

fn build_andnot_expr(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::andnot_expr);
    let mut children = pair.into_inner().filter(|p| p.as_rule() == Rule::pos_expr);
    let first = children.next().unwrap();
    let mut left = build_pos_expr(first, implicit_op)?;
    for child in children {
        let right = build_pos_expr(child, implicit_op)?;
        left = Expr::AndNot {
            positive: Box::new(left),
            negative: Box::new(right),
        };
    }
    Ok(left)
}

fn build_alt_and_expr(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::alt_and_expr);
    let mut children = pair
        .into_inner()
        .filter(|p| p.as_rule() == Rule::alt_andnot_expr);
    let first = children.next().unwrap();
    let mut left = build_alt_andnot_expr(first, implicit_op)?;
    for child in children {
        let right = build_alt_andnot_expr(child, implicit_op)?;
        left = Expr::And(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn build_alt_andnot_expr(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::alt_andnot_expr);
    let mut children = pair.into_inner().filter(|p| p.as_rule() == Rule::pos_expr);
    let first = children.next().unwrap();
    let mut left = build_pos_expr(first, implicit_op)?;
    for child in children {
        let right = build_pos_expr(child, implicit_op)?;
        left = Expr::AndNot {
            positive: Box::new(left),
            negative: Box::new(right),
        };
    }
    Ok(left)
}

// ── Positional filter tier ──────────────────────────────────────────

fn build_pos_expr(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::pos_expr);
    let mut inner = pair.into_inner();
    let rel = inner.next().unwrap();
    let expr = build_rel_expr(rel, implicit_op)?;

    if let Some(filter) = inner.find(|p| p.as_rule() == Rule::pos_filter) {
        build_pos_filter(filter, expr)
    } else {
        Ok(expr)
    }
}

fn build_pos_filter(pair: Pair<'_>, inner: Expr) -> Result<Expr, ParseError> {
    let variant = pair
        .into_inner()
        .find(|p| {
            matches!(
                p.as_rule(),
                Rule::pos_first | Rule::pos_last | Rule::pos_middle | Rule::pos_between
            )
        })
        .unwrap();

    match variant.as_rule() {
        Rule::pos_first => {
            let mut children = variant.into_inner();
            let int_pair = children.find(|p| p.as_rule() == Rule::integer).unwrap();
            let n = parse_u32_pair(&int_pair)?;
            let has_pct = children.any(|p| p.as_rule() == Rule::pct);
            let bound = if has_pct {
                PositionBound::Percent(n)
            } else {
                PositionBound::Absolute(n)
            };
            Ok(Expr::First {
                bound,
                inner: Box::new(inner),
            })
        }
        Rule::pos_last => {
            let mut children = variant.into_inner();
            let int_pair = children.find(|p| p.as_rule() == Rule::integer).unwrap();
            let n = parse_u32_pair(&int_pair)?;
            let has_pct = children.any(|p| p.as_rule() == Rule::pct);
            let bound = if has_pct {
                PositionBound::Percent(n)
            } else {
                PositionBound::Absolute(n)
            };
            Ok(Expr::Last {
                bound,
                inner: Box::new(inner),
            })
        }
        Rule::pos_middle => {
            let int_pair = variant
                .into_inner()
                .find(|p| p.as_rule() == Rule::integer)
                .unwrap();
            let n = parse_u32_pair(&int_pair)?;
            Ok(Expr::Middle {
                percent: n,
                inner: Box::new(inner),
            })
        }
        Rule::pos_between => {
            let ints: Vec<u32> = variant
                .into_inner()
                .filter(|p| p.as_rule() == Rule::integer)
                .map(|p| parse_u32_pair(&p))
                .collect::<Result<_, _>>()?;
            Ok(Expr::Between {
                lo: ints[0],
                hi: ints[1],
                inner: Box::new(inner),
            })
        }
        _ => unreachable!(),
    }
}

// ── Relation tier ───────────────────────────────────────────────────

fn build_rel_expr(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::rel_expr);
    let mut inner = pair.into_inner();
    let left_pair = inner.next().unwrap();
    let left = build_span_expr(left_pair, implicit_op)?;

    if let Some(op_pair) = inner.find(|p| p.as_rule() == Rule::rel_op) {
        let right_pair = inner.next().unwrap();
        let right = build_span_expr(right_pair, implicit_op)?;
        Ok(match classify_rel_op(&op_pair) {
            RelOp::Encloses => Expr::Encloses {
                big: Box::new(left),
                little: Box::new(right),
            },
            RelOp::NotEncloses => Expr::NotEncloses {
                big: Box::new(left),
                little: Box::new(right),
            },
            RelOp::EnclosedBy => Expr::EnclosedBy {
                little: Box::new(left),
                big: Box::new(right),
            },
            RelOp::NotEnclosedBy => Expr::NotEnclosedBy {
                little: Box::new(left),
                big: Box::new(right),
            },
            RelOp::Overlapping => Expr::Overlapping {
                a: Box::new(left),
                b: Box::new(right),
            },
            RelOp::NotOverlapping => Expr::NotOverlapping {
                a: Box::new(left),
                b: Box::new(right),
            },
            RelOp::Before => Expr::Before {
                a: Box::new(left),
                b: Box::new(right),
            },
            RelOp::After => Expr::After {
                a: Box::new(left),
                b: Box::new(right),
            },
        })
    } else {
        Ok(left)
    }
}

enum RelOp {
    Encloses,
    NotEncloses,
    EnclosedBy,
    NotEnclosedBy,
    Overlapping,
    NotOverlapping,
    Before,
    After,
}

fn classify_rel_op(pair: &Pair<'_>) -> RelOp {
    let children: Vec<Rule> = pair.clone().into_inner().map(|p| p.as_rule()).collect();
    if children.contains(&Rule::kw_NOT) {
        if children.contains(&Rule::kw_ENCLOSES) {
            RelOp::NotEncloses
        } else if children.contains(&Rule::kw_ENCLOSED) {
            RelOp::NotEnclosedBy
        } else {
            RelOp::NotOverlapping
        }
    } else if children.contains(&Rule::kw_ENCLOSES) {
        RelOp::Encloses
    } else if children.contains(&Rule::kw_ENCLOSED) {
        RelOp::EnclosedBy
    } else if children.contains(&Rule::kw_OVERLAPPING) {
        RelOp::Overlapping
    } else if children.contains(&Rule::kw_BEFORE) {
        RelOp::Before
    } else {
        RelOp::After
    }
}

// ── Span tier ───────────────────────────────────────────────────────

fn build_span_expr(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::span_expr);
    let mut inner = pair.into_inner();
    let first = inner.next().unwrap();
    let mut left = build_atom_expr(first, implicit_op)?;

    while let Some(next) = inner.next() {
        if next.as_rule() == Rule::span_op {
            let (is_then, gap) = parse_span_op(&next)?;
            let right_pair = inner.next().unwrap();
            let right = build_atom_expr(right_pair, implicit_op)?;
            if is_then {
                left = Expr::Then {
                    left: Box::new(left),
                    right: Box::new(right),
                    gap,
                };
            } else {
                left = Expr::Near {
                    left: Box::new(left),
                    right: Box::new(right),
                    gap,
                };
            }
        }
    }

    Ok(left)
}

fn parse_span_op(pair: &Pair<'_>) -> Result<(bool, u32), ParseError> {
    let mut is_then = true;
    let mut gap = 0u32;
    for child in pair.clone().into_inner() {
        match child.as_rule() {
            Rule::kw_THEN => is_then = true,
            Rule::kw_NEAR => is_then = false,
            Rule::slash_int => {
                gap = parse_u32(&child.as_str()[1..], child.as_span().start() + 1)?;
            }
            _ => {}
        }
    }
    Ok((is_then, gap))
}

// ── Atom tier ───────────────────────────────────────────────────────

fn build_atom_expr(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::atom_expr);
    let inner: Vec<Pair<'_>> = pair.into_inner().collect();

    let mut expr = build_primary(inner[0].clone(), implicit_op)?;

    // Check for WITHIN postfix: kw_WITHIN + integer
    if inner.len() > 1 {
        let int_pair = inner.iter().find(|p| p.as_rule() == Rule::integer).unwrap();
        let width = parse_u32_pair(int_pair)?;
        expr = Expr::Within {
            width,
            inner: Box::new(expr),
        };
    }

    Ok(expr)
}

// ── Primary ─────────────────────────────────────────────────────────

fn build_primary(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::primary);
    // primary is NOT compound atomic — whitespace between base and boost
    // is consumed by pest. We validate adjacency via span positions.
    let inner: Vec<Pair<'_>> = pair.into_inner().collect();

    let base_pair = &inner[0];
    let expr = build_base(base_pair.clone(), implicit_op)?;

    if let Some(boost_pair) = inner.iter().find(|p| p.as_rule() == Rule::boost) {
        // Validate adjacency: base must end exactly where boost starts
        let base_end = base_pair.as_span().end();
        let boost_start = boost_pair.as_span().start();
        if base_end != boost_start {
            return Err(ParseError::Expected {
                expected: "'^' adjacent to expression (no space)".into(),
                pos: boost_start,
                found: "whitespace before '^'".into(),
            });
        }
        let factor_text = &boost_pair.as_str()[1..];
        // The grammar's `number` rule guarantees `parse` succeeds; it can
        // still overflow to +inf, and BM25's f32 score fold needs the factor
        // inside the documented boost domain.
        let factor: f32 = factor_text.parse().unwrap();
        if !(0.0..=BoostFactor::MAX).contains(&factor) {
            return Err(ParseError::BoostOutOfRange {
                text: factor_text.to_string(),
                pos: boost_start + 1,
            });
        }
        Ok(Expr::Boost {
            factor: BoostFactor(factor),
            inner: Box::new(expr),
        })
    } else {
        Ok(expr)
    }
}

fn build_base(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::base);
    let child = pair.into_inner().next().unwrap();

    match child.as_rule() {
        Rule::grouped => {
            let or_expr = child
                .into_inner()
                .find(|p| p.as_rule() == Rule::or_expr)
                .unwrap();
            build_or_expr(or_expr, implicit_op)
        }
        Rule::all_of => build_all_of(child, implicit_op),
        Rule::at_least => build_at_least(child, implicit_op),
        Rule::alternatives => build_alternatives(child, implicit_op),
        Rule::phrase => build_phrase(child, implicit_op),
        Rule::matches_expr => build_matches_expr(child),
        Rule::contains_expr => build_contains_expr(child),
        Rule::word_primary => build_word_primary(child),
        _ => unreachable!("unexpected base rule: {:?}", child.as_rule()),
    }
}

// ── ALL OF ──────────────────────────────────────────────────────────

fn build_all_of(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    let alts = pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::alternatives)
        .unwrap();
    let exprs = build_alternatives_vec(alts, implicit_op)?;
    Ok(Expr::AtLeast {
        threshold: AtLeastThreshold::All,
        exprs,
    })
}

// ── AT LEAST ────────────────────────────────────────────────────────

fn build_at_least(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    let mut inner = pair.into_inner();
    let num_pair = inner.find(|p| p.as_rule() == Rule::at_least_num).unwrap();
    let num_children: Vec<Pair<'_>> = num_pair.into_inner().collect();
    let int_pair = num_children
        .iter()
        .find(|p| p.as_rule() == Rule::integer)
        .unwrap();
    let n = parse_u32_pair(int_pair)?;
    let has_pct = num_children.iter().any(|p| p.as_rule() == Rule::pct);
    let threshold = if has_pct {
        AtLeastThreshold::Percent(n)
    } else {
        AtLeastThreshold::Count(n)
    };
    let alts = inner.find(|p| p.as_rule() == Rule::alternatives).unwrap();
    let exprs = build_alternatives_vec(alts, implicit_op)?;
    Ok(Expr::AtLeast { threshold, exprs })
}

// ── Alternatives ────────────────────────────────────────────────────

fn build_alternatives(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    let items = build_alternatives_vec(pair, implicit_op)?;
    Ok(Expr::Alternatives(items))
}

fn build_alternatives_vec(
    pair: Pair<'_>,
    implicit_op: ImplicitOp,
) -> Result<Vec<Expr>, ParseError> {
    let open_pos = pair.as_span().start();
    let items: Result<Vec<Expr>, ParseError> = pair
        .into_inner()
        .filter(|p| p.as_rule() == Rule::alt_or_expr)
        .map(|p| build_alt_or_expr(p, implicit_op))
        .collect();
    let items = items?;
    if items.is_empty() {
        return Err(ParseError::EmptyAlternatives { pos: open_pos });
    }
    Ok(split_comma_separated_alternatives(items))
}

/// Alternatives lists document commas as element separators
/// (`[beer, ale, lager]` means `or(beer, ale, lager)`), but the word rule
/// deliberately admits `,` as a term character so free text keeps parsing.
/// Re-split bare-word elements on separator commas here so `[beer,wine]` and
/// `[beer, wine]` both mean the documented OR instead of a phrase. A comma
/// between two digits is a numeric separator, not a list separator — the same
/// distinction word segmentation makes — so `[47,000 48,000]` keeps its two
/// numeric terms.
fn split_comma_separated_alternatives(items: Vec<Expr>) -> Vec<Expr> {
    items
        .into_iter()
        .flat_map(|item| match item {
            Expr::Term(word) if word.contains(',') => {
                let pieces: Vec<Expr> = split_separator_commas(&word)
                    .into_iter()
                    .map(|piece| classify_term(&piece))
                    .collect();
                if pieces.is_empty() {
                    // Pure separator residue like a bare "," element:
                    // analysis-empty, matches nothing.
                    vec![Expr::MatchNone]
                } else {
                    pieces
                }
            }
            other => vec![other],
        })
        .collect()
}

/// Splits `word` at every comma that is not flanked by digits on both sides,
/// dropping empty pieces.
fn split_separator_commas(word: &str) -> Vec<String> {
    let bytes = word.as_bytes();
    let mut pieces = Vec::new();
    let mut start = 0;
    for (idx, byte) in bytes.iter().enumerate() {
        if *byte != b',' {
            continue;
        }
        let numeric_separator = idx > 0
            && bytes[idx - 1].is_ascii_digit()
            && bytes.get(idx + 1).is_some_and(u8::is_ascii_digit);
        if numeric_separator {
            continue;
        }
        if start < idx {
            pieces.push(word[start..idx].to_string());
        }
        start = idx + 1;
    }
    if start < word.len() {
        pieces.push(word[start..].to_string());
    }
    pieces
}

// ── Phrase ──────────────────────────────────────────────────────────

fn build_phrase(pair: Pair<'_>, implicit_op: ImplicitOp) -> Result<Expr, ParseError> {
    let open_pos = pair.as_span().start();
    let mut inner = pair.into_inner();

    let body_pair = inner.find(|p| p.as_rule() == Rule::phrase_body).unwrap();
    let body_text = body_pair.as_str();
    let body_offset = body_pair.as_span().start();

    let slop = inner
        .find(|p| p.as_rule() == Rule::slop)
        .map(|p| parse_u32(&p.as_str()[1..], p.as_span().start() + 1))
        .transpose()?;

    let elements = parse_phrase_content(body_text, body_offset, open_pos, implicit_op)?;
    Ok(Expr::Phrase { elements, slop })
}

fn parse_phrase_content(
    body: &str,
    body_offset: usize,
    open_pos: usize,
    implicit_op: ImplicitOp,
) -> Result<Vec<PhraseElement>, ParseError> {
    if body.is_empty() {
        return Err(ParseError::EmptyPhrase { pos: open_pos });
    }

    let pairs = PhraseContentParser::parse(PhraseRule::phrase_content, body).map_err(|_| {
        ParseError::Expected {
            expected: "valid phrase content".into(),
            pos: body_offset,
            found: "invalid phrase content".into(),
        }
    })?;

    let content_pair = pairs.into_iter().next().unwrap();
    let mut elements = Vec::new();

    for child in content_pair.into_inner() {
        if child.as_rule() == PhraseRule::EOI {
            continue;
        }
        // child is a phrase_element; drill into its single inner variant
        let variant = match child.into_inner().next() {
            Some(v) => v,
            None => continue,
        };
        match variant.as_rule() {
            PhraseRule::phrase_word => {
                let text = variant.as_str();
                if text.contains('\\') {
                    elements.push(PhraseElement::Term(unescape_phrase_term(text)));
                } else {
                    elements.push(PhraseElement::Term(text.to_string()));
                }
            }
            PhraseRule::phrase_gap => {
                let count = variant.as_str().len() as u32;
                elements.push(PhraseElement::Gap(count));
            }
            PhraseRule::phrase_alternatives => {
                let body_pair = variant
                    .into_inner()
                    .find(|p| p.as_rule() == PhraseRule::phrase_alt_body)
                    .unwrap();
                let alt_text = body_pair.as_str();
                let alt_offset = body_offset + body_pair.as_span().start();
                let exprs = parse_alternatives_text(alt_text, alt_offset, implicit_op)?;
                elements.push(PhraseElement::Alternatives(exprs));
            }
            _ => {}
        }
    }

    if elements.is_empty() {
        return Err(ParseError::EmptyPhrase { pos: open_pos });
    }

    Ok(elements)
}

fn parse_alternatives_text(
    text: &str,
    offset: usize,
    implicit_op: ImplicitOp,
) -> Result<Vec<Expr>, ParseError> {
    let synthetic = format!("[{text}]");
    let pairs = ExprParser::parse(Rule::alternatives, &synthetic).map_err(|e| {
        let pos = match e.location {
            pest::error::InputLocation::Pos(p) => p - (1) + offset,
            pest::error::InputLocation::Span((s, _)) => s - (1) + offset,
        };
        ParseError::Expected {
            expected: "expression".into(),
            pos,
            found: "unexpected input".into(),
        }
    })?;

    let alts_pair = pairs.into_iter().next().unwrap();
    alts_pair
        .into_inner()
        .filter(|p| p.as_rule() == Rule::alt_or_expr)
        .map(|p| build_alt_or_expr(p, implicit_op))
        .collect()
}

// ── MATCHES (regex) ──────────────────────────────────────────────────

fn build_matches_expr(pair: Pair<'_>) -> Result<Expr, ParseError> {
    let pattern_pair = pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::matches_pattern)
        .unwrap();
    let raw = pattern_pair.as_str();

    // Unescape `\ ` → ` ` (backslash-whitespace), pass all other
    // `\X` sequences through verbatim as regex content.
    if !raw.contains('\\') {
        return Ok(Expr::Regex(raw.to_string()));
    }

    let mut pattern = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(ws) if ws.is_ascii_whitespace() => pattern.push(ws),
                Some(other) => {
                    pattern.push('\\');
                    pattern.push(other);
                }
                None => pattern.push('\\'),
            }
        } else {
            pattern.push(c);
        }
    }

    Ok(Expr::Regex(pattern))
}

// ── CONTAINS (optional explicit term) ────────────────────────────────

fn build_contains_expr(pair: Pair<'_>) -> Result<Expr, ParseError> {
    let word_primary = pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::word_primary)
        .unwrap();
    build_word_primary(word_primary)
}

// ── Word primary ────────────────────────────────────────────────────

fn build_word_primary(pair: Pair<'_>) -> Result<Expr, ParseError> {
    let inner: Vec<Pair<'_>> = pair.into_inner().collect();

    if inner.len() == 1 {
        let word = inner[0].as_str();
        return Ok(classify_word(word));
    }

    // Range: word TO word
    if inner.iter().any(|p| p.as_rule() == Rule::kw_TO) {
        let words: Vec<&Pair<'_>> = inner.iter().filter(|p| p.as_rule() == Rule::word).collect();
        return Ok(Expr::Range {
            lower: build_range_bound(words[0])?,
            upper: build_range_bound(words[1])?,
        });
    }

    // Fuzzy: word~N or word~P:N
    if let Some(fuzzy) = inner.iter().find(|p| p.as_rule() == Rule::fuzzy_suffix) {
        let word = inner[0].as_str();
        let suffix_pos = fuzzy.as_span().start() + 1;
        let fuzzy = &fuzzy.as_str()[1..];
        let (prefix, distance) = if let Some((prefix, distance)) = fuzzy.split_once(':') {
            (
                parse_u32(prefix, suffix_pos)?,
                parse_u32(distance, suffix_pos + prefix.len() + 1)?,
            )
        } else {
            (1, parse_u32(fuzzy, suffix_pos)?)
        };
        return Ok(Expr::Fuzzy {
            term: word.to_string(),
            prefix,
            distance,
        });
    }

    Ok(classify_word(inner[0].as_str()))
}

fn classify_word(word: &str) -> Expr {
    if word == "*" {
        Expr::MatchAll
    } else {
        classify_term(word)
    }
}

/// Builds one range bound. A bound is either the open marker `*` or a plain
/// term; the free-term wildcard forms (`ban*`, `b?nana`) have no defined
/// range semantics and would otherwise be silently stripped by analysis, so
/// they are rejected here.
fn build_range_bound(word: &Pair<'_>) -> Result<RangeBound, ParseError> {
    let text = word.as_str();
    if text == "*" {
        return Ok(RangeBound::Open);
    }
    if matches!(classify_term(text), Expr::Wildcard(_)) {
        return Err(ParseError::WildcardInRangeBound {
            text: text.to_string(),
            pos: word.as_span().start(),
        });
    }
    Ok(RangeBound::Term(text.to_string()))
}
