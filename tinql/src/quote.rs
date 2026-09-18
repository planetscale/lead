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
use std::borrow::Cow;

use crate::{Expr, ImplicitOp, PhraseElement, parse};

/// Return `text` unchanged when it already parses as a standalone literal term.
/// Otherwise, wrap it in a quoted phrase so tinql treats it as one query atom.
pub fn maybe_quote(text: &str) -> Cow<'_, str> {
    if parses_as_identical_term(text) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(quote_as_phrase(text))
    }
}

fn parses_as_identical_term(text: &str) -> bool {
    matches!(parse(text, ImplicitOp::And), Ok(Expr::Term(parsed)) if parsed == text)
}

fn quote_as_phrase(text: &str) -> String {
    Expr::Phrase {
        elements: vec![PhraseElement::Term(text.to_owned())],
        slop: None,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::maybe_quote;
    use crate::{Expr, ImplicitOp, PhraseElement, parse};

    const STABLE_COMBINING_TERM: &str = "\u{0351}\u{034c}\u{0369}\u{0314}\u{0357}\u{0305}";

    #[test]
    fn leaves_plain_terms_unquoted() {
        assert_eq!(maybe_quote("beer"), "beer");
        assert_eq!(maybe_quote("47,000"), "47,000");
        assert_eq!(maybe_quote("wi-fi"), "wi-fi");
        assert_eq!(maybe_quote(STABLE_COMBINING_TERM), STABLE_COMBINING_TERM);
    }

    #[test]
    fn quotes_inputs_that_are_not_standalone_terms() {
        assert_eq!(maybe_quote("beer cheese"), r#""beer cheese""#);
        assert_eq!(maybe_quote("foo*bar"), r#""foo*bar""#);
        assert_eq!(maybe_quote("MATCHES"), r#""MATCHES""#);
    }

    #[test]
    fn quotes_phrase_special_characters() {
        assert_eq!(maybe_quote(r#"a"b[c]_d\e"#), r#""a\"b\[c\]\_d\\e""#);
    }

    #[test]
    fn quoted_wildcard_reparses_as_literal_phrase_term() {
        let reparsed = parse(&maybe_quote("foo*bar"), ImplicitOp::And).unwrap();
        assert_eq!(
            reparsed,
            Expr::Phrase {
                elements: vec![PhraseElement::Term("foo*bar".into())],
                slop: None,
            }
        );
    }

    #[test]
    fn quoted_phrase_input_reparses_successfully() {
        parse(&maybe_quote("beer cheese"), ImplicitOp::And)
            .expect("quoted phrase should parse successfully");
    }
}
