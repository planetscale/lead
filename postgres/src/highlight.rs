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
use crate::match_positions::MatchPosition;
use rustc_hash::FxHasher;
use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use thiserror::Error;
use tokenizer::Tokenizer;

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_TERM_FALLBACK_PALETTE: [&str; 8] = [
    "\x1b[1;31m",
    "\x1b[1;32m",
    "\x1b[1;33m",
    "\x1b[1;34m",
    "\x1b[1;35m",
    "\x1b[1;36m",
    "\x1b[1;91m",
    "\x1b[1;94m",
];
const ANSI_SPAN_FALLBACK_PALETTE: [&str; 8] = [
    "\x1b[97;41m",
    "\x1b[97;42m",
    "\x1b[30;43m",
    "\x1b[97;44m",
    "\x1b[97;45m",
    "\x1b[30;46m",
    "\x1b[30;101m",
    "\x1b[97;104m",
];
const ANSI_TERM_NAMED_COLORS: [(&str, &str); 11] = [
    ("black", "\x1b[1;30m"),
    ("red", "\x1b[1;31m"),
    ("green", "\x1b[1;32m"),
    ("yellow", "\x1b[1;33m"),
    ("blue", "\x1b[1;34m"),
    ("magenta", "\x1b[1;35m"),
    ("purple", "\x1b[1;35m"),
    ("cyan", "\x1b[1;36m"),
    ("white", "\x1b[1;37m"),
    ("gray", "\x1b[1;90m"),
    ("grey", "\x1b[1;90m"),
];
const ANSI_SPAN_NAMED_COLORS: [(&str, &str); 11] = [
    ("black", "\x1b[97;40m"),
    ("red", "\x1b[97;41m"),
    ("green", "\x1b[97;42m"),
    ("yellow", "\x1b[30;43m"),
    ("blue", "\x1b[97;44m"),
    ("magenta", "\x1b[97;45m"),
    ("purple", "\x1b[97;45m"),
    ("cyan", "\x1b[30;46m"),
    ("white", "\x1b[30;47m"),
    ("gray", "\x1b[97;100m"),
    ("grey", "\x1b[97;100m"),
];

#[derive(Clone, Copy)]
struct ByteRange {
    start: usize,
    end: usize,
}

struct HighlightRange {
    bytes: ByteRange,
    query_parts: Vec<String>,
    has_span: bool,
}

#[derive(Debug, PartialEq, Error)]
pub(crate) enum HighlightError {
    #[error(
        "highlight position range {start}..={end} is invalid for a token stream of length {token_count}"
    )]
    InvalidPositionRange {
        start: i32,
        end: i32,
        token_count: usize,
    },
    #[error("highlight cannot recover byte offsets when tokenization transforms token text")]
    TransformedTokenText,
}

enum HighlightStyle<'a> {
    Html {
        begin_tag: &'a str,
        end_tag: &'a str,
    },
    Ansi,
}

pub(crate) fn highlight_text(
    text: &str,
    begin_tag: &str,
    end_tag: &str,
    positions: &[MatchPosition],
) -> Result<String, HighlightError> {
    highlight_text_with_tokenizer(
        &tokenizer::presets::default_pipeline().source_spans(),
        text,
        HighlightStyle::Html { begin_tag, end_tag },
        positions,
    )
}

pub(crate) fn highlight_text_ansi(
    text: &str,
    positions: &[MatchPosition],
) -> Result<String, HighlightError> {
    highlight_text_with_tokenizer(
        &tokenizer::presets::default_pipeline().source_spans(),
        text,
        HighlightStyle::Ansi,
        positions,
    )
}

pub(crate) fn rewrap_text(text: &str, wrap_to: usize) -> String {
    assert!(wrap_to > 0, "wrap_to must be positive");

    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return text.to_owned();
    };

    let mut output = String::with_capacity(text.len());
    output.push_str(first);
    let mut line_width = first.chars().count();

    for word in words {
        let word_width = word.chars().count();
        if line_width + 1 + word_width <= wrap_to {
            output.push(' ');
            output.push_str(word);
            line_width += 1 + word_width;
        } else {
            output.push('\n');
            output.push_str(word);
            line_width = word_width;
        }
    }

    output
}

