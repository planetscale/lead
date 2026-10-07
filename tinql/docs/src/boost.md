# Boost

Boosting multiplies the scoring weights of terms in an expression by a factor.
It preserves the expression's matching conditions.

## Syntax

Append `^` and a number directly to an expression:

```
beer^2
beer^1.5
"craft beer"^3
```

Place `^` immediately after the expression. `beer ^2` (with a space) is an error.

## What you can boost

Boost can be applied to:

- **Terms:** `beer^2`
- **Phrases:** `"craft beer"^2`
- **Fuzzy terms:** `beer~2^3` (fuzzy first, then boost)
- **Sloppy phrases:** `"big bad"~2^1.5` (slop first, then boost)
- **Alternatives:** `[IPA ale]^2`
- **Parenthesized expressions:** `(A NEAR/0 B)^3`
- **Operands inside a phrase or span:** `ipa^3 NEAR/5 hoppy`, `"craft [beer^2 ale]"`

## Boost factors

The factor is a non-negative integer, decimal, or number in scientific
notation (`^1e3`):

- `^2`: double the contribution
- `^0.5`: halve the contribution
- `^1.5`: 50% more weight
- `^0`: zero out the expression's scoring contribution (it still matches)

The maximum factor is 10000; `^10000` itself is accepted. A factor above
10000, including one written in scientific notation like `^1e5`, causes a parse
error: `boost factor "1e5" is out of range (at byte ...):
must be a finite value of at most 10000`.

The effect on the total score also depends on term frequency, document length,
other query terms, and the scoring function's term selection. A boost
of 3 does not guarantee that one document scores three times as high as another.

TIN collects scoring terms from the whole query and adds their weights when a
term occurs in multiple branches. For example, `"craft beer"^3 OR (craft NEAR/5
beer)` gives both terms weight 4 in every matching document, including one that
matches only the proximity branch. Boosting the phrase provides no separate
bonus for matching its positions.

A word written more than once inside one phrase or span adds its weight once
per occurrence too: `"to be or not to be"^2` gives `to` and `be` weight 4 and
`or` and `not` weight 2. The right side of `NOT ENCLOSES`, `NOT ENCLOSED BY`
and `NOT OVERLAPPING` only filters, so its terms add no weight.

A boost on an operand inside a phrase or span weighs that operand alone, as it
would in a Boolean query: `ipa^3 NEAR/5 hoppy` gives `ipa` weight 3 and `hoppy`
weight 1, like `ipa^3 AND hoppy`. A boost on the whole phrase or span,
`(ipa NEAR/5 hoppy)^3`, multiplies every operand. A boost on the right side of
`NOT ENCLOSES`, `NOT ENCLOSED BY` or `NOT OVERLAPPING` adds nothing, because
that side only filters. As anywhere else, an explicit boost keeps a dense term
in `tin.score()`, and only that operand's term: in `the THEN/0 ipa^2`, a dense
`the` is still elided.

## Boost on hyphenated terms

A boost can follow a hyphenated term:

```
wi-fi^2
```

This boosts the expression produced by tokenizing "wi-fi" by a factor of 2.
