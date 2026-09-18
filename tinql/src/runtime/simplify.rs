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
//! Query simplification for lowered tinql runtime queries.
//!
//! This module owns two simplification profiles over [`Query`]:
//!
//! - [`SimplificationProfile::Structural`] keeps the old lowering-time work:
//!   flattening boolean chains, deduplicating flat term siblings, folding
//!   `MatchAll`, and collapsing one-child boolean nodes
//! - [`SimplificationProfile::LogicalUnscored`] adds implication-based
//!   absorption for positive unscored trees so planner-time display and
//!   unscored execution can both reuse the same backend-agnostic rewrites
//!
//! arXiv notes:
//!
//! - Koehler et al., "Sketch-Guided Equality Saturation" (arXiv:2111.13040)
//!   influenced the choice to keep rewrite search tightly scoped. We use the
//!   lesson that rewrite spaces need strong control, but we do not adopt
//!   equality saturation, extraction, or sketches here because this pass only
//!   needs a handful of deterministic local rewrites.
//! - Yin et al., "BoolE: Exact Symbolic Reasoning via Boolean Equality
//!   Saturation" (arXiv:2504.05577) reinforced that exact boolean reasoning
//!   benefits from domain-specific rules. We keep that part, but not the
//!   symbolic eqsat machinery, because the tinql runtime IR is already tiny
//!   and we don't need global equivalence search.
//! - Averkov et al., "Simplifier: A New Tool for Boolean Circuit
//!   Simplification" (arXiv:2503.19103) is relevant as a modern example of
//!   aggressive exact simplification. We do not use its circuit-level search or
//!   library-style simplification strategy because this feature only needs
//!   implication-based absorption on one internal query IR boundary.
//! - Chen et al., "E-morphic: Scalable Equality Saturation for Structural
//!   Exploration in Logic Synthesis" (arXiv:2504.11574) is useful as a recent
//!   example of scaling eqsat with pruning and custom extraction. We do not use
//!   that machinery here because tin is not exploring a broad alternative plan
//!   space at this boundary, it is just deleting a small set of provably
//!   redundant unscored subtrees.
//!
//! Future work:
//!
//! - `NOT` remains a negation boundary. We simplify inside negated subtrees,
//!   but we don't do implication reasoning across the negative edge yet.

use super::{Query, SpanExpr, SpanTermSlot};
use rustc_hash::FxHashSet;

#[derive(Clone, Copy)]
pub enum SimplificationProfile {
    Structural,
    StructuralPreserveTermMultiplicity,
    LogicalUnscored,
}

pub fn simplify(query: Query, profile: SimplificationProfile) -> Query {
    let query = normalize_boolean_query(query, profile);
    match profile {
        SimplificationProfile::Structural
        | SimplificationProfile::StructuralPreserveTermMultiplicity => query,
        SimplificationProfile::LogicalUnscored => reduce_unscored_redundancy(query),
    }
}

impl SimplificationProfile {
    const fn dedup_flat_terms(self) -> bool {
        match self {
            Self::Structural | Self::LogicalUnscored => true,
            Self::StructuralPreserveTermMultiplicity => false,
        }
    }
}

pub fn all_terms_required_span_query(span_query: &boldi_vigna::SpanQuery) -> bool {
    match span_query {
        boldi_vigna::SpanQuery::Empty => false,
        boldi_vigna::SpanQuery::Term(_) => true,
        boldi_vigna::SpanQuery::Ordered(children) | boldi_vigna::SpanQuery::Unordered(children) => {
            children.iter().all(all_terms_required_span_query)
        }
        boldi_vigna::SpanQuery::Or(_) => false,
        boldi_vigna::SpanQuery::MaxGaps { inner, .. }
        | boldi_vigna::SpanQuery::GapsInRange { inner, .. }
        | boldi_vigna::SpanQuery::MaxWidth { inner, .. }
        | boldi_vigna::SpanQuery::WithinPositions { inner, .. } => {
            all_terms_required_span_query(inner)
        }
        boldi_vigna::SpanQuery::Containing { big, little }
        | boldi_vigna::SpanQuery::ContainedBy {
            little: big,
            big: little,
        }
        | boldi_vigna::SpanQuery::Overlapping { a: big, b: little }
        | boldi_vigna::SpanQuery::Before { a: big, b: little }
        | boldi_vigna::SpanQuery::After { a: big, b: little } => {
            all_terms_required_span_query(big) && all_terms_required_span_query(little)
        }
        boldi_vigna::SpanQuery::NotContaining { .. }
        | boldi_vigna::SpanQuery::NotContainedBy { .. }
        | boldi_vigna::SpanQuery::NonOverlapping { .. } => false,
    }
}