/// Parses a tinql query and evaluates it against the document text to produce
/// match positions inline. Used when the planner injects the query text for
/// highlight evaluation.
///
/// Uses the built-in default analyzer pipeline for both the query and document.
pub(crate) fn positions_from_query(tinql_text: &str, text: &str) -> Vec<MatchPosition> {
    let query = match tinql::runtime::parse_tinql_to_query_default(tinql_text) {
        Ok(query) => query,
        Err(_) => return Vec::new(),
    };
    let doc = tinql::runtime::tokenize_doc(text, tokenizer::presets::default_pipeline());
    let matches = tinql::runtime::evaluate_for_highlight(&query, &doc);
    matches
        .into_iter()
        .map(|m| {
            if m.start == m.end {
                MatchPosition::point(m.part, m.start)
            } else {
                MatchPosition::span(m.part, m.start, m.end)
            }
        })
        .collect()
}

fn highlight_text_with_tokenizer<T: Tokenizer>(
    tokenizer: &T,
    text: &str,
    style: HighlightStyle<'_>,
    positions: &[MatchPosition],
) -> Result<String, HighlightError> {
    if text.is_empty() || positions.is_empty() {
        return Ok(text.to_owned());
    }

    let ranges = byte_ranges_for_positions(tokenizer, text, positions)?;

    let merged = merge_overlapping_ranges(ranges);
    if merged.is_empty() {
        return Ok(text.to_owned());
    }

    let extra_bytes = match style {
        HighlightStyle::Html { begin_tag, end_tag } => {
            merged.len() * (begin_tag.len() + end_tag.len())
        }
        HighlightStyle::Ansi => merged.len() * 16,
    };
    let mut output = String::with_capacity(text.len() + extra_bytes);
    let mut cursor = 0;
    for range in merged {
        output.push_str(&text[cursor..range.bytes.start]);
        match style {
            HighlightStyle::Html { begin_tag, end_tag } => {
                output.push_str(&render_begin_tag(begin_tag, &range.query_parts));
                output.push_str(&text[range.bytes.start..range.bytes.end]);
                output.push_str(end_tag);
            }
            HighlightStyle::Ansi => {
                push_ansi_highlighted_text(
                    &mut output,
                    render_ansi_begin(&range.query_parts, range.has_span),
                    &text[range.bytes.start..range.bytes.end],
                );
            }
        }
        cursor = range.bytes.end;
    }
    output.push_str(&text[cursor..]);
    Ok(output)
}

fn byte_ranges_for_positions<T: Tokenizer>(
    tokenizer: &T,
    text: &str,
    positions: &[MatchPosition],
) -> Result<Vec<HighlightRange>, HighlightError> {
    let token_ranges = token_byte_ranges(tokenizer, text)?;
    let token_count = token_ranges.len();
    let mut ranges = Vec::with_capacity(positions.len());
    for position in positions {
        let Ok(start_idx) = usize::try_from(position.start) else {
            return Err(HighlightError::InvalidPositionRange {
                start: position.start,
                end: position.end,
                token_count,
            });
        };
        let Ok(end_idx) = usize::try_from(position.end) else {
            return Err(HighlightError::InvalidPositionRange {
                start: position.start,
                end: position.end,
                token_count,
            });
        };
        if start_idx > end_idx {
            return Err(HighlightError::InvalidPositionRange {
                start: position.start,
                end: position.end,
                token_count,
            });
        }
        let Some(start) = token_ranges.get(start_idx).copied() else {
            return Err(HighlightError::InvalidPositionRange {
                start: position.start,
                end: position.end,
                token_count,
            });
        };
        let Some(end) = token_ranges.get(end_idx).copied() else {
            return Err(HighlightError::InvalidPositionRange {
                start: position.start,
                end: position.end,
                token_count,
            });
        };
        ranges.push(HighlightRange {
            bytes: ByteRange {
                start: start.start,
                end: end.end,
            },
            query_parts: vec![position.part.clone()],
            has_span: start_idx != end_idx,
        });
    }
    Ok(ranges)
}

fn token_byte_ranges<T: Tokenizer>(
    tokenizer: &T,
    text: &str,
) -> Result<Vec<ByteRange>, HighlightError> {
    let base = text.as_ptr() as usize;
    let mut ranges = Vec::new();

    for token in tokenizer.tokenize(text) {
        let borrowed = match token.text {
            Cow::Borrowed(token_text) => token_text,
            Cow::Owned(_) => return Err(HighlightError::TransformedTokenText),
        };
        let start = borrowed.as_ptr() as usize - base;
        let end = start + borrowed.len();
        ranges.push(ByteRange { start, end });
    }

    Ok(ranges)
}

