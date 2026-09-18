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
/// A parsed query expression.
///
/// This is the syntax tree — it faithfully represents what the user wrote.
/// A separate lowering pass converts it to the engine's `Query` + `SpanQuery`.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    // -- Leaf nodes --
    /// A single search term: `beer`
    Term(String),

    /// Match-all: standalone `*`
    MatchAll,

    /// Match-nothing: an input that parses or analyzes to the empty query.
    ///
    /// Produced for whitespace-only input and by sub-tokenization when a
    /// term's analyzed token stream is empty (bare punctuation, emoji, a
    /// wildcard whose literal prefix tokenizes away). Never produced for any
    /// non-empty surface syntax, so `Display` renders it as the empty string
    /// and the roundtrip property holds only at the top level — which is the
    /// only place the parser can produce it.
    MatchNone,

    /// Fuzzy term match: `beer~2`, `beer~0:2`
    Fuzzy {
        term: String,
        prefix: u32,
        distance: u32,
    },

    /// Wildcard pattern: `brew*`, `*house`, `b?er`
    Wildcard(Vec<WildcardPart>),

    /// Regex pattern: `MATCHES hop.*s`
    Regex(String),

    /// Term range: `aardvark TO cat`, `* TO cat`, `monkey TO *`
    Range {
        lower: RangeBound,
        upper: RangeBound,
    },

    /// Quoted phrase: `"big bad wolf"`, `"big _ wolf"`, `"big bad wolf"~2`
    Phrase {
        elements: Vec<PhraseElement>,
        slop: Option<u32>,
    },

    /// Alternatives: `[beer, ale, lager]`
    Alternatives(Vec<Expr>),

    /// Minimum-should-match: `AT LEAST 2 OF [...]`, `AT LEAST 50% OF [...]`, `ALL OF [...]`
    AtLeast {
        threshold: AtLeastThreshold,
        exprs: Vec<Expr>,
    },

    // -- Boolean operators (document-level) --
    /// `A AND B`
    And(Box<Expr>, Box<Expr>),

    /// `A OR B`
    Or(Box<Expr>, Box<Expr>),

    /// `A AND NOT B`
    AndNot {
        positive: Box<Expr>,
        negative: Box<Expr>,
    },

    // -- Proximity operators (span-level) --
    /// Ordered proximity: `A THEN/5 B`
    Then {
        left: Box<Expr>,
        right: Box<Expr>,
        gap: u32,
    },

    /// Unordered proximity: `A NEAR/5 B`
    Near {
        left: Box<Expr>,
        right: Box<Expr>,
        gap: u32,
    },

    // -- Relation operators --
    /// `A ENCLOSES B`
    Encloses { big: Box<Expr>, little: Box<Expr> },

    /// `A NOT ENCLOSES B`
    NotEncloses { big: Box<Expr>, little: Box<Expr> },

    /// `A ENCLOSED BY B`
    EnclosedBy { little: Box<Expr>, big: Box<Expr> },

    /// `A NOT ENCLOSED BY B`
    NotEnclosedBy { little: Box<Expr>, big: Box<Expr> },

    /// `A OVERLAPPING B`
    Overlapping { a: Box<Expr>, b: Box<Expr> },

    /// `A NOT OVERLAPPING B`
    NotOverlapping { a: Box<Expr>, b: Box<Expr> },

    /// `A BEFORE B`
    Before { a: Box<Expr>, b: Box<Expr> },

    /// `A AFTER B`
    After { a: Box<Expr>, b: Box<Expr> },

    // -- Positional filters --
    /// `expr IN FIRST 100 WORDS` or `expr IN FIRST 25%`
    First {
        bound: PositionBound,
        inner: Box<Expr>,
    },

    /// `expr IN LAST 50 WORDS` or `expr IN LAST 25%`
    Last {
        bound: PositionBound,
        inner: Box<Expr>,
    },

    /// `expr IN MIDDLE 50%`
    Middle { percent: u32, inner: Box<Expr> },

    /// `expr IN WORDS 500 TO 1000`
    Between { lo: u32, hi: u32, inner: Box<Expr> },

    // -- Width filter --
    /// `expr WITHIN 10`
    Within { width: u32, inner: Box<Expr> },

    // -- Scoring --
    /// Boost: `expr^2`, `"craft beer"^1.5`
    Boost {
        factor: BoostFactor,
        inner: Box<Expr>,
    },
}