pub fn all_terms_required_span_expr(span_expr: &SpanExpr) -> bool {
    span_expr.all_terms_required()
}

fn reduce_unscored_redundancy(query: Query) -> Query {
    match query {
        Query::And(left, right) => reduce_conjunction(vec![
            reduce_unscored_redundancy(*left),
            reduce_unscored_redundancy(*right),
        ]),
        Query::Conjunction(children) => reduce_conjunction(
            children
                .into_iter()
                .map(reduce_unscored_redundancy)
                .collect(),
        ),
        Query::Or(left, right) => reduce_disjunction(
            1,
            vec![
                reduce_unscored_redundancy(*left),
                reduce_unscored_redundancy(*right),
            ],
        ),
        Query::Disjunction { min, children } => {
            let children = children
                .into_iter()
                .map(reduce_unscored_redundancy)
                .collect();
            if min == 1 {
                reduce_disjunction(1, children)
            } else {
                normalize_disjunction(min, children, SimplificationProfile::LogicalUnscored)
            }
        }
        Query::Not(inner) => simplify_not(reduce_unscored_redundancy(*inner)),
        Query::Boost { factor, inner } => {
            simplify_boost(factor, reduce_unscored_redundancy(*inner))
        }
        Query::AtLeast { min, children } => Query::AtLeast {
            min,
            children: children
                .into_iter()
                .map(reduce_unscored_redundancy)
                .collect(),
        },
        other => other,
    }
}

fn reduce_conjunction(children: Vec<Query>) -> Query {
    let mut flat = Vec::new();
    for child in children {
        collect_conjuncts(child, &mut flat);
    }

    let mut keep = vec![true; flat.len()];
    for absorber_idx in 0..flat.len() {
        if !keep[absorber_idx] || !can_participate_in_positive_implication(&flat[absorber_idx]) {
            continue;
        }

        for candidate_idx in 0..flat.len() {
            if absorber_idx == candidate_idx
                || !keep[candidate_idx]
                || !can_participate_in_positive_implication(&flat[candidate_idx])
            {
                continue;
            }

            if implies(&flat[absorber_idx], &flat[candidate_idx]) {
                keep[candidate_idx] = false;
            }
        }
    }

    retain_kept(&mut flat, &keep);
    normalize_conjunction(flat, SimplificationProfile::LogicalUnscored)
}

fn reduce_disjunction(min: u32, children: Vec<Query>) -> Query {
    let mut flat = Vec::new();
    for child in children {
        collect_plain_disjuncts(child, &mut flat);
    }

    let mut keep = vec![true; flat.len()];
    for absorber_idx in 0..flat.len() {
        if !keep[absorber_idx] || !can_participate_in_positive_implication(&flat[absorber_idx]) {
            continue;
        }

        for candidate_idx in 0..flat.len() {
            if absorber_idx == candidate_idx
                || !keep[candidate_idx]
                || !can_participate_in_positive_implication(&flat[candidate_idx])
            {
                continue;
            }

            if implies(&flat[candidate_idx], &flat[absorber_idx]) {
                keep[candidate_idx] = false;
            }
        }
    }

    retain_kept(&mut flat, &keep);
    normalize_disjunction(min, flat, SimplificationProfile::LogicalUnscored)
}

