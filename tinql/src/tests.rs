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
mod cases {
    use crate::Expr;
    use crate::ImplicitOp;
    use crate::ast::*;
    use crate::error::ParseError;

    // -- Constructors for concise test assertions --

    fn term(s: &str) -> Expr {
        Expr::Term(s.into())
    }
    fn and(a: Expr, b: Expr) -> Expr {
        Expr::And(Box::new(a), Box::new(b))
    }
    fn or(a: Expr, b: Expr) -> Expr {
        Expr::Or(Box::new(a), Box::new(b))
    }
    fn and_not(pos: Expr, neg: Expr) -> Expr {
        Expr::AndNot {
            positive: Box::new(pos),
            negative: Box::new(neg),
        }
    }
    fn then(l: Expr, r: Expr, gap: u32) -> Expr {
        Expr::Then {
            left: Box::new(l),
            right: Box::new(r),
            gap,
        }
    }
    fn near(l: Expr, r: Expr, gap: u32) -> Expr {
        Expr::Near {
            left: Box::new(l),
            right: Box::new(r),
            gap,
        }
    }
    fn phrase(terms: &[&str], slop: Option<u32>) -> Expr {
        Expr::Phrase {
            elements: terms
                .iter()
                .map(|t| PhraseElement::Term(t.to_string()))
                .collect(),
            slop,
        }
    }
    fn fuzzy(term: &str, prefix: u32, distance: u32) -> Expr {
        Expr::Fuzzy {
            term: term.into(),
            prefix,
            distance,
        }
    }
    /// Build a Wildcard from a display-format pattern.
    /// `*` → Any, `?` → Single, everything else → Literal.
    fn wildcard(pattern: &str) -> Expr {
        let mut parts = Vec::new();
        let mut literal = String::new();
        for c in pattern.chars() {
            match c {
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
        Expr::Wildcard(parts)
    }
    fn boost(e: Expr, f: f32) -> Expr {
        Expr::Boost {
            factor: BoostFactor(f),
            inner: Box::new(e),
        }
    }

    fn p(input: &str) -> Result<Expr, ParseError> {
        crate::parse(input, ImplicitOp::And)
    }

    fn parse_or_mode(input: &str) -> Result<Expr, ParseError> {
        crate::parse(input, ImplicitOp::Or)
    }

    // ── Leaf nodes ──────────────────────────────────────────────────────

    #[test]
    fn single_term() {
        assert_eq!(p("beer").unwrap(), term("beer"));
    }

    #[test]
    fn case_preserved_in_terms() {
        assert_eq!(p("HopScotch").unwrap(), term("HopScotch"));
    }

    #[test]
    fn fuzzy_term() {
        assert_eq!(p("beer~2").unwrap(), fuzzy("beer", 1, 2));
    }

    #[test]
    fn fuzzy_term_with_explicit_prefix() {
        assert_eq!(p("beer~0:2").unwrap(), fuzzy("beer", 0, 2));
    }

    #[test]
    fn wildcard_right() {
        assert_eq!(p("brew*").unwrap(), wildcard("brew*"));
    }

    #[test]
    fn wildcard_left() {
        assert_eq!(p("*house").unwrap(), wildcard("*house"));
    }

    #[test]
    fn wildcard_single_char() {
        assert_eq!(p("b?er").unwrap(), wildcard("b?er"));
    }

    #[test]
    fn wildcard_both() {
        assert_eq!(p("*brew*").unwrap(), wildcard("*brew*"));
    }

    #[test]
    fn matches_simple() {
        assert_eq!(p("MATCHES hop.*s").unwrap(), Expr::Regex("hop.*s".into()));
    }

    #[test]
    fn matches_lowercase_is_term() {
        // `matches` (lowercase) is not a keyword — it's a term.
        assert_eq!(
            p("matches beer").unwrap(),
            and(term("matches"), term("beer"))
        );
    }

    #[test]
    fn matches_with_backslash_regex() {
        // \d passes through verbatim as regex content
        assert_eq!(
            p(r"MATCHES \d{3}-\d{4}").unwrap(),
            Expr::Regex(r"\d{3}-\d{4}".into()),
        );
    }

    #[test]
    fn matches_escaped_space() {
        // \ followed by space embeds a literal space
        assert_eq!(
            p(r"MATCHES big\ bad\ wolf").unwrap(),
            Expr::Regex("big bad wolf".into()),
        );
    }

    #[test]
    fn matches_with_boolean() {
        assert_eq!(
            p("MATCHES be{2}r AND cafe").unwrap(),
            and(Expr::Regex("be{2}r".into()), term("cafe")),
        );
    }

    #[test]
    fn matches_no_pattern_is_error() {
        // MATCHES at end of input with no pattern
        assert!(p("beer AND MATCHES").is_err());
    }

    #[test]
    fn bare_matches_is_error() {
        // MATCHES alone at end of input — requires a pattern
        assert!(p("MATCHES").is_err());
    }

    // ── CONTAINS ──────────────────────────────────────────────────────

    #[test]
    fn contains_term() {
        assert_eq!(p("CONTAINS beer").unwrap(), term("beer"));
    }

    #[test]
    fn contains_wildcard() {
        assert_eq!(p("CONTAINS brew*").unwrap(), wildcard("brew*"),);
    }

    #[test]
    fn contains_fuzzy() {
        assert_eq!(p("CONTAINS beer~2").unwrap(), fuzzy("beer", 1, 2),);
    }

    #[test]
    fn contains_with_boolean() {
        assert_eq!(
            p("CONTAINS beer AND MATCHES hop.*s").unwrap(),
            and(term("beer"), Expr::Regex("hop.*s".into())),
        );
    }

    #[test]
    fn contains_lowercase_is_term() {
        // `contains` (lowercase) is not a keyword — it's a term.
        assert_eq!(
            p("contains beer").unwrap(),
            and(term("contains"), term("beer"))
        );
    }

    #[test]
    fn bare_contains_is_error() {
        // CONTAINS alone at end of input — requires a term
        assert!(p("CONTAINS").is_err());
    }

    // ── Match-all ───────────────────────────────────────────────────────

    #[test]
    fn match_all() {
        assert_eq!(p("*").unwrap(), Expr::MatchAll);
    }

    #[test]
    fn match_all_not_encloses() {
        assert_eq!(
            p("* NOT ENCLOSES beer").unwrap(),
            Expr::NotEncloses {
                big: Box::new(Expr::MatchAll),
                little: Box::new(term("beer")),
            }
        );
    }

    #[test]
    fn star_to_is_range_not_matchall() {
        assert_eq!(
            p("* TO cat").unwrap(),
            Expr::Range {
                lower: RangeBound::Open,
                upper: RangeBound::Term("cat".into()),
            }
        );
    }

    // ── Ranges ──────────────────────────────────────────────────────────

    #[test]
    fn range_closed() {
        assert_eq!(
            p("aardvark TO cat").unwrap(),
            Expr::Range {
                lower: RangeBound::Term("aardvark".into()),
                upper: RangeBound::Term("cat".into()),
            }
        );
    }

    #[test]
    fn range_open_left() {
        assert_eq!(
            p("* TO cat").unwrap(),
            Expr::Range {
                lower: RangeBound::Open,
                upper: RangeBound::Term("cat".into()),
            }
        );
    }

    #[test]
    fn range_open_right() {
        assert_eq!(
            p("monkey TO *").unwrap(),
            Expr::Range {
                lower: RangeBound::Term("monkey".into()),
                upper: RangeBound::Open,
            }
        );
    }

    // ── Phrases ─────────────────────────────────────────────────────────

    #[test]
    fn exact_phrase() {
        assert_eq!(
            p(r#""big bad wolf""#).unwrap(),
            phrase(&["big", "bad", "wolf"], None),
        );
    }

    #[test]
    fn sloppy_phrase() {
        assert_eq!(
            p(r#""big bad wolf"~2"#).unwrap(),
            phrase(&["big", "bad", "wolf"], Some(2)),
        );
    }

    #[test]
    fn phrase_with_gap() {
        assert_eq!(
            p(r#""big _ wolf""#).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("big".into()),
                    PhraseElement::Gap(1),
                    PhraseElement::Term("wolf".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn phrase_with_double_gap() {
        assert_eq!(
            p(r#""big __ wolf""#).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("big".into()),
                    PhraseElement::Gap(2),
                    PhraseElement::Term("wolf".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn phrase_with_alternatives() {
        assert_eq!(
            p(r#""[big large] bad wolf""#).unwrap(),
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Alternatives(vec![term("big"), term("large")]),
                    PhraseElement::Term("bad".into()),
                    PhraseElement::Term("wolf".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn empty_phrase_error() {
        assert!(p(r#""""#).is_err());
    }

    #[test]
    fn unclosed_phrase_error() {
        assert!(p(r#""big bad"#).is_err());
    }

    // ── Alternatives ────────────────────────────────────────────────────

    #[test]
    fn simple_alternatives() {
        assert_eq!(
            p("[beer ale lager]").unwrap(),
            Expr::Alternatives(vec![term("beer"), term("ale"), term("lager")]),
        );
    }

    #[test]
    fn alternatives_with_phrases() {
        assert_eq!(
            p(r#"["big bad wolf" piggy]"#).unwrap(),
            Expr::Alternatives(vec![phrase(&["big", "bad", "wolf"], None), term("piggy"),]),
        );
    }

    #[test]
    fn alternatives_with_span() {
        assert_eq!(
            p("[house NEAR/2 brick down]").unwrap(),
            Expr::Alternatives(vec![near(term("house"), term("brick"), 2), term("down")]),
        );
    }

    #[test]
    fn alternatives_with_comma_terms() {
        assert_eq!(
            p("[47,000 48,000]").unwrap(),
            Expr::Alternatives(vec![term("47,000"), term("48,000")]),
        );
    }

    // Commas between alternatives are list separators (documented form
    // `[beer, ale, lager]`), with or without spaces. Commas between digits
    // stay numeric separators (previous test).
    #[test]
    fn alternatives_with_comma_separators() {
        assert_eq!(
            p("[beer, wine]").unwrap(),
            Expr::Alternatives(vec![term("beer"), term("wine")]),
        );
        assert_eq!(
            p("[beer,wine]").unwrap(),
            Expr::Alternatives(vec![term("beer"), term("wine")]),
        );
        // A separator comma between quoted elements is empty residue: it
        // becomes an element that matches nothing rather than poisoning the OR.
        assert_eq!(
            p(r#"["beer", "wine"]"#).unwrap(),
            Expr::Alternatives(vec![
                phrase(&["beer"], None),
                Expr::MatchNone,
                phrase(&["wine"], None),
            ]),
        );
    }

    #[test]
    fn empty_alternatives_error() {
        assert!(p("[]").is_err());
    }

    // ── AT LEAST ────────────────────────────────────────────────────────

    #[test]
    fn at_least_basic() {
        assert_eq!(
            p("AT LEAST 2 OF [beer wine cheese bread]").unwrap(),
            Expr::AtLeast {
                threshold: AtLeastThreshold::Count(2),
                exprs: vec![term("beer"), term("wine"), term("cheese"), term("bread")],
            }
        );
    }

    #[test]
    fn at_least_lowercase_is_terms() {
        // `at least 3 of [a b c d]` with lowercase → all terms.
        let result = p("at least 3 of [a b c d]").unwrap();
        // `at` is a term, `least` is a term, etc.
        assert!(matches!(result, Expr::And(..)));
    }

    #[test]
    fn at_least_with_complex_exprs() {
        assert_eq!(
            p(r#"AT LEAST 1 OF ["craft beer" hops THEN/0 malt]"#).unwrap(),
            Expr::AtLeast {
                threshold: AtLeastThreshold::Count(1),
                exprs: vec![
                    phrase(&["craft", "beer"], None),
                    then(term("hops"), term("malt"), 0),
                ],
            }
        );
    }

    #[test]
    fn at_least_missing_of_is_error() {
        assert!(p("AT LEAST 2 [a b]").is_err());
    }

    #[test]
    fn at_least_percent() {
        assert_eq!(
            p("AT LEAST 50% OF [a b c d]").unwrap(),
            Expr::AtLeast {
                threshold: AtLeastThreshold::Percent(50),
                exprs: vec![term("a"), term("b"), term("c"), term("d")],
            }
        );
    }

    #[test]
    fn at_least_percent_lowercase_is_terms() {
        // `at least 75% of [x y z]` with lowercase → terms.
        let result = p("at least 75% of [x y z]");
        // Not a valid AT LEAST with lowercase keywords.
        assert!(result.is_err() || matches!(result.unwrap(), Expr::And(..)));
    }

    #[test]
    fn all_of_basic() {
        assert_eq!(
            p("ALL OF [beer wine spirits]").unwrap(),
            Expr::AtLeast {
                threshold: AtLeastThreshold::All,
                exprs: vec![term("beer"), term("wine"), term("spirits")],
            }
        );
    }

    #[test]
    fn all_of_lowercase_is_terms() {
        // `all of [a b]` with lowercase → `all` and `of` are terms.
        let result = p("all of [a b]").unwrap();
        assert!(matches!(result, Expr::And(..)));
    }

    #[test]
    fn bare_uppercase_keyword_is_error() {
        // UPPER CASE keywords cannot be bare search terms.
        for kw in [
            "AND",
            "OR",
            "NOT",
            "TO",
            "IN",
            "THEN",
            "NEAR",
            "WITHIN",
            "ALL",
            "AT",
            "MATCHES",
            "CONTAINS",
            "ENCLOSES",
            "ENCLOSED",
            "OVERLAPPING",
            "BEFORE",
            "AFTER",
        ] {
            assert!(
                p(kw).is_err(),
                "bare keyword '{kw}' should be a parse error"
            );
        }
    }

    #[test]
    fn lowercase_keyword_words_are_terms() {
        // Lowercase forms are plain terms, not keywords.
        assert_eq!(p("all").unwrap(), term("all"));
        assert_eq!(p("at").unwrap(), term("at"));
        assert_eq!(p("and").unwrap(), term("and"));
        assert_eq!(p("or").unwrap(), term("or"));
        assert_eq!(p("not").unwrap(), term("not"));
        assert_eq!(p("to").unwrap(), term("to"));
        assert_eq!(p("in").unwrap(), term("in"));
        assert_eq!(p("then").unwrap(), term("then"));
        assert_eq!(p("near").unwrap(), term("near"));
        assert_eq!(p("within").unwrap(), term("within"));
    }

    #[test]
    fn mixed_case_keyword_words_are_terms() {
        // Mixed-case forms are plain terms, not keywords.
        assert_eq!(p("And").unwrap(), term("And"));
        assert_eq!(p("Or").unwrap(), term("Or"));
        assert_eq!(p("Not").unwrap(), term("Not"));
        assert_eq!(p("To").unwrap(), term("To"));
        assert_eq!(p("In").unwrap(), term("In"));
    }

    #[test]
    fn quoted_keyword_is_valid() {
        // Uppercase keywords can be searched when quoted.
        assert!(p("\"AND\"").is_ok());
        assert!(p("\"TO\"").is_ok());
        assert!(p("\"NOT\"").is_ok());
    }

    #[test]
    fn words_containing_keywords_are_terms() {
        // Words that contain keyword substrings but aren't keywords
        // themselves must still parse as terms.
        assert_eq!(p("together").unwrap(), term("together"));
        assert_eq!(p("android").unwrap(), term("android"));
        assert_eq!(p("tornado").unwrap(), term("tornado"));
        assert_eq!(p("north").unwrap(), term("north"));
        assert_eq!(p("internal").unwrap(), term("internal"));
        assert_eq!(p("notify").unwrap(), term("notify"));
        assert_eq!(p("attention").unwrap(), term("attention"));
    }

    #[test]
    fn lowercase_boolean_operators_are_implicit_and() {
        // `beer and wine` → (beer AND "and") AND wine (three terms)
        // because `and` (lowercase) is not a keyword.
        assert_eq!(
            p("beer and wine").unwrap(),
            and(and(term("beer"), term("and")), term("wine"))
        );
    }

    // ── Boolean operators ───────────────────────────────────────────────

    #[test]
    fn boolean_and() {
        assert_eq!(p("beer AND wine").unwrap(), and(term("beer"), term("wine")),);
    }

    #[test]
    fn boolean_or() {
        assert_eq!(p("beer OR wine").unwrap(), or(term("beer"), term("wine")),);
    }

    #[test]
    fn boolean_and_not() {
        assert_eq!(
            p("beer AND NOT bud").unwrap(),
            and_not(term("beer"), term("bud")),
        );
    }

    #[test]
    fn precedence_and_over_or() {
        assert_eq!(
            p("A OR B AND C").unwrap(),
            or(term("A"), and(term("B"), term("C"))),
        );
    }

    #[test]
    fn precedence_and_not_over_and() {
        assert_eq!(
            p("A AND B AND NOT C").unwrap(),
            and(term("A"), and_not(term("B"), term("C"))),
        );
    }

    #[test]
    fn chained_and() {
        assert_eq!(
            p("A AND B AND C").unwrap(),
            and(and(term("A"), term("B")), term("C")),
        );
    }

    #[test]
    fn chained_or() {
        assert_eq!(
            p("A OR B OR C").unwrap(),
            or(or(term("A"), term("B")), term("C")),
        );
    }

    #[test]
    fn and_not_then_and() {
        assert_eq!(
            p("A AND NOT B AND C").unwrap(),
            and(and_not(term("A"), term("B")), term("C")),
        );
    }

    #[test]
    fn parenthesized_override() {
        assert_eq!(
            p("(A OR B) AND C").unwrap(),
            and(or(term("A"), term("B")), term("C")),
        );
    }

    // ── Proximity operators ─────────────────────────────────────────────

    #[test]
    fn then_adjacent() {
        assert_eq!(
            p("craft THEN/0 beer").unwrap(),
            then(term("craft"), term("beer"), 0),
        );
    }

    #[test]
    fn then_with_gap() {
        assert_eq!(
            p("craft THEN/3 beer").unwrap(),
            then(term("craft"), term("beer"), 3),
        );
    }

    #[test]
    fn near_adjacent() {
        assert_eq!(
            p("craft NEAR/0 beer").unwrap(),
            near(term("craft"), term("beer"), 0),
        );
    }

    #[test]
    fn near_with_gap() {
        assert_eq!(
            p("craft NEAR/5 beer").unwrap(),
            near(term("craft"), term("beer"), 5),
        );
    }

    #[test]
    fn chained_then() {
        assert_eq!(
            p("hop THEN/2 skip THEN/5 jump").unwrap(),
            then(then(term("hop"), term("skip"), 2), term("jump"), 5),
        );
    }

    #[test]
    fn mixed_then_near() {
        assert_eq!(
            p(r#""big bad" NEAR/5 wolf THEN/10 story"#).unwrap(),
            then(
                near(phrase(&["big", "bad"], None), term("wolf"), 5),
                term("story"),
                10,
            ),
        );
    }

    #[test]
    fn lowercase_keywords_are_terms() {
        // Lowercase `then` is a term, not a keyword.
        assert_eq!(
            p("craft then beer").unwrap(),
            and(and(term("craft"), term("then")), term("beer")),
        );
        // Lowercase `and` / `or` are terms.
        assert_eq!(
            p("A and B").unwrap(),
            and(and(term("A"), term("and")), term("B")),
        );
        assert_eq!(
            p("A or B").unwrap(),
            and(and(term("A"), term("or")), term("B")),
        );
    }

    // ── Relation operators ──────────────────────────────────────────────

    #[test]
    fn encloses() {
        assert_eq!(
            p("A ENCLOSES B").unwrap(),
            Expr::Encloses {
                big: Box::new(term("A")),
                little: Box::new(term("B")),
            },
        );
    }

    #[test]
    fn not_encloses() {
        assert_eq!(
            p("A NOT ENCLOSES B").unwrap(),
            Expr::NotEncloses {
                big: Box::new(term("A")),
                little: Box::new(term("B")),
            },
        );
    }

    #[test]
    fn enclosed_by() {
        assert_eq!(
            p("A ENCLOSED BY B").unwrap(),
            Expr::EnclosedBy {
                little: Box::new(term("A")),
                big: Box::new(term("B")),
            },
        );
    }

    #[test]
    fn not_enclosed_by() {
        assert_eq!(
            p("A NOT ENCLOSED BY B").unwrap(),
            Expr::NotEnclosedBy {
                little: Box::new(term("A")),
                big: Box::new(term("B")),
            },
        );
    }

    #[test]
    fn overlapping() {
        assert_eq!(
            p("A OVERLAPPING B").unwrap(),
            Expr::Overlapping {
                a: Box::new(term("A")),
                b: Box::new(term("B")),
            },
        );
    }

    #[test]
    fn not_overlapping() {
        assert_eq!(
            p("A NOT OVERLAPPING B").unwrap(),
            Expr::NotOverlapping {
                a: Box::new(term("A")),
                b: Box::new(term("B")),
            },
        );
    }

    #[test]
    fn before() {
        assert_eq!(
            p("A BEFORE B").unwrap(),
            Expr::Before {
                a: Box::new(term("A")),
                b: Box::new(term("B")),
            },
        );
    }

    #[test]
    fn after() {
        assert_eq!(
            p("A AFTER B").unwrap(),
            Expr::After {
                a: Box::new(term("A")),
                b: Box::new(term("B")),
            },
        );
    }

    // ── Positional filters ──────────────────────────────────────────────

    #[test]
    fn first_n() {
        assert_eq!(
            p("beer IN FIRST 100 WORDS").unwrap(),
            Expr::First {
                bound: PositionBound::Absolute(100),
                inner: Box::new(term("beer")),
            },
        );
    }

    #[test]
    fn first_percent() {
        assert_eq!(
            p("beer IN FIRST 25%").unwrap(),
            Expr::First {
                bound: PositionBound::Percent(25),
                inner: Box::new(term("beer")),
            },
        );
    }

    #[test]
    fn last_absolute() {
        assert_eq!(
            p("beer IN LAST 50 WORDS").unwrap(),
            Expr::Last {
                bound: PositionBound::Absolute(50),
                inner: Box::new(term("beer")),
            },
        );
    }

    #[test]
    fn last_percent() {
        assert_eq!(
            p("beer IN LAST 25%").unwrap(),
            Expr::Last {
                bound: PositionBound::Percent(25),
                inner: Box::new(term("beer")),
            },
        );
    }

    #[test]
    fn middle_percent() {
        assert_eq!(
            p("beer IN MIDDLE 50%").unwrap(),
            Expr::Middle {
                percent: 50,
                inner: Box::new(term("beer")),
            },
        );
    }

    #[test]
    fn middle_with_expression() {
        assert_eq!(
            p("(hops NEAR/0 malt) IN MIDDLE 80%").unwrap(),
            Expr::Middle {
                percent: 80,
                inner: Box::new(near(term("hops"), term("malt"), 0)),
            },
        );
    }

    #[test]
    fn between() {
        assert_eq!(
            p("beer IN WORDS 500 TO 1000").unwrap(),
            Expr::Between {
                lo: 500,
                hi: 1000,
                inner: Box::new(term("beer")),
            },
        );
    }

    // ── Width filter ────────────────────────────────────────────────────

    #[test]
    fn within() {
        assert_eq!(
            p("(hops NEAR/0 malt) WITHIN 6").unwrap(),
            Expr::Within {
                width: 6,
                inner: Box::new(near(term("hops"), term("malt"), 0)),
            },
        );
    }

    // ── Boost ───────────────────────────────────────────────────────────

    #[test]
    fn boost_term() {
        assert_eq!(p("beer^2").unwrap(), boost(term("beer"), 2.0));
    }

    #[test]
    fn boost_float() {
        assert_eq!(p("beer^1.5").unwrap(), boost(term("beer"), 1.5));
    }

    #[test]
    fn boost_phrase() {
        assert_eq!(
            p(r#""craft beer"^2"#).unwrap(),
            boost(phrase(&["craft", "beer"], None), 2.0),
        );
    }

    #[test]
    fn boost_parenthesized() {
        assert_eq!(
            p("(A NEAR/0 B)^3").unwrap(),
            boost(near(term("A"), term("B"), 0), 3.0),
        );
    }

    #[test]
    fn boost_alternatives() {
        assert_eq!(
            p("[IPA ale]^2").unwrap(),
            boost(Expr::Alternatives(vec![term("IPA"), term("ale")]), 2.0),
        );
    }

    #[test]
    fn fuzzy_then_boost() {
        assert_eq!(p("beer~2^3").unwrap(), boost(fuzzy("beer", 1, 2), 3.0),);
    }

    #[test]
    fn sloppy_phrase_then_boost() {
        assert_eq!(
            p(r#""big bad"~2^1.5"#).unwrap(),
            boost(phrase(&["big", "bad"], Some(2)), 1.5),
        );
    }

    #[test]
    fn boost_requires_adjacent_caret() {
        assert!(p("beer ^2").is_err());
    }

    // A boost factor accepts scientific notation as one number; the exponent
    // never splits off into a juxtaposed term.
    #[test]
    fn boost_accepts_scientific_notation() {
        assert_eq!(p("beer^1e3").unwrap(), boost(term("beer"), 1000.0));
        assert_eq!(p("beer^1.5e2").unwrap(), boost(term("beer"), 150.0));
        assert_eq!(p("beer^2E-1").unwrap(), boost(term("beer"), 0.2));
    }

    // The boost domain is [0, BoostFactor::MAX]: factors above the cap —
    // including ones that overflow f32 parsing to +inf — are rejected at
    // parse time with a positioned error.
    #[test]
    fn boost_rejects_factors_outside_the_domain() {
        assert_eq!(
            p(&format!("beer^{}", BoostFactor::MAX)).unwrap(),
            boost(term("beer"), BoostFactor::MAX),
        );
        assert_eq!(p("beer^0").unwrap(), boost(term("beer"), 0.0));
        for query in [
            "beer^10001",
            "beer^1e5",
            &format!("beer^{}", "9".repeat(35)),
            &format!("beer^{}", "9".repeat(39)), // parses to +inf
        ] {
            assert!(
                matches!(p(query), Err(ParseError::BoostOutOfRange { pos: 5, .. })),
                "expected BoostOutOfRange for {query:?}",
            );
        }
    }

    // Parenthesized and bracketed MATCHES compose like every other primary:
    // an unescaped ')' or ']' ends the pattern unless it closes a group or
    // character class opened inside the pattern.
    #[test]
    fn matches_composes_with_parens_brackets_and_boost() {
        assert_eq!(p("(MATCHES hop.*s)").unwrap(), Expr::Regex("hop.*s".into()));
        assert_eq!(
            p("(MATCHES hop.*s)^2").unwrap(),
            boost(Expr::Regex("hop.*s".into()), 2.0),
        );
        assert_eq!(
            p("(MATCHES hop.*s) AND hops").unwrap(),
            and(Expr::Regex("hop.*s".into()), term("hops")),
        );
        assert_eq!(
            p("[MATCHES hop.*s]").unwrap(),
            Expr::Alternatives(vec![Expr::Regex("hop.*s".into())]),
        );
        // Regex groups and character classes keep their closers.
        assert_eq!(p("MATCHES hop(s)?").unwrap(), Expr::Regex("hop(s)?".into()));
        assert_eq!(
            p("(MATCHES alp[a-z]*)").unwrap(),
            Expr::Regex("alp[a-z]*".into()),
        );
    }

    // Range bounds are plain terms or the open marker; the free-term
    // wildcard forms have no defined range semantics and fail closed.
    #[test]
    fn range_bounds_reject_wildcards() {
        assert!(matches!(
            p("b?nana TO date"),
            Err(ParseError::WildcardInRangeBound { .. })
        ));
        assert!(matches!(
            p("ban* TO date"),
            Err(ParseError::WildcardInRangeBound { .. })
        ));
        assert!(matches!(
            p("apple TO d?te"),
            Err(ParseError::WildcardInRangeBound { .. })
        ));
        // The pure open marker is still an open bound, not a wildcard.
        assert_eq!(
            p("* TO cat").unwrap(),
            Expr::Range {
                lower: RangeBound::Open,
                upper: RangeBound::Term("cat".into()),
            },
        );
    }

    // Numbers that do not fit in u32 fail cleanly on every numeric surface
    // instead of panicking inside the builder.
    #[test]
    fn numeric_surfaces_reject_out_of_range_integers() {
        for query in [
            "a THEN/4294967296 b",
            "a NEAR/4294967296 b",
            "(a) WITHIN 4294967296",
            "\"a b\"~4294967296",
            "a~4294967296",
            "a~1:4294967296",
            "a IN FIRST 4294967296 WORDS",
            "a IN LAST 4294967296%",
            "a IN MIDDLE 4294967296%",
            "a IN WORDS 0 TO 4294967296",
            "AT LEAST 4294967296 OF [a b]",
            "AT LEAST 4294967296% OF [a b]",
        ] {
            assert!(
                matches!(p(query), Err(ParseError::NumberOutOfRange { .. })),
                "expected NumberOutOfRange for {query:?}",
            );
        }
        // u32::MAX itself stays accepted.
        assert_eq!(
            p("a THEN/4294967295 b").unwrap(),
            then(term("a"), term("b"), u32::MAX),
        );
    }

    // A comma after a suffixed primary is a list separator: the residue is a
    // standalone element that matches nothing, never part of the fuzzy term.
    #[test]
    fn at_least_comma_after_suffix_keeps_separate_elements() {
        assert_eq!(
            p("AT LEAST 2 OF [beer~1, wine, water]").unwrap(),
            Expr::AtLeast {
                threshold: AtLeastThreshold::Count(2),
                exprs: vec![
                    fuzzy("beer", 1, 1),
                    Expr::MatchNone,
                    term("wine"),
                    term("water"),
                ],
            },
        );
    }

    // ── Complex expressions from the design doc ─────────────────────────

    #[test]
    fn design_doc_complete_example() {
        let input = r#"(
            (security NEAR/5 [threat vulnerability risk])
            NOT ENCLOSES [buy purchase subscribe pricing]
        ) IN FIRST 200 WORDS
        AND compliance"#;

        let expected = and(
            Expr::First {
                bound: PositionBound::Absolute(200),
                inner: Box::new(Expr::NotEncloses {
                    big: Box::new(near(
                        term("security"),
                        Expr::Alternatives(vec![
                            term("threat"),
                            term("vulnerability"),
                            term("risk"),
                        ]),
                        5,
                    )),
                    little: Box::new(Expr::Alternatives(vec![
                        term("buy"),
                        term("purchase"),
                        term("subscribe"),
                        term("pricing"),
                    ])),
                }),
            },
            term("compliance"),
        );

        assert_eq!(p(input).unwrap(), expected);
    }

    #[test]
    fn not_overlapping_with_boolean() {
        let input = "(beer NEAR/5 craft) NOT OVERLAPPING bud";
        assert_eq!(
            p(input).unwrap(),
            Expr::NotOverlapping {
                a: Box::new(near(term("beer"), term("craft"), 5)),
                b: Box::new(term("bud")),
            },
        );
    }

    #[test]
    fn before_with_alternatives() {
        let input = r#"("craft" THEN/0 [IPA stout lager]) BEFORE [price cost]"#;
        assert_eq!(
            p(input).unwrap(),
            Expr::Before {
                a: Box::new(then(
                    phrase(&["craft"], None),
                    Expr::Alternatives(vec![term("IPA"), term("stout"), term("lager")]),
                    0,
                )),
                b: Box::new(Expr::Alternatives(vec![term("price"), term("cost")])),
            },
        );
    }

    #[test]
    fn results_after_methods() {
        assert_eq!(
            p("results AFTER methods").unwrap(),
            Expr::After {
                a: Box::new(term("results")),
                b: Box::new(term("methods")),
            },
        );
    }

    #[test]
    fn positional_filter_binds_to_rel_expr() {
        assert_eq!(
            p("beer IN FIRST 200 WORDS AND wine").unwrap(),
            and(
                Expr::First {
                    bound: PositionBound::Absolute(200),
                    inner: Box::new(term("beer")),
                },
                term("wine"),
            ),
        );
    }

    // ── Implicit AND ────────────────────────────────────────────────────

    #[test]
    fn implicit_and_two_terms() {
        assert_eq!(p("beer wine").unwrap(), and(term("beer"), term("wine")),);
    }

    #[test]
    fn implicit_and_three_terms() {
        assert_eq!(
            p("beer wine stout").unwrap(),
            and(and(term("beer"), term("wine")), term("stout")),
        );
    }

    #[test]
    fn implicit_and_with_explicit_or() {
        assert_eq!(
            p("beer wine OR stout").unwrap(),
            or(and(term("beer"), term("wine")), term("stout")),
        );
    }

    #[test]
    fn implicit_and_mixed_with_explicit() {
        assert_eq!(
            p("beer wine AND stout").unwrap(),
            and(and(term("beer"), term("wine")), term("stout")),
        );
    }

    #[test]
    fn implicit_and_with_phrase() {
        assert_eq!(
            p(r#"beer "craft ale""#).unwrap(),
            and(term("beer"), phrase(&["craft", "ale"], None)),
        );
    }

    #[test]
    fn implicit_and_with_span() {
        assert_eq!(
            p("beer wine THEN/0 stout").unwrap(),
            and(term("beer"), then(term("wine"), term("stout"), 0)),
        );
    }

    #[test]
    fn implicit_and_with_parens() {
        assert_eq!(
            p("beer (wine OR stout)").unwrap(),
            and(term("beer"), or(term("wine"), term("stout"))),
        );
    }

    #[test]
    fn implicit_and_stops_at_or() {
        assert_eq!(
            p("beer wine OR stout cheese").unwrap(),
            or(
                and(term("beer"), term("wine")),
                and(term("stout"), term("cheese")),
            ),
        );
    }

    // ── Implicit OR ─────────────────────────────────────────────────────

    #[test]
    fn implicit_or_two_terms() {
        assert_eq!(
            parse_or_mode("beer wine").unwrap(),
            or(term("beer"), term("wine")),
        );
    }

    #[test]
    fn implicit_or_three_terms() {
        assert_eq!(
            parse_or_mode("beer wine stout").unwrap(),
            or(or(term("beer"), term("wine")), term("stout")),
        );
    }

    #[test]
    fn implicit_or_with_explicit_and() {
        assert_eq!(
            parse_or_mode("beer stout AND wine").unwrap(),
            or(term("beer"), and(term("stout"), term("wine"))),
        );
    }

    #[test]
    fn implicit_or_with_explicit_or() {
        assert_eq!(
            parse_or_mode("beer OR wine stout").unwrap(),
            or(or(term("beer"), term("wine")), term("stout")),
        );
    }

    // ── Error cases ─────────────────────────────────────────────────────

    // An input that parses to the empty query matches nothing; it is not an
    // error.
    #[test]
    fn empty_input() {
        assert_eq!(p("").unwrap(), Expr::MatchNone);
    }

    #[test]
    fn whitespace_only() {
        assert_eq!(p("   ").unwrap(), Expr::MatchNone);
    }

    #[test]
    fn unclosed_paren() {
        assert!(p("(beer AND wine").is_err());
    }

    #[test]
    fn unclosed_bracket() {
        assert!(p("[beer wine").is_err());
    }

    #[test]
    fn slash_is_term_not_regex() {
        // /pattern is now a regular term, not a regex delimiter
        assert_eq!(p("/pattern").unwrap(), term("/pattern"));
    }

    // ── Backslash is a syntax character (terminates terms) ─────────────

    #[test]
    fn backslash_in_term() {
        // backslash is a valid term character
        assert_eq!(p(r"foo\bar").unwrap(), term(r"foo\bar"));
    }

    #[test]
    fn bare_backslash_is_term() {
        assert_eq!(p(r"\").unwrap(), term(r"\"));
    }

    #[test]
    fn backslash_before_syntax_char_is_error() {
        // \( can't start an expression
        assert!(p(r"\(foo").is_err());
    }

    #[test]
    fn backslash_in_phrase_still_escapes() {
        // Inside phrases, backslash escaping works as expected
        assert_eq!(
            p(r#""foo\\bar""#).unwrap(),
            Expr::Phrase {
                elements: vec![PhraseElement::Term(r"foo\bar".into())],
                slop: None,
            }
        );
    }

    // ── Unicode and emoji terms ────────────────────────────────────────

    #[test]
    fn standalone_emoji() {
        assert_eq!(p("🍺").unwrap(), term("🍺"));
    }

    #[test]
    fn emoji_in_term() {
        assert_eq!(p("beer🍺").unwrap(), term("beer🍺"));
    }

    #[test]
    fn emoji_implicit_and() {
        assert_eq!(p("🍺 🍷").unwrap(), and(term("🍺"), term("🍷")),);
    }

    #[test]
    fn unicode_accented_term() {
        assert_eq!(p("café").unwrap(), term("café"));
    }

    #[test]
    fn unicode_start_term() {
        assert_eq!(p("über").unwrap(), term("über"));
    }

    #[test]
    fn cjk_term() {
        assert_eq!(p("日本語").unwrap(), term("日本語"));
    }

    #[test]
    fn roundtrip_unicode() {
        roundtrip("🍺");
        roundtrip("café");
        roundtrip("日本語");
    }

    // ── Term with special characters ────────────────────────────────────

    #[test]
    fn term_with_hyphen() {
        assert_eq!(p("wi-fi").unwrap(), term("wi-fi"));
    }

    #[test]
    fn term_with_dot() {
        assert_eq!(p("v2.0").unwrap(), term("v2.0"));
    }

    #[test]
    fn term_with_apostrophe() {
        assert_eq!(p("o'reilly").unwrap(), term("o'reilly"));
    }

    #[test]
    fn term_with_comma() {
        assert_eq!(p("47,000").unwrap(), term("47,000"));
    }

    #[test]
    fn comma_term_with_boolean() {
        assert_eq!(
            p("welbutrin AND NOT 47,000").unwrap(),
            and_not(term("welbutrin"), term("47,000")),
        );
    }

    #[test]
    fn slash_starts_term() {
        assert_eq!(p("/foo").unwrap(), term("/foo"));
    }

    #[test]
    fn absolute_path() {
        assert_eq!(p("/usr/bin/foo").unwrap(), term("/usr/bin/foo"));
    }

    #[test]
    fn relative_path() {
        assert_eq!(
            p("a/relative/path.zip").unwrap(),
            term("a/relative/path.zip")
        );
    }

    #[test]
    fn backslash_path() {
        assert_eq!(
            p(r"Users\docs\file.exe").unwrap(),
            term(r"Users\docs\file.exe")
        );
    }

    #[test]
    fn windows_drive_path() {
        assert_eq!(p(r"C:\Users\file.exe").unwrap(), term(r"C:\Users\file.exe"));
    }

    #[test]
    fn url() {
        assert_eq!(
            p("https://example.com/path").unwrap(),
            term("https://example.com/path"),
        );
    }

    #[test]
    fn url_with_unescaped_query_is_wildcard() {
        // unescaped `?` triggers wildcard classification
        assert_eq!(
            p("https://example.com/path?q=1").unwrap(),
            wildcard("https://example.com/path?q=1"),
        );
    }

    #[test]
    fn url_with_escaped_query_is_term() {
        // `\?` escapes the wildcard — literal ? in the term
        assert_eq!(
            p(r"https://foo.com/index.html\?id=42").unwrap(),
            term("https://foo.com/index.html?id=42"),
        );
    }

    #[test]
    fn escaped_star_in_glob() {
        // `\*` escapes the wildcard — literal * in the term
        assert_eq!(p(r"file:\*.tar.gz").unwrap(), term("file:*.tar.gz"),);
    }

    #[test]
    fn mixed_escaped_and_unescaped_wildcards() {
        // `\*` is literal, unescaped `*` is still a wildcard
        assert_eq!(
            p(r"foo\*bar*").unwrap(),
            Expr::Wildcard(vec![
                WildcardPart::Literal("foo*bar".into()),
                WildcardPart::Any,
            ]),
        );
    }

    #[test]
    fn escaped_wildcards_roundtrip() {
        // Term with literal * and ? roundtrips through Display
        let parsed = p(r"file:\*.tar.gz").unwrap();
        assert_eq!(parsed, term("file:*.tar.gz"));
        let displayed = parsed.to_string();
        assert_eq!(displayed, r"file:\*.tar.gz");
        // Re-parse the displayed form
        assert_eq!(p(&displayed).unwrap(), term("file:*.tar.gz"));
    }

    #[test]
    fn url_with_boolean() {
        assert_eq!(
            p("https://foo.com AND https://bar.com").unwrap(),
            and(term("https://foo.com"), term("https://bar.com")),
        );
    }

    #[test]
    fn path_with_boolean() {
        assert_eq!(
            p("/usr/bin/foo AND /etc/bar").unwrap(),
            and(term("/usr/bin/foo"), term("/etc/bar")),
        );
    }

    #[test]
    fn then_gap_still_works_with_slash_terms() {
        // THEN/5 still parses as keyword + gap, not one big word
        assert_eq!(
            p("craft THEN/5 beer").unwrap(),
            then(term("craft"), term("beer"), 5),
        );
    }

    #[test]
    fn hash_starts_term() {
        assert_eq!(p("#hashtag").unwrap(), term("#hashtag"));
    }

    #[test]
    fn dollar_starts_term() {
        assert_eq!(p("$variable").unwrap(), term("$variable"));
    }

    #[test]
    fn at_sign_starts_term() {
        assert_eq!(p("@user").unwrap(), term("@user"));
    }

    #[test]
    fn plus_starts_term() {
        assert_eq!(p("+1").unwrap(), term("+1"));
    }

    #[test]
    fn bang_starts_term() {
        assert_eq!(p("!important").unwrap(), term("!important"));
    }

    #[test]
    fn special_start_with_boolean() {
        assert_eq!(
            p("#rust AND $cargo").unwrap(),
            and(term("#rust"), term("$cargo")),
        );
    }

    // ── Hyphenated terms and UUIDs ───────────────────────────────────────

    #[test]
    fn uuid() {
        assert_eq!(
            p("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            term("550e8400-e29b-41d4-a716-446655440000"),
        );
    }

    #[test]
    fn hyphen_leading_term() {
        assert_eq!(p("-foo").unwrap(), term("-foo"));
    }

    #[test]
    fn hyphen_leading_with_implicit_and() {
        assert_eq!(p("beer -bud").unwrap(), and(term("beer"), term("-bud")),);
    }

    #[test]
    fn multi_hyphen_token() {
        assert_eq!(p("my-long-token-name").unwrap(), term("my-long-token-name"),);
    }

    #[test]
    fn hyphenated_terms_with_and_not() {
        // AND NOT is the way to negate — hyphens are just term chars
        assert_eq!(
            p("wi-fi AND NOT blue-tooth").unwrap(),
            and_not(term("wi-fi"), term("blue-tooth")),
        );
    }

    #[test]
    fn uuid_with_boolean() {
        assert_eq!(
            p("550e8400-e29b-41d4-a716-446655440000 AND active").unwrap(),
            and(term("550e8400-e29b-41d4-a716-446655440000"), term("active")),
        );
    }

    #[test]
    fn roundtrip_hyphenated() {
        roundtrip("-foo");
        roundtrip("wi-fi");
        roundtrip("550e8400-e29b-41d4-a716-446655440000");
        roundtrip("my-long-token-name");
    }

    #[test]
    fn roundtrip_comma_term() {
        roundtrip("47,000");
        roundtrip("tbl,foo");
    }

    // ── Keyword quoting ─────────────────────────────────────────────────

    #[test]
    fn keyword_as_phrase_term() {
        assert_eq!(p(r#""THEN""#).unwrap(), phrase(&["THEN"], None),);
    }

    // ── AND NOT disambiguation with relation ops ────────────────────────

    #[test]
    fn and_not_encloses_is_relation() {
        assert_eq!(
            p("A AND B NOT ENCLOSES C").unwrap(),
            and(
                term("A"),
                Expr::NotEncloses {
                    big: Box::new(term("B")),
                    little: Box::new(term("C")),
                },
            ),
        );
    }

    // ── Display roundtrip ───────────────────────────────────────────────

    fn roundtrip(input: &str) {
        let ast = p(input).unwrap();
        let displayed = ast.to_string();
        let reparsed = p(&displayed).unwrap();
        assert_eq!(
            ast, reparsed,
            "\n  input:     {input}\n  displayed: {displayed}"
        );
    }

    #[test]
    fn roundtrip_leaves() {
        roundtrip("beer");
        roundtrip("beer~2");
        roundtrip("beer~1:2");
        roundtrip("beer~0:2");
        roundtrip("brew*");
        roundtrip("*house");
        roundtrip("b?er");
        roundtrip("*");
        roundtrip("MATCHES hop.*s");
        roundtrip(r"MATCHES \d{3}-\d{4}");
        roundtrip("aardvark TO cat");
        roundtrip("* TO cat");
        roundtrip("monkey TO *");
    }

    #[test]
    fn roundtrip_phrases() {
        roundtrip(r#""big bad wolf""#);
        roundtrip(r#""big bad wolf"~2"#);
        roundtrip(r#""big _ wolf""#);
        roundtrip(r#""big __ wolf""#);
        roundtrip(r#""[big large] bad wolf""#);
    }

    #[test]
    fn roundtrip_alternatives() {
        roundtrip("[beer ale lager]");
        roundtrip(r#"["big bad wolf" piggy]"#);
        roundtrip("[house NEAR/2 brick down]");
    }

    #[test]
    fn roundtrip_at_least() {
        roundtrip("AT LEAST 2 OF [beer wine cheese]");
        roundtrip("AT LEAST 50% OF [a b c d]");
        roundtrip("ALL OF [beer wine spirits]");
    }

    #[test]
    fn roundtrip_boolean() {
        roundtrip("beer AND wine");
        roundtrip("beer OR wine");
        roundtrip("beer AND NOT bud");
        roundtrip("A AND B AND C");
        roundtrip("A OR B OR C");
    }

    #[test]
    fn roundtrip_proximity() {
        roundtrip("craft THEN/0 beer");
        roundtrip("craft THEN/5 beer");
        roundtrip("craft NEAR/0 beer");
        roundtrip("craft NEAR/5 beer");
        roundtrip("hop THEN/2 skip THEN/5 jump");
    }

    #[test]
    fn roundtrip_relations() {
        roundtrip("A ENCLOSES B");
        roundtrip("A NOT ENCLOSES B");
        roundtrip("A ENCLOSED BY B");
        roundtrip("A NOT ENCLOSED BY B");
        roundtrip("A OVERLAPPING B");
        roundtrip("A NOT OVERLAPPING B");
        roundtrip("A BEFORE B");
        roundtrip("A AFTER B");
    }

    #[test]
    fn roundtrip_positional() {
        roundtrip("beer IN FIRST 100 WORDS");
        roundtrip("beer IN FIRST 25%");
        roundtrip("beer IN LAST 50 WORDS");
        roundtrip("beer IN LAST 25%");
        roundtrip("beer IN MIDDLE 50%");
        roundtrip("beer IN WORDS 500 TO 1000");
    }

    #[test]
    fn roundtrip_postfix() {
        roundtrip("(hops NEAR/0 malt) WITHIN 6");
        roundtrip("beer^2");
        roundtrip("beer^1.5");
        roundtrip(r#""craft beer"^2"#);
        roundtrip("beer~2^3");
    }

    #[test]
    fn roundtrip_precedence() {
        roundtrip("A OR B AND C");
        roundtrip("(A OR B) AND C");
        roundtrip("A AND B AND NOT C");
        roundtrip("A AND NOT B AND C");
    }

    #[test]
    fn roundtrip_complex() {
        roundtrip(
            "security NEAR/5 [threat vulnerability risk] \
             NOT ENCLOSES [buy purchase subscribe pricing] \
             IN FIRST 200 WORDS AND compliance",
        );
    }

    // ── Display specific output ─────────────────────────────────────────

    #[test]
    fn display_no_unnecessary_parens() {
        assert_eq!(p("A OR B AND C").unwrap().to_string(), "A OR B AND C");
    }

    #[test]
    fn display_adds_parens_when_needed() {
        assert_eq!(p("(A OR B) AND C").unwrap().to_string(), "(A OR B) AND C");
    }

    #[test]
    fn display_left_assoc_and() {
        assert_eq!(p("A AND B AND C").unwrap().to_string(), "A AND B AND C");
    }

    #[test]
    fn display_right_assoc_and_gets_parens() {
        let expr = and(term("A"), and(term("B"), term("C")));
        assert_eq!(expr.to_string(), "A AND (B AND C)");
    }

    #[test]
    fn display_boost_integer() {
        assert_eq!(p("beer^2").unwrap().to_string(), "beer^2");
    }

    #[test]
    fn display_boost_fractional() {
        assert_eq!(p("beer^1.5").unwrap().to_string(), "beer^1.5");
    }

    #[test]
    fn display_then_no_gap() {
        assert_eq!(p("A THEN/0 B").unwrap().to_string(), "A THEN/0 B");
    }

    #[test]
    fn display_then_with_gap() {
        assert_eq!(p("A THEN/5 B").unwrap().to_string(), "A THEN/5 B");
    }

    #[test]
    fn display_match_all() {
        assert_eq!(p("*").unwrap().to_string(), "*");
    }

    #[test]
    fn display_alternatives_use_whitespace_separator() {
        let expr = Expr::Alternatives(vec![term("47,000"), term("48,000")]);
        assert_eq!(expr.to_string(), "[47,000 48,000]");
    }

    #[test]
    fn display_positional_inner_needs_parens() {
        let expr = Expr::First {
            bound: PositionBound::Absolute(100),
            inner: Box::new(and(term("A"), term("B"))),
        };
        assert_eq!(expr.to_string(), "(A AND B) IN FIRST 100 WORDS");
    }

    // ── Escape handling (integration) ───────────────────────────────────

    #[test]
    fn escaped_quote_in_phrase() {
        let ast = p(r#""hello \"world\"""#).unwrap();
        assert_eq!(
            ast,
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("hello".into()),
                    PhraseElement::Term("\"world\"".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn escaped_underscore_in_phrase() {
        let ast = p(r#""hello \_ world""#).unwrap();
        assert_eq!(
            ast,
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("hello".into()),
                    PhraseElement::Term("_".into()),
                    PhraseElement::Term("world".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn escaped_bracket_in_phrase() {
        let ast = p(r#""hello \[world\]""#).unwrap();
        assert_eq!(
            ast,
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("hello".into()),
                    PhraseElement::Term("[world]".into()),
                ],
                slop: None,
            }
        );
    }

    #[test]
    fn escaped_backslash_in_phrase() {
        let ast = p(r#""back\\slash""#).unwrap();
        assert_eq!(
            ast,
            Expr::Phrase {
                elements: vec![PhraseElement::Term(r"back\slash".into()),],
                slop: None,
            }
        );
    }

    #[test]
    fn escape_roundtrip_quote() {
        roundtrip(r#""hello \"world\"""#);
    }

    #[test]
    fn escape_roundtrip_underscore() {
        roundtrip(r#""hello \_ world""#);
    }

    #[test]
    fn escape_roundtrip_bracket() {
        roundtrip(r#""hello \[world\]""#);
    }

    #[test]
    fn escape_roundtrip_backslash() {
        roundtrip(r#""back\\slash""#);
    }

    #[test]
    fn unescaped_special_chars_still_work() {
        let ast = p(r#""big _ [small tiny] wolf""#).unwrap();
        assert_eq!(
            ast,
            Expr::Phrase {
                elements: vec![
                    PhraseElement::Term("big".into()),
                    PhraseElement::Gap(1),
                    PhraseElement::Alternatives(vec![term("small"), term("tiny")]),
                    PhraseElement::Term("wolf".into()),
                ],
                slop: None,
            }
        );
    }

    mod dedup {
        use crate::runtime::Query;

        fn q(input: &str) -> Query {
            crate::runtime::parse_tinql_to_query_default(input).unwrap()
        }

        #[test]
        fn and_dedup_removes_duplicate_terms() {
            // "to be or not to be" → six terms, but "to" and "be" appear twice
            let query = q("to be or not to be");
            let terms = collect_and_terms(&query);
            // Should be deduplicated to four unique terms.
            assert_eq!(terms, vec!["to", "be", "or", "not"]);
            // Display should also show the deduped form.
            assert_eq!(query.to_string(), "AND(to, be, or, not)");
        }

        #[test]
        fn or_dedup_removes_duplicate_terms() {
            let query = q("beer OR wine OR beer OR cheese OR wine");
            let terms = collect_or_terms(&query);
            assert_eq!(terms, vec!["beer", "wine", "cheese"]);
        }

        #[test]
        fn no_dedup_across_different_operators() {
            // beer AND (wine OR beer) — the inner beer is in a different
            // operator context, so it should NOT be deduped.
            let query = q("beer AND (wine OR beer)");
            assert!(matches!(query, Query::Conjunction(..)));
        }

        #[test]
        fn single_term_unchanged() {
            assert!(matches!(q("beer"), Query::Term(ref s) if s == "beer"));
        }

        #[test]
        fn quoted_single_term_is_term_not_span() {
            // `"AND"` is a single-term phrase that should lower to a plain
            // Term, not a Span wrapper.
            let query = q(r#"food "AND" beer"#);
            assert_eq!(query.to_string(), "AND(food, and, beer)");
        }

        /// Flatten an AND chain into term strings (for assertion).
        fn collect_and_terms(query: &Query) -> Vec<String> {
            match query {
                Query::And(l, r) => {
                    let mut out = collect_and_terms(l);
                    out.extend(collect_and_terms(r));
                    out
                }
                Query::Conjunction(children) => {
                    children.iter().flat_map(collect_and_terms).collect()
                }
                Query::Term(s) => vec![s.clone()],
                _ => vec![format!("<non-term:{query:?}>")],
            }
        }

        /// Flatten an OR chain into term strings (for assertion).
        fn collect_or_terms(query: &Query) -> Vec<String> {
            match query {
                Query::Or(l, r) => {
                    let mut out = collect_or_terms(l);
                    out.extend(collect_or_terms(r));
                    out
                }
                Query::Disjunction { min: 1, children } => {
                    children.iter().flat_map(collect_or_terms).collect()
                }
                Query::Term(s) => vec![s.clone()],
                _ => vec![format!("<non-term:{query:?}>")],
            }
        }
    }
}