/// A boost factor for relevance scoring.
///
/// Wraps `f32` with bitwise equality so `Expr` can derive `Eq`.
#[derive(Debug, Clone)]
pub struct BoostFactor(pub f32);

impl BoostFactor {
    /// Largest boost factor the parser accepts.
    ///
    /// The grammar admits only non-negative numbers, so the parsed domain is
    /// `[0, MAX]`. The cap keeps BM25's single-precision score fold finite:
    /// scoring multiplies `idf (<= ~45) * boost * tf-bucket representative
    /// (<= 31288) * (k1 + 1)`, and with `boost` and `k1` each capped at 1e4
    /// that product stays ~24 orders of magnitude below `f32::MAX`. 1e4 is
    /// also three decades above any published relevance-tuning boost, so the
    /// cap rejects only inputs that could never score meaningfully.
    pub const MAX: f32 = 1.0e4;
}

impl PartialEq for BoostFactor {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for BoostFactor {}

impl From<f32> for BoostFactor {
    fn from(f: f32) -> Self {
        Self(f)
    }
}

// ── Display ─────────────────────────────────────────────────────────────
//
// Precedence-aware formatting that produces minimal parentheses.
// The output of `Display` is always a valid query string that parses
// back to the same AST (roundtrip property).

use std::fmt;

/// Precedence levels — higher number binds tighter (closer to leaves).
mod prec {
    pub const OR: u8 = 1;
    pub const AND: u8 = 2;
    pub const AND_NOT: u8 = 3;
    pub const POS: u8 = 4;
    pub const REL: u8 = 5;
    pub const SPAN: u8 = 6;
    pub const WITHIN: u8 = 7;
    pub const BOOST: u8 = 8;
    pub const PRIMARY: u8 = 9;
}

impl Expr {
    fn precedence(&self) -> u8 {
        match self {
            Expr::Or(..) => prec::OR,
            Expr::And(..) => prec::AND,
            Expr::AndNot { .. } => prec::AND_NOT,
            Expr::First { .. } | Expr::Last { .. } | Expr::Middle { .. } | Expr::Between { .. } => {
                prec::POS
            }
            Expr::Encloses { .. }
            | Expr::NotEncloses { .. }
            | Expr::EnclosedBy { .. }
            | Expr::NotEnclosedBy { .. }
            | Expr::Overlapping { .. }
            | Expr::NotOverlapping { .. }
            | Expr::Before { .. }
            | Expr::After { .. } => prec::REL,
            Expr::Then { .. } | Expr::Near { .. } => prec::SPAN,
            Expr::Within { .. } => prec::WITHIN,
            Expr::Boost { .. } => prec::BOOST,
            _ => prec::PRIMARY,
        }
    }
}

/// Format an expression, wrapping in parens when `expr`'s precedence is
/// below `min_prec`.
fn fmt_expr(f: &mut fmt::Formatter<'_>, expr: &Expr, min_prec: u8) -> fmt::Result {
    let needs_parens = expr.precedence() < min_prec;
    if needs_parens {
        write!(f, "(")?;
    }

    match expr {
        // -- Leaves --
        Expr::Term(s) => {
            for c in s.chars() {
                if c == '*' || c == '?' {
                    write!(f, "\\")?;
                }
                write!(f, "{c}")?;
            }
        }
        Expr::MatchAll => write!(f, "*")?,
        Expr::MatchNone => {}
        Expr::Fuzzy {
            term,
            prefix,
            distance,
        } => {
            write!(f, "{term}{}", fuzzy_suffix(*prefix, *distance))?;
        }
        Expr::Wildcard(parts) => {
            for part in parts {
                match part {
                    WildcardPart::Literal(s) => {
                        for c in s.chars() {
                            if c == '*' || c == '?' {
                                write!(f, "\\")?;
                            }
                            write!(f, "{c}")?;
                        }
                    }
                    WildcardPart::Any => write!(f, "*")?,
                    WildcardPart::Single => write!(f, "?")?,
                }
            }
        }
        Expr::Regex(pattern) => {
            write!(f, "MATCHES ")?;
            for c in pattern.chars() {
                if c.is_ascii_whitespace() {
                    write!(f, "\\{c}")?;
                } else {
                    write!(f, "{c}")?;
                }
            }
        }
        Expr::Range { lower, upper } => write!(f, "{lower} TO {upper}")?,
        Expr::Phrase { elements, slop } => {
            write!(f, "\"")?;
            for (i, elem) in elements.iter().enumerate() {
                if i > 0 {
                    write!(f, " ")?;
                }
                write!(f, "{elem}")?;
            }
            write!(f, "\"")?;
            if let Some(n) = slop {
                write!(f, "~{n}")?;
            }
        }
        Expr::Alternatives(items) => {
            write!(f, "[")?;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    write!(f, " ")?;
                }
                fmt_expr(f, item, 0)?;
            }
            write!(f, "]")?;
        }
        Expr::AtLeast { threshold, exprs } => {
            match threshold {
                AtLeastThreshold::Count(n) => write!(f, "AT LEAST {n} OF [")?,
                AtLeastThreshold::Percent(p) => write!(f, "AT LEAST {p}% OF [")?,
                AtLeastThreshold::All => write!(f, "ALL OF [")?,
            }
            for (i, item) in exprs.iter().enumerate() {
                if i > 0 {
                    write!(f, " ")?;
                }
                fmt_expr(f, item, 0)?;
            }
            write!(f, "]")?;
        }