fn retain_kept(children: &mut Vec<Query>, keep: &[bool]) {
    let mut idx = 0;
    children.retain(|_| {
        let keep_child = keep[idx];
        idx += 1;
        keep_child
    });
}

fn collect_conjuncts(query: Query, out: &mut Vec<Query>) {
    match query {
        Query::And(left, right) => {
            collect_conjuncts(*left, out);
            collect_conjuncts(*right, out);
        }
        Query::Conjunction(children) => {
            for child in children {
                collect_conjuncts(child, out);
            }
        }
        other => out.push(other),
    }
}

fn collect_plain_disjuncts(query: Query, out: &mut Vec<Query>) {
    match query {
        Query::Or(left, right) => {
            collect_plain_disjuncts(*left, out);
            collect_plain_disjuncts(*right, out);
        }
        Query::Disjunction { min: 1, children } => {
            for child in children {
                collect_plain_disjuncts(child, out);
            }
        }
        other => out.push(other),
    }
}

fn can_participate_in_positive_implication(query: &Query) -> bool {
    positive_root(query).is_some()
}

fn positive_root(query: &Query) -> Option<&Query> {
    match query {
        Query::Boost { inner, .. } => positive_root(inner),
        Query::Not(_) => None,
        other => Some(other),
    }
}

fn implies(lhs: &Query, rhs: &Query) -> bool {
    if lhs == rhs {
        return true;
    }

    if is_empty_query(lhs) {
        return true;
    }

    if is_empty_query(rhs) {
        return is_empty_query(lhs);
    }

    match rhs {
        Query::MatchAll => return true,
        Query::Term(term) => return query_implies_term(lhs, term),
        Query::And(left, right) => return implies(lhs, left) && implies(lhs, right),
        Query::Conjunction(children) => return children.iter().all(|child| implies(lhs, child)),
        Query::Or(left, right) => return implies(lhs, left) || implies(lhs, right),
        Query::Disjunction { min: 1, children } => {
            return children.iter().any(|child| implies(lhs, child));
        }
        Query::Boost { inner, .. } => return implies(lhs, inner),
        Query::Span {
            term_slots,
            span_query,
            position_filter,
        } => return query_implies_span(lhs, term_slots, span_query, position_filter.as_ref()),
        Query::SpanExpr {
            term_slots,
            span_expr,
        } => return query_implies_span_expr(lhs, term_slots, span_expr),
        Query::Disjunction { .. }
        | Query::Not(_)
        | Query::Regex(_)
        | Query::Range { .. }
        | Query::Fuzzy { .. }
        | Query::AtLeast { .. } => {}
    }

    match lhs {
        Query::And(left, right) => implies(left, rhs) || implies(right, rhs),
        Query::Conjunction(children) => children.iter().any(|child| implies(child, rhs)),
        Query::Or(left, right) => implies(left, rhs) && implies(right, rhs),
        Query::Disjunction { children, .. } => children.iter().all(|child| implies(child, rhs)),
        Query::Boost { inner, .. } => implies(inner, rhs),
        _ => false,
    }
}

fn query_implies_term(query: &Query, rhs_term: &str) -> bool {
    if is_empty_query(query) {
        return true;
    }

    match query {
        Query::Term(term) => term == rhs_term,
        Query::And(left, right) => {
            query_implies_term(left, rhs_term) || query_implies_term(right, rhs_term)
        }
        Query::Conjunction(children) => children
            .iter()
            .any(|child| query_implies_term(child, rhs_term)),
        Query::Or(left, right) => {
            query_implies_term(left, rhs_term) && query_implies_term(right, rhs_term)
        }
        Query::Disjunction { children, .. } => children
            .iter()
            .all(|child| query_implies_term(child, rhs_term)),
        Query::Span {
            term_slots,
            span_query,
            ..
        } => {
            all_terms_required_span_query(span_query)
                && span_query_references_term(span_query, term_slots, rhs_term)
        }
        Query::SpanExpr {
            term_slots,
            span_expr,
        } => {
            all_terms_required_span_expr(span_expr)
                && span_expr_references_term(span_expr, term_slots, rhs_term)
        }
        Query::Boost { inner, .. } => query_implies_term(inner, rhs_term),
        Query::MatchAll
        | Query::Not(_)
        | Query::Regex(_)
        | Query::Range { .. }
        | Query::Fuzzy { .. }
        | Query::AtLeast { .. } => false,
    }
}

