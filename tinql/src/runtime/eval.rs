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
use std::cmp::Ordering;

use boldi_vigna::{Interval, SpanSolver, TermPositions};
use rustc_hash::FxHashMap;

use super::{Query, RangeBound, SpanTermSlot};

pub struct TokenizedDoc {
    tokens: Vec<String>,
    token_positions: Vec<u32>,
    positions: FxHashMap<String, Vec<u32>>,
}

impl TokenizedDoc {
    pub fn new(positioned_tokens: Vec<(String, u32)>) -> Self {
        let mut tokens = Vec::with_capacity(positioned_tokens.len());
        let mut token_positions = Vec::with_capacity(positioned_tokens.len());
        let mut positions = FxHashMap::default();
        for (token, pos) in positioned_tokens {
            positions
                .entry(token.clone())
                .or_insert_with(Vec::new)
                .push(pos);
            tokens.push(token);
            token_positions.push(pos);
        }
        debug_assert!(token_positions.windows(2).all(|pair| pair[0] < pair[1]));
        Self {
            tokens,
            token_positions,
            positions,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    pub fn positions(&self, term: &str) -> &[u32] {
        self.positions.get(term).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn snippet(&self, interval: Interval, context: usize) -> String {
        if self.tokens.is_empty() {
            return String::new();
        }

        let context = u32::try_from(context).unwrap_or(u32::MAX);
        let lo_position = interval.start.saturating_sub(context);
        let hi_position = interval.end.saturating_add(context);
        let lo = self
            .token_positions
            .partition_point(|position| *position < lo_position);
        let hi = self
            .token_positions
            .partition_point(|position| *position <= hi_position);
        self.tokens[lo..hi].join(" ")
    }
}

pub struct MatchResult {
    pub matched: bool,
    pub intervals: Vec<Interval>,
}

impl MatchResult {
    fn miss() -> Self {
        Self {
            matched: false,
            intervals: Vec::new(),
        }
    }

    fn hit(intervals: Vec<Interval>) -> Self {
        Self {
            matched: true,
            intervals,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("span evaluation failed: {0}")]
    Span(#[from] boldi_vigna::SpanError),
}

pub fn tokenize_doc<T>(text: &str, tokenizer: &T) -> TokenizedDoc
where
    T: tokenizer::Tokenizer,
{
    let tokens = tokenizer
        .tokenize(text)
        .map(|token| (token.text.into_owned(), token.pos))
        .collect();
    TokenizedDoc::new(tokens)
}

pub fn evaluate(query: &Query, doc: &TokenizedDoc) -> Result<MatchResult, EvalError> {
    if doc.is_empty() {
        return Ok(MatchResult::miss());
    }
    evaluate_searchable(query, doc)
}

fn evaluate_searchable(query: &Query, doc: &TokenizedDoc) -> Result<MatchResult, EvalError> {
    match query {
        Query::Term(term) => {
            let intervals = doc
                .positions(term)
                .iter()
                .copied()
                .map(Interval::point)
                .collect::<Vec<_>>();
            if intervals.is_empty() {
                Ok(MatchResult::miss())
            } else {
                Ok(MatchResult::hit(intervals))
            }
        }
        Query::And(left, right) => {
            let left = evaluate_searchable(left, doc)?;
            if !left.matched {
                return Ok(MatchResult::miss());
            }

            let right = evaluate_searchable(right, doc)?;
            if !right.matched {
                return Ok(MatchResult::miss());
            }

            Ok(MatchResult::hit(combine_intervals(
                left.intervals,
                right.intervals,
            )))
        }
        Query::Or(left, right) => {
            let left = evaluate_searchable(left, doc)?;
            let right = evaluate_searchable(right, doc)?;
            match (left.matched, right.matched) {
                (false, false) => Ok(MatchResult::miss()),
                (true, false) => Ok(left),
                (false, true) => Ok(right),
                (true, true) => Ok(MatchResult::hit(combine_intervals(
                    left.intervals,
                    right.intervals,
                ))),
            }
        }
        Query::Conjunction(children) => {
            let mut intervals = Vec::new();
            for child in children {
                let result = evaluate_searchable(child, doc)?;
                if !result.matched {
                    return Ok(MatchResult::miss());
                }
                intervals.extend(result.intervals);
            }

            Ok(MatchResult::hit(normalize_intervals(intervals)))
        }
        Query::Disjunction { min, children } => {
            let mut matched_children = Vec::new();
            for child in children {
                let result = evaluate_searchable(child, doc)?;
                if result.matched {
                    matched_children.push(result.intervals);
                }
            }

            if matched_children.len() < *min as usize {
                return Ok(MatchResult::miss());
            }

            let intervals = matched_children.into_iter().flatten().collect::<Vec<_>>();
            Ok(MatchResult::hit(normalize_intervals(intervals)))
        }
        Query::Not(inner) => {
            let inner = evaluate_searchable(inner, doc)?;
            if inner.matched {
                Ok(MatchResult::miss())
            } else {
                Ok(MatchResult::hit(Vec::new()))
            }
        }
        Query::Span {
            term_slots,
            span_query,
            position_filter,
        } => {
            let positions = term_slots
                .iter()
                .map(|slot| resolve_slot_positions(slot, doc))
                .collect::<Vec<_>>();
            let positions = SlotPositions { positions };
            let mut solver = SpanSolver::new(span_query)?;
            let mut intervals = solver.intervals(&positions).collect::<Vec<_>>();
            if let Some(position_filter) = position_filter {
                let doc_len = doc.len() as u32;
                intervals.retain(|interval| position_filter.matches_interval(doc_len, *interval));
            }
            if intervals.is_empty() {
                Ok(MatchResult::miss())
            } else {
                Ok(MatchResult::hit(intervals))
            }
        }
        Query::SpanExpr {
            term_slots,
            span_expr,
        } => {
            let positions = term_slots
                .iter()
                .map(|slot| resolve_slot_positions(slot, doc))
                .collect::<Vec<_>>();
            let positions = SlotPositions { positions };
            let resolved = span_expr.resolve(doc.len() as u32);
            let mut solver = SpanSolver::new(&resolved)?;
            let intervals = solver.intervals(&positions).collect::<Vec<_>>();
            if intervals.is_empty() {
                Ok(MatchResult::miss())
            } else {
                Ok(MatchResult::hit(intervals))
            }
        }
        Query::MatchAll => Ok(MatchResult::hit(Vec::new())),
        Query::Regex(pattern) => Ok(expanded_match_result(doc, |term| pattern.is_match(term))),
        Query::Range { lower, upper } => Ok(expanded_match_result(doc, |term| {
            range_matches(term, lower, upper)
        })),
        Query::Fuzzy {
            term,
            prefix,
            distance,
        } => {
            let matcher = FuzzyMatcher::new(term, *prefix, *distance);
            Ok(expanded_match_result(doc, |candidate| {
                matcher.is_match(candidate)
            }))
        }
        Query::Boost { inner, .. } => evaluate_searchable(inner, doc),
        Query::AtLeast { min, children } => {
            let mut matched_children = Vec::new();
            for child in children {
                let result = evaluate_searchable(child, doc)?;
                if result.matched {
                    matched_children.push(result.intervals);
                }
            }

            if matched_children.len() < *min as usize {
                return Ok(MatchResult::miss());
            }

            let intervals = matched_children.into_iter().flatten().collect::<Vec<_>>();
            Ok(MatchResult::hit(normalize_intervals(intervals)))
        }
    }
}

struct SlotPositions {
    positions: Vec<Vec<u32>>,
}

impl TermPositions for SlotPositions {
    fn positions(&self, term_index: usize) -> &[u32] {
        &self.positions[term_index]
    }
}

fn resolve_slot_positions(slot: &SpanTermSlot, doc: &TokenizedDoc) -> Vec<u32> {
    match slot {
        SpanTermSlot::Term(term) => doc.positions(term).to_vec(),
        SpanTermSlot::Regex(pattern) => expanded_positions(doc, |term| pattern.is_match(term)),
        SpanTermSlot::Range { lower, upper } => {
            expanded_positions(doc, |term| range_matches(term, lower, upper))
        }
        SpanTermSlot::Fuzzy {
            term,
            prefix,
            distance,
        } => {
            let matcher = FuzzyMatcher::new(term, *prefix, *distance);
            expanded_positions(doc, |candidate| matcher.is_match(candidate))
        }
    }
}

/// A single highlight match: a labeled interval produced by per-component
/// query evaluation against a tokenized document.
#[derive(PartialEq)]
pub struct HighlightMatch {
    pub part: String,
    pub start: u32,
    pub end: u32,
}

/// Evaluates a query against a tokenized document and returns per-query-part
/// labeled intervals suitable for highlight rendering. Unlike [`evaluate`],
/// which returns combined intervals for the whole query, this function
/// decomposes the query and reports each leaf component's matches with its
/// display label.
pub fn evaluate_for_highlight(query: &Query, doc: &TokenizedDoc) -> Vec<HighlightMatch> {
    let mut out = Vec::new();
    collect_highlight_matches(query, doc, &mut out);
    out.sort_unstable_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(a.end.cmp(&b.end))
            .then(a.part.cmp(&b.part))
    });
    out.dedup();
    out
}

fn collect_highlight_matches(query: &Query, doc: &TokenizedDoc, out: &mut Vec<HighlightMatch>) {
    match query {
        Query::Term(term) => {
            for &pos in doc.positions(term) {
                out.push(HighlightMatch {
                    part: term.clone(),
                    start: pos,
                    end: pos,
                });
            }
        }
        Query::And(left, right) | Query::Or(left, right) => {
            collect_highlight_matches(left, doc, out);
            collect_highlight_matches(right, doc, out);
        }
        Query::Conjunction(children)
        | Query::Disjunction { children, .. }
        | Query::AtLeast { children, .. } => {
            for child in children {
                collect_highlight_matches(child, doc, out);
            }
        }
        Query::Not(_) => {
            // NOT suppresses matches — nothing to highlight.
        }
        Query::Boost { inner, .. } => {
            collect_highlight_matches(inner, doc, out);
        }
        Query::Span {
            term_slots,
            span_query,
            position_filter,
        } => {
            let positions = term_slots
                .iter()
                .map(|slot| resolve_slot_positions(slot, doc))
                .collect::<Vec<_>>();
            let slot_pos = SlotPositions { positions };
            let part = format!("{query}");
            let constrained;
            let root = if let Some(filter) = position_filter {
                let Some(window) = filter.resolve_window(doc.len() as u32) else {
                    return;
                };
                constrained = boldi_vigna::SpanQuery::WithinPositions {
                    inner: Box::new(span_query.clone()),
                    lo: window.lo,
                    hi: window.hi,
                };
                &constrained
            } else {
                span_query
            };
            collect_span_marks(root, root, &slot_pos, &part, out);
        }
        Query::SpanExpr {
            term_slots,
            span_expr,
        } => {
            let positions = term_slots
                .iter()
                .map(|slot| resolve_slot_positions(slot, doc))
                .collect::<Vec<_>>();
            let slot_pos = SlotPositions { positions };
            let resolved = span_expr.resolve(doc.len() as u32);
            let part = format!("{query}");
            collect_span_marks(&resolved, &resolved, &slot_pos, &part, out);
        }
        Query::MatchAll => {}
        Query::Regex(pattern) => {
            // Highlight part labels are user-facing (`$QUERY_PART` in
            // tin.highlight tags): use the tinql surface form, not the
            // `REGEX(..)` IR form. Wildcards normalize to Regex during
            // sub-tokenization, so this labels `email*` as `MATCHES email.*`.
            collect_expanded_matches(
                format!("MATCHES {pattern}"),
                doc,
                |term| pattern.is_match(term),
                out,
            );
        }
        Query::Range { lower, upper } => collect_expanded_matches(
            format!("{query}"),
            doc,
            |term| range_matches(term, lower, upper),
            out,
        ),
        Query::Fuzzy {
            term,
            prefix,
            distance,
        } => {
            let matcher = FuzzyMatcher::new(term, *prefix, *distance);
            collect_expanded_matches(
                format!("{query}"),
                doc,
                |candidate| matcher.is_match(candidate),
                out,
            );
        }
    }
}

fn collect_expanded_matches<F>(
    part: String,
    doc: &TokenizedDoc,
    mut predicate: F,
    out: &mut Vec<HighlightMatch>,
) where
    F: FnMut(&str) -> bool,
{
    for (term, positions) in &doc.positions {
        if !predicate(term) {
            continue;
        }
        for &pos in positions {
            out.push(HighlightMatch {
                part: part.clone(),
                start: pos,
                end: pos,
            });
        }
    }
}

fn expanded_match_result<F>(doc: &TokenizedDoc, predicate: F) -> MatchResult
where
    F: FnMut(&str) -> bool,
{
    let intervals = expanded_positions(doc, predicate)
        .into_iter()
        .map(Interval::point)
        .collect::<Vec<_>>();
    if intervals.is_empty() {
        MatchResult::miss()
    } else {
        MatchResult::hit(intervals)
    }
}

fn expanded_positions<F>(doc: &TokenizedDoc, mut predicate: F) -> Vec<u32>
where
    F: FnMut(&str) -> bool,
{
    let mut positions = Vec::new();
    for (term, term_positions) in &doc.positions {
        if predicate(term) {
            positions.extend(term_positions.iter().copied());
        }
    }
    positions.sort_unstable();
    positions.dedup();
    positions
}

pub fn range_matches(term: &str, lower: &RangeBound, upper: &RangeBound) -> bool {
    let lower_ok = match lower {
        RangeBound::Open => true,
        RangeBound::Term(lower) => term >= lower.as_str(),
    };
    let upper_ok = match upper {
        RangeBound::Open => true,
        RangeBound::Term(upper) => term <= upper.as_str(),
    };
    lower_ok && upper_ok
}

pub struct FuzzyMatcher {
    target_chars: Vec<char>,
    prefix_chars: usize,
    max_distance: usize,
}

impl FuzzyMatcher {
    pub fn new(target: &str, prefix_chars: u32, max_distance: u32) -> Self {
        let target_chars = target.chars().collect::<Vec<_>>();
        let prefix_chars = prefix_chars.min(target_chars.len() as u32) as usize;
        Self {
            target_chars,
            prefix_chars,
            max_distance: max_distance as usize,
        }
    }

    pub fn is_match(&self, candidate: &str) -> bool {
        let candidate_chars = candidate.chars().collect::<Vec<_>>();
        if candidate_chars.len() < self.prefix_chars {
            return false;
        }
        if self.target_chars[..self.prefix_chars] != candidate_chars[..self.prefix_chars] {
            return false;
        }

        edit_distance_with_limit(
            &self.target_chars[self.prefix_chars..],
            &candidate_chars[self.prefix_chars..],
            self.max_distance,
        )
    }
}

fn edit_distance_with_limit(target: &[char], candidate: &[char], max_distance: usize) -> bool {
    let target_len = target.len();
    let candidate_len = candidate.len();
    if target_len.abs_diff(candidate_len) > max_distance {
        return false;
    }

    let mut previous = (0..=candidate_len).collect::<Vec<_>>();
    let mut current = vec![0; candidate_len + 1];

    for (target_idx, target_char) in target.iter().enumerate() {
        current[0] = target_idx + 1;
        let mut row_min = current[0];

        for (candidate_idx, candidate_char) in candidate.iter().enumerate() {
            let substitution_cost = usize::from(target_char != candidate_char);
            let insertion = current[candidate_idx] + 1;
            let deletion = previous[candidate_idx + 1] + 1;
            let substitution = previous[candidate_idx] + substitution_cost;
            let value = insertion.min(deletion).min(substitution);
            current[candidate_idx + 1] = value;
            row_min = row_min.min(value);
        }

        if row_min > max_distance {
            return false;
        }

        std::mem::swap(&mut previous, &mut current);
    }

    previous[candidate_len] <= max_distance
}

/// Emits a highlight mark for every operand occurrence that participates in a
/// satisfying witness tree of the span query.
///
/// `participating` is a span query whose solution set is exactly the subset of
/// `node`'s output intervals that contribute to the overall match. At the root
/// the two coincide. Descending through a positive relation operator keeps the
/// primary side's participating set unchanged (the relation reports the
/// primary side's intervals) and derives the other side's participating set by
/// solving the converse relation against the participating primaries — so only
/// occurrences that actually pair with a contributing primary are marked, and
/// nested relations surface every load-bearing operand.
fn collect_span_marks(
    node: &boldi_vigna::SpanQuery,
    participating: &boldi_vigna::SpanQuery,
    slot_pos: &SlotPositions,
    part: &str,
    out: &mut Vec<HighlightMatch>,
) {
    use boldi_vigna::SpanQuery;
    match node {
        SpanQuery::Before { a, b } => {
            collect_span_marks(a, participating, slot_pos, part, out);
            let witness = SpanQuery::After {
                a: b.clone(),
                b: Box::new(participating.clone()),
            };
            collect_span_marks(b, &witness, slot_pos, part, out);
        }
        SpanQuery::After { a, b } => {
            collect_span_marks(a, participating, slot_pos, part, out);
            let witness = SpanQuery::Before {
                a: b.clone(),
                b: Box::new(participating.clone()),
            };
            collect_span_marks(b, &witness, slot_pos, part, out);
        }
        SpanQuery::Containing { big, little } => {
            collect_span_marks(big, participating, slot_pos, part, out);
            let witness = SpanQuery::ContainedBy {
                little: little.clone(),
                big: Box::new(participating.clone()),
            };
            collect_span_marks(little, &witness, slot_pos, part, out);
        }
        SpanQuery::ContainedBy { little, big } => {
            collect_span_marks(little, participating, slot_pos, part, out);
            let witness = SpanQuery::Containing {
                big: big.clone(),
                little: Box::new(participating.clone()),
            };
            collect_span_marks(big, &witness, slot_pos, part, out);
        }
        SpanQuery::Overlapping { a, b } => {
            collect_span_marks(a, participating, slot_pos, part, out);
            let witness = SpanQuery::Overlapping {
                a: b.clone(),
                b: Box::new(participating.clone()),
            };
            collect_span_marks(b, &witness, slot_pos, part, out);
        }
        // Negative relations report primary-side intervals; the excluded side
        // contributes by absence and has no occurrence to mark.
        SpanQuery::NotContaining { big, .. } => {
            collect_span_marks(big, participating, slot_pos, part, out);
        }
        SpanQuery::NotContainedBy { little, .. } => {
            collect_span_marks(little, participating, slot_pos, part, out);
        }
        SpanQuery::NonOverlapping { a, .. } => {
            collect_span_marks(a, participating, slot_pos, part, out);
        }
        // Filters restrict which outputs participate; `participating` already
        // carries the wrapped (filtered) query, so descend unchanged.
        SpanQuery::MaxGaps { inner, .. }
        | SpanQuery::GapsInRange { inner, .. }
        | SpanQuery::MaxWidth { inner, .. }
        | SpanQuery::WithinPositions { inner, .. } => {
            collect_span_marks(inner, participating, slot_pos, part, out);
        }
        SpanQuery::Empty
        | SpanQuery::Term(_)
        | SpanQuery::Ordered(_)
        | SpanQuery::Unordered(_)
        | SpanQuery::Or(_) => {
            let Ok(mut solver) = SpanSolver::new(participating) else {
                return;
            };
            for iv in solver.intervals(slot_pos) {
                out.push(HighlightMatch {
                    part: part.to_string(),
                    start: iv.start,
                    end: iv.end,
                });
            }
        }
    }
}

fn combine_intervals(left: Vec<Interval>, right: Vec<Interval>) -> Vec<Interval> {
    let mut intervals = left;
    intervals.extend(right);
    normalize_intervals(intervals)
}

fn normalize_intervals(mut intervals: Vec<Interval>) -> Vec<Interval> {
    intervals.sort_unstable_by(interval_cmp);
    intervals.dedup();
    intervals
}

fn interval_cmp(left: &Interval, right: &Interval) -> Ordering {
    left.start.cmp(&right.start).then(left.end.cmp(&right.end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ImplicitOp;
    use tokenizer::presets::default_pipeline;

    fn parse_lower(input: &str) -> Query {
        let expr = crate::parse(input, ImplicitOp::And).expect("query should parse");
        let expr = super::super::subtokenize::sub_tokenize(expr, default_pipeline())
            .expect("query should sub-tokenize");
        super::super::lower::lower(&expr).expect("query should lower")
    }

    fn doc(text: &str) -> TokenizedDoc {
        tokenize_doc(text, default_pipeline())
    }

    // Highlight part labels are user-facing via $QUERY_PART: expansion
    // matches label with tinql SURFACE syntax. A wildcard (normalized to a
    // regex during sub-tokenization) and a written MATCHES both label as
    // `MATCHES <pattern>`, never the `REGEX(..)` IR form.
    #[test]
    fn highlight_labels_regex_expansions_with_surface_syntax() {
        let doc = doc("email emailing post");
        for query in ["email*", "MATCHES email.*"] {
            let matches = evaluate_for_highlight(&parse_lower(query), &doc);
            assert!(!matches.is_empty(), "query {query:?} should match");
            for m in &matches {
                assert_eq!(m.part, "MATCHES email.*", "query {query:?}");
            }
        }
    }

    // BEFORE/AFTER compare span starts (spans may overlap), so same-term
    // operands and a phrase before its own trailing term both match, and the
    // two operators stay exact converses.
    #[test]
    fn before_and_after_use_start_positions() {
        let multi = doc("alpha x alpha");
        assert!(
            evaluate(&parse_lower("alpha BEFORE alpha"), &multi)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("alpha AFTER alpha"), &multi)
                .unwrap()
                .matched
        );

        let single = doc("alpha");
        assert!(
            !evaluate(&parse_lower("alpha BEFORE alpha"), &single)
                .unwrap()
                .matched
        );
        assert!(
            !evaluate(&parse_lower("alpha AFTER alpha"), &single)
                .unwrap()
                .matched
        );

        // Overlap is allowed: the phrase span starts before its own second
        // term's span.
        let overlap = doc("alpha beta");
        assert!(
            evaluate(&parse_lower("\"alpha beta\" BEFORE beta"), &overlap)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("beta AFTER \"alpha beta\""), &overlap)
                .unwrap()
                .matched
        );

        // A later start is still required.
        let reversed = doc("gamma alpha");
        assert!(
            !evaluate(&parse_lower("alpha BEFORE gamma"), &reversed)
                .unwrap()
                .matched
        );
    }

    // Phrase gaps require exactly their number of intervening tokens; slop
    // stays a budget.
    #[test]
    fn phrase_gaps_require_exact_interveners() {
        let gap1 = parse_lower("\"alpha _ gamma\"");
        assert!(evaluate(&gap1, &doc("alpha beta gamma")).unwrap().matched);
        assert!(!evaluate(&gap1, &doc("alpha gamma")).unwrap().matched);
        assert!(!evaluate(&gap1, &doc("alpha x y gamma")).unwrap().matched);

        let gap2 = parse_lower("\"alpha __ gamma\"");
        assert!(evaluate(&gap2, &doc("alpha x y gamma")).unwrap().matched);
        assert!(!evaluate(&gap2, &doc("alpha beta gamma")).unwrap().matched);

        // Each gap is pinned at its own slot.
        let pinned = parse_lower("\"a _ b __ c\"");
        assert!(evaluate(&pinned, &doc("a x b y z c")).unwrap().matched);
        assert!(!evaluate(&pinned, &doc("a x y b z c")).unwrap().matched);

        let slop1 = parse_lower("\"alpha gamma\"~1");
        assert!(evaluate(&slop1, &doc("alpha gamma")).unwrap().matched);
        assert!(evaluate(&slop1, &doc("alpha beta gamma")).unwrap().matched);
    }

    #[test]
    fn queries_never_match_a_tokenless_document() {
        let empty = doc("   ");

        assert!(!evaluate(&Query::MatchAll, &empty).unwrap().matched);
        assert!(
            !evaluate(&Query::Not(Box::new(Query::Term("apple".into()))), &empty)
                .unwrap()
                .matched
        );
    }

    #[test]
    fn term_phrase_and_boolean_queries_match() {
        let doc = doc("craft beer lovers and beer makers");
        assert!(evaluate(&parse_lower("craft"), &doc).unwrap().matched);
        assert!(
            evaluate(&parse_lower("\"craft beer\""), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("craft AND makers"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            !evaluate(&parse_lower("craft AND wine"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("craft AND NOT wine"), &doc)
                .unwrap()
                .matched
        );
    }

    #[test]
    fn proximity_and_relations_match() {
        let doc = doc("craft beer festival welcomes local beer fans");
        assert!(
            evaluate(&parse_lower("craft THEN/1 festival"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("festival AFTER craft"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("craft NEAR/2 beer"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("\"craft beer\" ENCLOSES beer"), &doc)
                .unwrap()
                .matched
        );
    }

    #[test]
    fn positional_filters_and_at_least_match() {
        let doc = doc("alpha beta gamma delta epsilon");
        assert!(
            evaluate(&parse_lower("alpha IN FIRST 2 WORDS"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("beta IN WORDS 1 TO 2"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("epsilon IN LAST 2 WORDS"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("alpha IN FIRST 25%"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("gamma IN MIDDLE 50%"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("AT LEAST 2 OF [alpha, beta, zeta]"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            !evaluate(&parse_lower("AT LEAST 3 OF [alpha, beta, zeta]"), &doc)
                .unwrap()
                .matched
        );
    }

    #[test]
    fn nested_runtime_filters_and_span_context_at_least_match() {
        let doc = doc("alpha beta gamma delta epsilon zeta");
        assert!(
            evaluate(&parse_lower("(beta IN FIRST 50%) BEFORE epsilon"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(&parse_lower("alpha BEFORE (zeta IN LAST 25%)"), &doc)
                .unwrap()
                .matched
        );
        assert!(
            evaluate(
                &parse_lower("(AT LEAST 2 OF [beta, gamma, zeta]) WITHIN 3"),
                &doc
            )
            .unwrap()
            .matched
        );
        assert!(
            !evaluate(
                &parse_lower("(AT LEAST 3 OF [beta, gamma, zeta]) WITHIN 2"),
                &doc
            )
            .unwrap()
            .matched
        );
    }

    #[test]
    fn boost_does_not_change_matching_rows() {
        let doc = doc("craft beer");
        let plain = evaluate(&parse_lower("\"craft beer\""), &doc).unwrap();
        let boosted = evaluate(&parse_lower("\"craft beer\"^2"), &doc).unwrap();
        assert_eq!(plain.matched, boosted.matched);
        assert_eq!(plain.intervals, boosted.intervals);
    }

    #[test]
    fn evaluator_expands_query_terms_against_the_document() {
        let doc = doc("brew brewer beer bear cat cot dog");

        for query in ["brew*", "MATCHES b.*r", "cat TO cot", "beer~1"] {
            let result = evaluate(&parse_lower(query), &doc).unwrap();
            assert!(result.matched, "query={query}");
        }

        let result = evaluate(&parse_lower("kombucha*"), &doc).unwrap();
        assert!(!result.matched);
    }

    #[test]
    fn evaluator_expands_span_slots_against_the_document() {
        let doc = doc("brew beer brewer beer stale");
        let result = evaluate(&parse_lower("brew* THEN/1 beer"), &doc).unwrap();

        assert_eq!(
            result.intervals,
            vec![Interval::new(0, 1), Interval::new(2, 3)]
        );
    }

    #[test]
    fn highlight_expands_query_terms_against_the_document() {
        let doc = doc("brew brewer beer bear cat cot dog");
        let query = parse_lower("brew* OR MATCHES b.*r OR cat TO cot OR beer~1");

        let mut positions = evaluate_for_highlight(&query, &doc)
            .into_iter()
            .map(|m| m.start)
            .collect::<Vec<_>>();
        positions.sort_unstable();
        positions.dedup();

        assert_eq!(positions, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn highlight_expands_span_slots_against_the_document() {
        let doc = doc("brew beer brewer beer stale");
        let query = parse_lower("brew* THEN/1 beer");

        let intervals = evaluate_for_highlight(&query, &doc)
            .into_iter()
            .map(|m| (m.start, m.end))
            .collect::<Vec<_>>();

        assert_eq!(intervals, vec![(0, 1), (2, 3)]);
    }

    #[test]
    fn snippets_follow_intervals() {
        let doc = doc("zero one two three four");
        let result = evaluate(&parse_lower("two"), &doc).unwrap();
        assert_eq!(doc.snippet(result.intervals[0], 1), "one two three");
    }
}