        // -- Boolean --
        Expr::Or(l, r) => {
            fmt_expr(f, l, prec::OR)?;
            write!(f, " OR ")?;
            fmt_expr(f, r, prec::OR + 1)?;
        }
        Expr::And(l, r) => {
            fmt_expr(f, l, prec::AND)?;
            write!(f, " AND ")?;
            fmt_expr(f, r, prec::AND + 1)?;
        }
        Expr::AndNot { positive, negative } => {
            fmt_expr(f, positive, prec::AND_NOT)?;
            write!(f, " AND NOT ")?;
            fmt_expr(f, negative, prec::AND_NOT + 1)?;
        }
        // -- Proximity --
        Expr::Then { left, right, gap } => {
            fmt_expr(f, left, prec::SPAN)?;
            write!(f, " THEN/{gap} ")?;
            fmt_expr(f, right, prec::SPAN + 1)?;
        }
        Expr::Near { left, right, gap } => {
            fmt_expr(f, left, prec::SPAN)?;
            write!(f, " NEAR/{gap} ")?;
            fmt_expr(f, right, prec::SPAN + 1)?;
        }

        // -- Relations --
        Expr::Encloses { big, little } => {
            fmt_expr(f, big, prec::SPAN)?;
            write!(f, " ENCLOSES ")?;
            fmt_expr(f, little, prec::SPAN)?;
        }
        Expr::NotEncloses { big, little } => {
            fmt_expr(f, big, prec::SPAN)?;
            write!(f, " NOT ENCLOSES ")?;
            fmt_expr(f, little, prec::SPAN)?;
        }
        Expr::EnclosedBy { little, big } => {
            fmt_expr(f, little, prec::SPAN)?;
            write!(f, " ENCLOSED BY ")?;
            fmt_expr(f, big, prec::SPAN)?;
        }
        Expr::NotEnclosedBy { little, big } => {
            fmt_expr(f, little, prec::SPAN)?;
            write!(f, " NOT ENCLOSED BY ")?;
            fmt_expr(f, big, prec::SPAN)?;
        }
        Expr::Overlapping { a, b } => {
            fmt_expr(f, a, prec::SPAN)?;
            write!(f, " OVERLAPPING ")?;
            fmt_expr(f, b, prec::SPAN)?;
        }
        Expr::NotOverlapping { a, b } => {
            fmt_expr(f, a, prec::SPAN)?;
            write!(f, " NOT OVERLAPPING ")?;
            fmt_expr(f, b, prec::SPAN)?;
        }
        Expr::Before { a, b } => {
            fmt_expr(f, a, prec::SPAN)?;
            write!(f, " BEFORE ")?;
            fmt_expr(f, b, prec::SPAN)?;
        }
        Expr::After { a, b } => {
            fmt_expr(f, a, prec::SPAN)?;
            write!(f, " AFTER ")?;
            fmt_expr(f, b, prec::SPAN)?;
        }