fn query_implies_span(
    query: &Query,
    rhs_slots: &[SpanTermSlot],
    rhs_span_query: &boldi_vigna::SpanQuery,
    rhs_position_filter: Option<&super::SpanPositionFilter>,
) -> bool {
    match query {
        Query::Span {
            term_slots,
            span_query,
            position_filter,
        } => {
            term_slots == rhs_slots
                && span_query == rhs_span_query
                && (position_filter.as_ref() == rhs_position_filter
                    || rhs_position_filter.is_none())
        }
        Query::And(left, right) => {
            query_implies_span(left, rhs_slots, rhs_span_query, rhs_position_filter)
                || query_implies_span(right, rhs_slots, rhs_span_query, rhs_position_filter)
        }
        Query::Conjunction(children) => children
            .iter()
            .any(|child| query_implies_span(child, rhs_slots, rhs_span_query, rhs_position_filter)),
        Query::Or(left, right) => {
            query_implies_span(left, rhs_slots, rhs_span_query, rhs_position_filter)
                && query_implies_span(right, rhs_slots, rhs_span_query, rhs_position_filter)
        }
        Query::Disjunction { children, .. } => children
            .iter()
            .all(|child| query_implies_span(child, rhs_slots, rhs_span_query, rhs_position_filter)),
        Query::Boost { inner, .. } => {
            query_implies_span(inner, rhs_slots, rhs_span_query, rhs_position_filter)
        }
        _ => false,
    }
}

fn query_implies_span_expr(
    query: &Query,
    rhs_slots: &[SpanTermSlot],
    rhs_span_expr: &SpanExpr,
) -> bool {
    match query {
        Query::SpanExpr {
            term_slots,
            span_expr,
        } => term_slots == rhs_slots && span_expr == rhs_span_expr,
        Query::And(left, right) => {
            query_implies_span_expr(left, rhs_slots, rhs_span_expr)
                || query_implies_span_expr(right, rhs_slots, rhs_span_expr)
        }
        Query::Conjunction(children) => children
            .iter()
            .any(|child| query_implies_span_expr(child, rhs_slots, rhs_span_expr)),
        Query::Or(left, right) => {
            query_implies_span_expr(left, rhs_slots, rhs_span_expr)
                && query_implies_span_expr(right, rhs_slots, rhs_span_expr)
        }
        Query::Disjunction { children, .. } => children
            .iter()
            .all(|child| query_implies_span_expr(child, rhs_slots, rhs_span_expr)),
        Query::Boost { inner, .. } => query_implies_span_expr(inner, rhs_slots, rhs_span_expr),
        _ => false,
    }
}

