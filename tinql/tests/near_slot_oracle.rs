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
//! tinql's evaluator against an independent brute-force NEAR oracle, for slots
//! that are OR groups and may share a word with the other slot.
//!
//! The oracle tokenizes the text with the tokenizer crate's default pipeline,
//! takes each slot's positions directly from the tokens, and enumerates every
//! pair of positions, one per slot.
//!
//! Semantics implemented (book `tinql/proximity.md`, and the span solver's
//! `SpanQuery` contract for what the book leaves open):
//! - `NEAR/n` is unordered: either slot may come first.
//! - Distance: single-word slots at positions `p` and `q` match when at most
//!   `n` positions lie strictly between them, `|p - q| - 1 <= n`.
//! - Same-position reuse: the book does not say whether both slots may use one
//!   occurrence. The solver's contract does (`MaxGaps` passes no overlapping
//!   sub-intervals), and the shipped `alpha NEAR/3 alpha` already requires two
//!   distinct occurrences. The oracle follows that: `p != q`.

use tinql::runtime::{evaluate, lower::lower, subtokenize::sub_tokenize, tokenize_doc};
use tinql::{ImplicitOp, parse};
use tokenizer::Tokenizer;
use tokenizer::presets::default_pipeline;

fn engine_matches(query: &str, text: &str) -> bool {
    let expr = parse(query, ImplicitOp::And).expect("query parses");
    let expr = sub_tokenize(expr, default_pipeline()).expect("query sub-tokenizes");
    let query = lower(&expr).expect("query lowers");
    evaluate(&query, &tokenize_doc(text, default_pipeline()))
        .expect("query evaluates")
        .matched
}

/// The single token the default pipeline makes of a slot word.
fn normalize(word: &str) -> String {
    let mut tokens = default_pipeline().tokenize(word);
    let token = tokens
        .next()
        .expect("slot word tokenizes")
        .text
        .into_owned();
    assert!(tokens.next().is_none(), "slot word {word:?} is one token");
    token
}

fn oracle_matches(left: &[&str], right: &[&str], max_gaps: u32, text: &str) -> bool {
    let tokens: Vec<(String, u32)> = default_pipeline()
        .tokenize(text)
        .map(|token| (token.text.into_owned(), token.pos))
        .collect();
    let slot_positions = |slot: &[&str]| -> Vec<u32> {
        let words: Vec<String> = slot.iter().map(|w| normalize(w)).collect();
        tokens
            .iter()
            .filter(|(token, _)| words.contains(token))
            .map(|&(_, pos)| pos)
            .collect()
    };
    let (left, right) = (slot_positions(left), slot_positions(right));
    left.iter().any(|&p| {
        right
            .iter()
            .any(|&q| p != q && p.abs_diff(q) - 1 <= max_gaps)
    })
}

fn slot_text(slot: &[&str]) -> String {
    match slot {
        [word] => (*word).to_owned(),
        words => format!("({})", words.join(" OR ")),
    }
}

fn near_query(left: &[&str], right: &[&str], max_gaps: u32) -> String {
    format!("{} NEAR/{max_gaps} {}", slot_text(left), slot_text(right))
}

/// Query, left slot, right slot, gap, text.
type Row = (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    u32,
    &'static str,
);

/// The M5c span-fixture queries, on texts that contain a match.
#[test]
fn group_slots_sharing_a_word_match_the_oracle() {
    let rows: &[Row] = &[
        (
            "alpha NEAR/3 alpha",
            &["alpha"],
            &["alpha"],
            3,
            "alpha of the alpha",
        ),
        (
            "(alpha OR bravo) NEAR/3 alpha",
            &["alpha", "bravo"],
            &["alpha"],
            3,
            "alpha of the alpha",
        ),
        (
            "[alpha bravo] NEAR/2 alpha",
            &["alpha", "bravo"],
            &["alpha"],
            2,
            "the alpha of alpha",
        ),
        (
            "alpha NEAR/2 alpha",
            &["alpha"],
            &["alpha"],
            2,
            "alpha of the alpha",
        ),
        (
            "(alpha OR bravo) NEAR/2 (alpha OR charlie)",
            &["alpha", "bravo"],
            &["alpha", "charlie"],
            2,
            "alpha the alpha",
        ),
        (
            "(alpha OR bravo) NEAR/2 (alpha OR charlie)",
            &["alpha", "bravo"],
            &["alpha", "charlie"],
            2,
            "charlie of the bravo alpha",
        ),
    ];
    let mut failures = Vec::new();
    for &(query, left, right, max_gaps, text) in rows {
        let expected = oracle_matches(left, right, max_gaps, text);
        let actual = engine_matches(query, text);
        if actual != expected {
            failures.push(format!(
                "{query:?} on {text:?}: tinql {actual}, oracle {expected}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn one_shared_occurrence_is_not_a_pair() {
    assert!(!engine_matches(
        "(alpha OR bravo) NEAR/3 alpha",
        "the alpha of"
    ));
    assert!(!oracle_matches(
        &["alpha", "bravo"],
        &["alpha"],
        3,
        "the alpha of"
    ));
}

/// xorshift64*: deterministic generated cases without a test dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const SLOT_WORDS: [&str; 4] = ["alpha", "bravo", "charlie", "delta"];
const FILLER: [&str; 3] = ["the", "of", "echo"];

fn generated_text(rng: &mut Rng) -> String {
    let len = 1 + rng.below(12);
    (0..len)
        .map(|_| {
            if rng.below(2) == 0 {
                SLOT_WORDS[rng.below(SLOT_WORDS.len())]
            } else {
                FILLER[rng.below(FILLER.len())]
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn generated_slot(rng: &mut Rng) -> Vec<&'static str> {
    let mut slot: Vec<&str> = SLOT_WORDS
        .iter()
        .copied()
        .filter(|_| rng.below(3) == 0)
        .collect();
    if slot.is_empty() {
        slot.push(SLOT_WORDS[rng.below(SLOT_WORDS.len())]);
    }
    slot
}

#[test]
fn generated_slot_queries_match_the_oracle_and_widening_never_drops_a_doc() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut failures = Vec::new();
    for _ in 0..3000 {
        let text = generated_text(&mut rng);
        let left = generated_slot(&mut rng);
        let right = generated_slot(&mut rng);
        let max_gaps = rng.below(4) as u32;

        let query = near_query(&left, &right, max_gaps);
        let actual = engine_matches(&query, &text);
        let expected = oracle_matches(&left, &right, max_gaps, &text);
        if actual != expected {
            failures.push(format!(
                "{query:?} on {text:?}: tinql {actual}, oracle {expected}"
            ));
        }

        let extra = SLOT_WORDS[rng.below(SLOT_WORDS.len())];
        if actual && !left.contains(&extra) {
            let mut wider = left.clone();
            wider.push(extra);
            let wide_query = near_query(&wider, &right, max_gaps);
            if !engine_matches(&wide_query, &text) {
                failures.push(format!(
                    "{query:?} matched {text:?} but {wide_query:?} did not"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures, first:\n{}",
        failures.len(),
        failures[..failures.len().min(10)].join("\n")
    );
}
