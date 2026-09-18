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
use regex_automata::meta::Regex as MetaRegex;
use regex_syntax::hir::Hir;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Clone)]
pub struct CompiledRegex {
    source: Arc<str>,
    hir: Arc<Hir>,
    matcher: MetaRegex,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid regex \"{pattern}\": {message}")]
pub struct RegexError {
    pattern: String,
    message: String,
}

impl CompiledRegex {
    pub fn new(pattern: &str) -> Result<Self, RegexError> {
        let hir = regex_syntax::parse(&full_term_regex_pattern(pattern))
            .map_err(|err| RegexError::new(pattern, err.to_string()))?;
        let matcher = MetaRegex::builder()
            .build_from_hir(&hir)
            .map_err(|err| RegexError::new(pattern, err.to_string()))?;
        Ok(Self {
            source: Arc::from(pattern),
            hir: Arc::new(hir),
            matcher,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn hir(&self) -> &Hir {
        self.hir.as_ref()
    }

    pub fn is_match(&self, haystack: &str) -> bool {
        self.matcher.is_match(haystack)
    }

    /// Returns the literal prefix when this pattern is exactly
    /// `<literal>.*` — the shape the sub-tokenize pass emits for a
    /// prefix wildcard like `foo*`. Matching such a pattern is equivalent
    /// to a byte-prefix test, so callers can range-scan instead of
    /// predicate-filtering. Any other shape returns `None`.
    pub fn pure_prefix(&self) -> Option<String> {
        let body = self.source.strip_suffix(".*")?;
        let mut prefix = String::with_capacity(body.len());
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => prefix.push(chars.next()?),
                // A bare metacharacter means the body is not a plain literal.
                '.' | '*' | '?' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' => {
                    return None;
                }
                _ => prefix.push(c),
            }
        }
        (!prefix.is_empty()).then_some(prefix)
    }
}

impl RegexError {
    fn new(pattern: &str, message: String) -> Self {
        Self {
            pattern: pattern.to_owned(),
            message,
        }
    }
}

impl PartialEq for CompiledRegex {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for CompiledRegex {}

impl Hash for CompiledRegex {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.source.hash(state);
    }
}

impl fmt::Debug for CompiledRegex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CompiledRegex").field(&self.source).finish()
    }
}

impl fmt::Display for CompiledRegex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.source())
    }
}

fn full_term_regex_pattern(pattern: &str) -> String {
    format!(r"\A(?:{pattern})\z")
}

#[cfg(test)]
mod tests {
    use super::CompiledRegex;

    #[test]
    fn compiled_regex_uses_full_term_semantics() {
        let regex = CompiledRegex::new("alp[a-z]*").expect("regex should compile");

        assert!(regex.is_match("alpha"));
        assert!(!regex.is_match("xalpha"));
        assert!(!regex.is_match("alpha-suffix"));
    }

    #[test]
    fn pure_prefix_recognizes_only_literal_dot_star() {
        let prefix = |pattern: &str| CompiledRegex::new(pattern).unwrap().pure_prefix();

        assert_eq!(prefix("foo.*"), Some("foo".into()));
        assert_eq!(prefix(r"l\.a.*"), Some("l.a".into()));
        assert_eq!(prefix("foo.*bar.*"), None); // interior metachar
        assert_eq!(prefix("foo."), None); // not a .* suffix
        assert_eq!(prefix(".*foo"), None);
        assert_eq!(prefix(".*"), None); // empty prefix
        assert_eq!(prefix("fo?o.*"), None);
    }
}