fn span_query_references_term(
    span_query: &boldi_vigna::SpanQuery,
    term_slots: &[SpanTermSlot],
    rhs_term: &str,
) -> bool {
    match span_query {
        boldi_vigna::SpanQuery::Empty => false,
        boldi_vigna::SpanQuery::Term(idx) => match term_slots.get(*idx) {
            Some(SpanTermSlot::Term(term)) => term == rhs_term,
            Some(_) => false,
            None => panic!(
                "span query referenced term slot {} but only {} slots were lowered",
                idx,
                term_slots.len()
            ),
        },
        boldi_vigna::SpanQuery::Ordered(children) | boldi_vigna::SpanQuery::Unordered(children) => {
            children
                .iter()
                .any(|child| span_query_references_term(child, term_slots, rhs_term))
        }
        boldi_vigna::SpanQuery::Or(children) => children
            .iter()
            .any(|child| span_query_references_term(child, term_slots, rhs_term)),
        boldi_vigna::SpanQuery::MaxGaps { inner, .. }
        | boldi_vigna::SpanQuery::GapsInRange { inner, .. }
        | boldi_vigna::SpanQuery::MaxWidth { inner, .. }
        | boldi_vigna::SpanQuery::WithinPositions { inner, .. } => {
            span_query_references_term(inner, term_slots, rhs_term)
        }
        boldi_vigna::SpanQuery::Containing { big, little }
        | boldi_vigna::SpanQuery::ContainedBy {
            little: big,
            big: little,
        }
        | boldi_vigna::SpanQuery::Overlapping { a: big, b: little }
        | boldi_vigna::SpanQuery::Before { a: big, b: little }
        | boldi_vigna::SpanQuery::After { a: big, b: little }
        | boldi_vigna::SpanQuery::NotContaining { big, little }
        | boldi_vigna::SpanQuery::NotContainedBy {
            little: big,
            big: little,
        }
        | boldi_vigna::SpanQuery::NonOverlapping { a: big, b: little } => {
            span_query_references_term(big, term_slots, rhs_term)
                || span_query_references_term(little, term_slots, rhs_term)
        }
    }
}

fn span_expr_references_term(
    span_expr: &SpanExpr,
    term_slots: &[SpanTermSlot],
    rhs_term: &str,
) -> bool {
    match span_expr {
        SpanExpr::Empty => false,
        SpanExpr::Term(idx) => match term_slots.get(*idx) {
            Some(SpanTermSlot::Term(term)) => term == rhs_term,
            Some(_) => false,
            None => panic!(
                "span expr referenced term slot {} but only {} slots were lowered",
                idx,
                term_slots.len()
            ),
        },
        SpanExpr::Ordered(children) | SpanExpr::Unordered(children) | SpanExpr::Or(children) => {
            children
                .iter()
                .any(|child| span_expr_references_term(child, term_slots, rhs_term))
        }
        SpanExpr::AtLeast { children, .. } => children
            .iter()
            .any(|child| span_expr_references_term(child, term_slots, rhs_term)),
        SpanExpr::MaxGaps { inner, .. }
        | SpanExpr::GapsInRange { inner, .. }
        | SpanExpr::MaxWidth { inner, .. }
        | SpanExpr::WithinPositions { inner, .. }
        | SpanExpr::PositionFilter { inner, .. } => {
            span_expr_references_term(inner, term_slots, rhs_term)
        }
        SpanExpr::Containing { big, little }
        | SpanExpr::ContainedBy {
            little: big,
            big: little,
        }
        | SpanExpr::Overlapping { a: big, b: little }
        | SpanExpr::Before { a: big, b: little }
        | SpanExpr::After { a: big, b: little }
        | SpanExpr::NotContaining { big, little }
        | SpanExpr::NotContainedBy {
            little: big,
            big: little,
        }
        | SpanExpr::NonOverlapping { a: big, b: little } => {
            span_expr_references_term(big, term_slots, rhs_term)
                || span_expr_references_term(little, term_slots, rhs_term)
        }
    }
}

fn empty_query() -> Query {
    Query::Not(Box::new(Query::MatchAll))
}

fn is_empty_query(query: &Query) -> bool {
    matches!(query, Query::Not(inner) if inner.as_ref() == &Query::MatchAll)
}

fn simplify_not(inner: Query) -> Query {
    if is_empty_query(&inner) {
        Query::MatchAll
    } else if inner == Query::MatchAll {
        empty_query()
    } else if let Query::Not(inner) = inner {
        *inner
    } else {
        Query::Not(Box::new(inner))
    }
}

fn simplify_boost(factor: f32, inner: Query) -> Query {
    Query::Boost {
        factor,
        inner: Box::new(inner),
    }
}