        // -- Positional filters (postfix) --
        Expr::First { bound, inner } => {
            fmt_expr(f, inner, prec::REL)?;
            match bound {
                PositionBound::Absolute(n) => write!(f, " IN FIRST {n} WORDS")?,
                PositionBound::Percent(n) => write!(f, " IN FIRST {n}%")?,
            }
        }
        Expr::Last { bound, inner } => {
            fmt_expr(f, inner, prec::REL)?;
            match bound {
                PositionBound::Absolute(n) => write!(f, " IN LAST {n} WORDS")?,
                PositionBound::Percent(n) => write!(f, " IN LAST {n}%")?,
            }
        }
        Expr::Middle { percent, inner } => {
            fmt_expr(f, inner, prec::REL)?;
            write!(f, " IN MIDDLE {percent}%")?;
        }
        Expr::Between { lo, hi, inner } => {
            fmt_expr(f, inner, prec::REL)?;
            write!(f, " IN WORDS {lo} TO {hi}")?;
        }

        // -- Postfix --
        Expr::Within { width, inner } => {
            fmt_expr(f, inner, prec::BOOST)?;
            write!(f, " WITHIN {width}")?;
        }
        Expr::Boost { factor, inner } => {
            fmt_expr(f, inner, prec::PRIMARY)?;
            write!(f, "^{factor}")?;
        }
    }

    if needs_parens {
        write!(f, ")")?;
    }
    Ok(())
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_expr(f, self, 0)
    }
}

fn fuzzy_suffix(prefix: u32, distance: u32) -> String {
    if prefix == 1 {
        format!("~{distance}")
    } else {
        format!("~{prefix}:{distance}")
    }
}

impl fmt::Display for PhraseElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PhraseElement::Term(s) => {
                // Re-escape characters that are special inside phrases.
                for c in s.chars() {
                    if matches!(c, '\\' | '"' | '[' | ']' | '_') {
                        write!(f, "\\")?;
                    }
                    write!(f, "{c}")?;
                }
                Ok(())
            }
            PhraseElement::Gap(n) => {
                for _ in 0..*n {
                    write!(f, "_")?;
                }
                Ok(())
            }
            PhraseElement::Alternatives(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
        }
    }
}

impl fmt::Display for RangeBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RangeBound::Open => write!(f, "*"),
            RangeBound::Term(s) => write!(f, "{s}"),
        }
    }
}

impl fmt::Display for PositionBound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PositionBound::Absolute(n) => write!(f, "{n}"),
            PositionBound::Percent(n) => write!(f, "{n}%"),
        }
    }
}

impl fmt::Display for BoostFactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.fract() == 0.0 && self.0.is_finite() {
            write!(f, "{}", self.0 as u64)
        } else {
            write!(f, "{}", self.0)
        }
    }
}

// ── Supporting types ────────────────────────────────────────────────────

/// Threshold for `AT LEAST ... OF [...]` expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum AtLeastThreshold {
    /// `AT LEAST 3 OF [...]` — absolute count.
    Count(u32),
    /// `AT LEAST 50% OF [...]` — percentage of list length.
    Percent(u32),
    /// `ALL OF [...]` — every alternative must match.
    All,
}

/// A component of a wildcard pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WildcardPart {
    /// Literal text segment.
    Literal(String),
    /// `*` — matches zero or more characters.
    Any,
    /// `?` — matches exactly one character.
    Single,
}

/// An element inside a quoted phrase.
#[derive(Debug, Clone, PartialEq)]
pub enum PhraseElement {
    /// A term in the phrase.
    Term(String),
    /// A gap: each `_` is one position. `__` = 2 positions.
    Gap(u32),
    /// Alternatives inside a phrase: `"[big, large] bad wolf"`
    Alternatives(Vec<Expr>),
}

/// A bound in a term range expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeBound {
    /// Open bound: `*`
    Open,
    /// Specific term bound.
    Term(String),
}

/// The bound for a `LAST` positional filter.
#[derive(Debug, Clone, PartialEq)]
pub enum PositionBound {
    /// `expr IN LAST 50 WORDS` — last 50 positions.
    Absolute(u32),
    /// `expr IN LAST 25%` — last 25% of the document.
    Percent(u32),
}
