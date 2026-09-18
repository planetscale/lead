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
use crate::ast::{Expr, WildcardPart};

/// Classify a raw term string into `Term` or `Wildcard`.
///
/// Unescaped `*` and `?` produce wildcard parts (`Any` and `Single`).
/// `\*` and `\?` are escaped literals — they become literal characters
/// in `Literal` segments. All other backslash sequences pass through
/// unchanged (backslash is a normal term character).
pub(crate) fn classify_term(raw: &str) -> Expr {
    let mut has_unescaped_wildcard = false;
    let mut escaped = false;
    for c in raw.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
        } else if c == '*' || c == '?' {
            has_unescaped_wildcard = true;
            break;
        }
    }

    if has_unescaped_wildcard {
        Expr::Wildcard(parse_wildcard_parts(raw))
    } else {
        Expr::Term(unescape_wildcard_chars(raw))
    }
}

/// Parse a raw term into structured wildcard parts.
///
/// Unescaped `*` → `Any`, unescaped `?` → `Single`, everything else
/// (including `\*` → `*` and `\?` → `?`) goes into `Literal` segments.
fn parse_wildcard_parts(raw: &str) -> Vec<WildcardPart> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut chars = raw.chars();

    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.clone().next() {
                Some('*') | Some('?') => {
                    literal.push(chars.next().unwrap());
                }
                _ => literal.push('\\'),
            },
            '*' => {
                if !literal.is_empty() {
                    parts.push(WildcardPart::Literal(std::mem::take(&mut literal)));
                }
                parts.push(WildcardPart::Any);
            }
            '?' => {
                if !literal.is_empty() {
                    parts.push(WildcardPart::Literal(std::mem::take(&mut literal)));
                }
                parts.push(WildcardPart::Single);
            }
            _ => literal.push(c),
        }
    }

    if !literal.is_empty() {
        parts.push(WildcardPart::Literal(literal));
    }

    parts
}

/// Unescape `\*` → `*` and `\?` → `?` for plain terms.
/// All other characters (including bare backslashes) pass through unchanged.
fn unescape_wildcard_chars(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.clone().next() {
                Some('*') | Some('?') => {
                    out.push(chars.next().unwrap());
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Unescape a phrase term: `\"` → `"`, `\[` → `[`, `\\` → `\`, etc.
/// If the term contains no backslashes, returns it without allocation.
pub(crate) fn unescape_phrase_term(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Consume the escaped character (or trailing backslash).
            match chars.next() {
                Some(escaped) => out.push(escaped),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