fn normalize_boolean_query(query: Query, profile: SimplificationProfile) -> Query {
    match query {
        Query::And(left, right) => normalize_conjunction(vec![*left, *right], profile),
        Query::Conjunction(children) => normalize_conjunction(children, profile),
        Query::Or(left, right) => normalize_disjunction(1, vec![*left, *right], profile),
        Query::Disjunction { min, children } => normalize_disjunction(min, children, profile),
        Query::Not(inner) => simplify_not(normalize_boolean_query(*inner, profile)),
        Query::Boost { factor, inner } => {
            simplify_boost(factor, normalize_boolean_query(*inner, profile))
        }
        Query::AtLeast { min, children } => Query::AtLeast {
            min,
            children: children
                .into_iter()
                .map(|child| normalize_boolean_query(child, profile))
                .collect(),
        },
        other => other,
    }
}

fn normalize_conjunction(children: Vec<Query>, profile: SimplificationProfile) -> Query {
    let mut flat = Vec::new();
    for child in children {
        push_conjunction_child(normalize_boolean_query(child, profile), &mut flat);
    }

    if flat.iter().any(is_empty_query) {
        return empty_query();
    }

    flat.retain(|child| !matches!(child, Query::MatchAll));
    if profile.dedup_flat_terms() {
        flat = dedup_flat_terms(flat);
    }
    fold_conjunction(flat)
}

fn push_conjunction_child(child: Query, out: &mut Vec<Query>) {
    match child {
        Query::And(left, right) => {
            push_conjunction_child(*left, out);
            push_conjunction_child(*right, out);
        }
        Query::Conjunction(children) => {
            for child in children {
                push_conjunction_child(child, out);
            }
        }
        other => out.push(other),
    }
}

fn normalize_disjunction(min: u32, children: Vec<Query>, profile: SimplificationProfile) -> Query {
    let mut flat = Vec::new();
    let mut match_all_children = 0_u32;

    for child in children {
        let child = normalize_boolean_query(child, profile);
        if matches!(child, Query::MatchAll)
            && !matches!(
                profile,
                SimplificationProfile::StructuralPreserveTermMultiplicity
            )
        {
            match_all_children += 1;
            continue;
        }
        if is_empty_query(&child) {
            continue;
        }
        push_disjunction_child(min, child, &mut flat);
    }

    let min = min.saturating_sub(match_all_children);
    if min == 0 {
        return Query::MatchAll;
    }

    if min as usize > flat.len() {
        return empty_query();
    }

    let children = if min == 1 && profile.dedup_flat_terms() {
        dedup_flat_terms(flat)
    } else {
        flat
    };

    if min as usize == children.len() {
        return fold_conjunction(children);
    }

    match children.len() {
        0 => empty_query(),
        1 if min == 1 => children.into_iter().next().expect("length checked above"),
        _ => Query::Disjunction { min, children },
    }
}

fn push_disjunction_child(parent_min: u32, child: Query, out: &mut Vec<Query>) {
    match child {
        Query::Or(left, right) if parent_min == 1 => {
            push_disjunction_child(1, *left, out);
            push_disjunction_child(1, *right, out);
        }
        Query::Disjunction { min, children } if parent_min == 1 && min == 1 => {
            for child in children {
                push_disjunction_child(1, child, out);
            }
        }
        other => out.push(other),
    }
}

fn dedup_flat_terms(children: Vec<Query>) -> Vec<Query> {
    let mut seen = FxHashSet::default();
    children
        .into_iter()
        .filter(|child| match child {
            Query::Term(term) => seen.insert(term.clone()),
            _ => true,
        })
        .collect()
}