fn merge_overlapping_ranges(mut ranges: Vec<HighlightRange>) -> Vec<HighlightRange> {
    if ranges.is_empty() {
        return ranges;
    }

    ranges.sort_unstable_by_key(|range| (range.bytes.start, range.bytes.end));
    let mut merged: Vec<HighlightRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(current) if range.bytes.start <= current.bytes.end => {
                current.bytes.end = current.bytes.end.max(range.bytes.end);
                current.query_parts.extend(range.query_parts);
                current.has_span |= range.has_span;
                normalize_query_parts(&mut current.query_parts);
            }
            _ => merged.push(range),
        }
    }
    merged
}

fn normalize_query_parts(query_parts: &mut Vec<String>) {
    query_parts.sort_unstable();
    query_parts.dedup();
}

fn render_begin_tag(template: &str, query_parts: &[String]) -> String {
    if !template.contains("$QUERY_PART") && !template.contains("$QUERY_LABEL") {
        return template.to_owned();
    }

    let mut rendered = template.to_owned();

    if rendered.contains("$QUERY_PART") {
        let mut joined = query_parts.join("; ");
        html_escape_in_place(&mut joined);
        rendered = rendered.replace("$QUERY_PART", &joined);
    }

    if rendered.contains("$QUERY_LABEL") {
        let mut labels = query_parts
            .iter()
            .map(|query_part| query_label(query_part))
            .collect::<Vec<_>>();
        labels.sort_unstable();
        labels.dedup();
        rendered = rendered.replace("$QUERY_LABEL", &labels.join(" "));
    }

    rendered
}

fn render_ansi_begin(query_parts: &[String], has_span: bool) -> &'static str {
    query_parts
        .iter()
        .find_map(|query_part| named_ansi_color(query_part, has_span))
        .unwrap_or_else(|| hashed_ansi_color(query_parts, has_span))
}

fn push_ansi_highlighted_text(output: &mut String, begin: &str, text: &str) {
    let bytes = text.as_bytes();
    let mut chunk_start = 0;
    let mut cursor = 0;

    while cursor < bytes.len() {
        if !matches!(bytes[cursor], b'\n' | b'\r') {
            cursor += 1;
            continue;
        }

        if chunk_start < cursor {
            output.push_str(begin);
            output.push_str(&text[chunk_start..cursor]);
            output.push_str(ANSI_RESET);
        }

        let line_break_start = cursor;
        cursor += 1;
        if bytes[line_break_start] == b'\r' && cursor < bytes.len() && bytes[cursor] == b'\n' {
            cursor += 1;
        }
        output.push_str(&text[line_break_start..cursor]);
        chunk_start = cursor;
    }

    if chunk_start < text.len() {
        output.push_str(begin);
        output.push_str(&text[chunk_start..]);
        output.push_str(ANSI_RESET);
    }
}

fn named_ansi_color(query_part: &str, has_span: bool) -> Option<&'static str> {
    let palette = if has_span {
        &ANSI_SPAN_NAMED_COLORS
    } else {
        &ANSI_TERM_NAMED_COLORS
    };
    palette.iter().find_map(|(name, ansi)| {
        query_part
            .trim()
            .eq_ignore_ascii_case(name)
            .then_some(*ansi)
    })
}

fn hashed_ansi_color(query_parts: &[String], has_span: bool) -> &'static str {
    let mut hasher = FxHasher::default();
    for query_part in query_parts {
        query_part.hash(&mut hasher);
        0xff_u8.hash(&mut hasher);
    }
    let palette = if has_span {
        &ANSI_SPAN_FALLBACK_PALETTE
    } else {
        &ANSI_TERM_FALLBACK_PALETTE
    };
    palette[(hasher.finish() as usize) % palette.len()]
}

fn html_escape_in_place(text: &mut String) {
    let original = std::mem::take(text);
    let mut escaped = String::with_capacity(original.len());
    for ch in original.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    *text = escaped;
}

fn query_label(query_part: &str) -> String {
    let mut label = String::with_capacity(query_part.len());
    let mut last_was_separator = false;

    for ch in query_part.chars() {
        if ch.is_ascii_alphanumeric() {
            label.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            label.push('-');
            last_was_separator = true;
        }
    }

    let label = label.trim_matches('-');
    if label.is_empty() {
        return "qpart".into();
    }
    if label.as_bytes()[0].is_ascii_digit() {
        return format!("qpart-{label}");
    }
    label.into()
}