fn fold_conjunction(mut children: Vec<Query>) -> Query {
    match children.len() {
        0 => Query::MatchAll,
        1 => children.pop().expect("length checked above"),
        _ => Query::Conjunction(children),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_simplify_flattens_and_dedups_terms() {
        let query = Query::And(
            Box::new(Query::Term("beer".into())),
            Box::new(Query::Conjunction(vec![
                Query::Term("wine".into()),
                Query::Term("beer".into()),
            ])),
        );

        assert_eq!(
            simplify(query, SimplificationProfile::Structural),
            Query::Conjunction(vec![Query::Term("beer".into()), Query::Term("wine".into())]),
        );
    }

    #[test]
    fn multiplicity_preserving_simplify_keeps_duplicate_terms() {
        let query = Query::And(
            Box::new(Query::Term("to".into())),
            Box::new(Query::Conjunction(vec![
                Query::Term("be".into()),
                Query::Term("to".into()),
            ])),
        );

        assert_eq!(
            simplify(
                query,
                SimplificationProfile::StructuralPreserveTermMultiplicity
            ),
            Query::Conjunction(vec![
                Query::Term("to".into()),
                Query::Term("be".into()),
                Query::Term("to".into()),
            ]),
        );
    }

    #[test]
    fn multiplicity_preserving_simplify_keeps_scored_or_child_with_match_all() {
        let query = Query::Disjunction {
            min: 1,
            children: vec![Query::MatchAll, Query::Term("beer".into())],
        };

        assert_eq!(
            simplify(
                query.clone(),
                SimplificationProfile::StructuralPreserveTermMultiplicity
            ),
            query,
        );
    }

    #[test]
    fn logical_unscored_absorbs_weaker_or_child() {
        let query = Query::Conjunction(vec![
            Query::Term("beer".into()),
            Query::Disjunction {
                min: 1,
                children: vec![Query::Term("beer".into()), Query::Term("wine".into())],
            },
        ]);

        assert_eq!(
            simplify(query, SimplificationProfile::LogicalUnscored),
            Query::Term("beer".into()),
        );
    }

    #[test]
    fn logical_unscored_absorbs_all_required_span_into_or() {
        let query = Query::Disjunction {
            min: 1,
            children: vec![
                Query::Conjunction(vec![Query::Term("the".into()), Query::Term("who".into())]),
                Query::Span {
                    term_slots: vec![
                        SpanTermSlot::Term("the".into()),
                        SpanTermSlot::Term("who".into()),
                    ],
                    span_query: boldi_vigna::SpanQuery::MaxGaps {
                        max_gaps: 0,
                        inner: Box::new(boldi_vigna::SpanQuery::Ordered(vec![
                            boldi_vigna::SpanQuery::Term(0),
                            boldi_vigna::SpanQuery::Term(1),
                        ])),
                    },
                    position_filter: None,
                },
            ],
        };

        assert_eq!(
            simplify(query, SimplificationProfile::LogicalUnscored),
            Query::Conjunction(vec![Query::Term("the".into()), Query::Term("who".into())]),
        );
    }

    #[test]
    fn logical_unscored_does_not_cross_negative_edge() {
        let query = Query::Conjunction(vec![
            Query::Term("beer".into()),
            Query::Not(Box::new(Query::Disjunction {
                min: 1,
                children: vec![Query::Term("beer".into()), Query::Term("wine".into())],
            })),
        ]);

        assert_eq!(
            simplify(query, SimplificationProfile::LogicalUnscored),
            Query::Conjunction(vec![
                Query::Term("beer".into()),
                Query::Not(Box::new(Query::Disjunction {
                    min: 1,
                    children: vec![Query::Term("beer".into()), Query::Term("wine".into())],
                })),
            ]),
        );
    }

    #[test]
    fn logical_unscored_ignores_unused_span_slots() {
        let query = Query::Disjunction {
            min: 1,
            children: vec![
                Query::Span {
                    term_slots: vec![
                        SpanTermSlot::Term("a".into()),
                        SpanTermSlot::Term("c".into()),
                    ],
                    span_query: boldi_vigna::SpanQuery::Term(0),
                    position_filter: None,
                },
                Query::Term("c".into()),
            ],
        };

        assert_eq!(
            simplify(query, SimplificationProfile::LogicalUnscored),
            Query::Disjunction {
                min: 1,
                children: vec![
                    Query::Span {
                        term_slots: vec![
                            SpanTermSlot::Term("a".into()),
                            SpanTermSlot::Term("c".into()),
                        ],
                        span_query: boldi_vigna::SpanQuery::Term(0),
                        position_filter: None,
                    },
                    Query::Term("c".into()),
                ],
            },
        );
    }
}
