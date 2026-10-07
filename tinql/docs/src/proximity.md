# Proximity

Proximity operators constrain the distance and order between matching spans.

## THEN/N

`THEN/N` requires the left expression to appear **before** the right expression,
with at most N extra word positions between them.

```
craft THEN/0 beer
```

This matches any place in a document where "craft" is immediately followed by
"beer", at consecutive positions.

```
craft THEN/3 beer
```

This matches when "craft" appears at some position and "beer" appears within
the next 3+1 positions after it. So "craft beer" (adjacent), "craft pale beer",
"craft really good beer" would all match, but "craft this is definitely a good
beer" (five intervening words) would not.

The gap counts extra positions beyond adjacency:

| Query | Max distance (positions apart) |
|---|---|
| `craft THEN/0 beer` | 1 (adjacent) |
| `craft THEN/1 beer` | 2 |
| `craft THEN/3 beer` | 4 |
| `craft THEN/N beer` | N+1 |

The `/N` suffix is required. Bare `THEN` is a syntax error.

### Order matters

`THEN/N` is directional. `craft THEN/0 beer` only matches when "craft" comes
first. It will not match "beer craft".

## NEAR/N

`NEAR/N` works like `THEN/N` but allows matching in **either order**.

```
craft NEAR/0 beer
```

This matches adjacent occurrences of "craft beer" and "beer craft".

```
craft NEAR/5 beer
```

Matches when "craft" and "beer" are within 5+1 positions of each other, in
either order. The `/N` suffix is required.

### When to use NEAR vs. THEN

- Use **THEN/N** when word order is meaningful. "New York" should probably be
  `new THEN/0 york`, because "York New" is not the same thing.
- Use **NEAR/N** when you care about co-occurrence within a region but not about
  order. "Does this paragraph discuss both hops and malt?" is a `NEAR/N` question.

## Chaining proximity operators

Chained proximity operators group left to right:

```
hop THEN/2 skip THEN/5 jump
```

This is equivalent to `(hop THEN/2 skip) THEN/5 jump`. First, find spans where "hop"
is followed by "skip" within 2 extra positions. Then, find places where those
spans are followed by "jump" within 5 extra positions.

You can mix `THEN/N` and `NEAR/N` in the same chain:

```
"big bad" NEAR/5 wolf THEN/10 story
```

This is equivalent to `("big bad" NEAR/5 wolf) THEN/10 story`:

1. Find spans where the phrase "big bad" and the word "wolf" are within 5 extra
   positions of each other (either order).
2. Then find places where those spans are followed by "story" within 10 extra
   positions.

## Proximity with complex operands

The operands of `THEN/N` and `NEAR/N` can be terms, phrases, alternatives,
parenthesized groups, or other proximity expressions. Match-all (`*`) is not
valid in a proximity expression.

```
"craft beer" THEN/5 [IPA stout lager]
```

This matches the phrase "craft beer" followed within 5 extra positions by any
of "IPA", "stout", or "lager".

```
(beer OR ale) NEAR/10 (food OR dinner)
```

Either "beer" or "ale" within 10 extra positions of either "food" or "dinner",
in any order.

Each operand matches at its own position: one occurrence of a word can't
satisfy two operands. `alpha NEAR/3 alpha` needs two occurrences of "alpha",
and `alpha NEAR/0 [alpha beta]` matches "alpha beta" through "beta".

## Understanding the gap parameter

The gap parameter in `THEN/N` and `NEAR/N` controls how many *extra* positions
are allowed beyond the minimum. The minimum distance is determined by the width
of the operand spans.

For simple terms (which each occupy one position), `THEN/0` means the terms
must be adjacent (distance of 1 position). `THEN/3` means up to 3 extra
positions can separate them (distance of up to 4 positions).

When operands are wider (phrases, nested proximity expressions), the gap is
measured from the **end** of the left span to the **start** of the right span.
So `"big bad" THEN/2 wolf` means "wolf" can appear up to 2 extra positions
after the end of the "big bad" phrase.

## WITHIN

`WITHIN N` limits the full match span to N positions, including both endpoints.

### Example

Given this document:

```
pos:  1     2    3     4      5    6    7    8    9
     The  craft beer  scene  has  been hops  and  malt
```

The query `hops NEAR/10 malt` matches because "hops" (pos 7) and "malt"
(pos 9) are within 10+1 positions of each other.  The resulting span covers
positions 7–9, width = 3.

`NEAR/10` would also match "hops" at position 1 and "malt" at position 9,
a span of width 9. Add WITHIN to restrict the width:

```
(hops NEAR/10 malt) WITHIN 4
```

This keeps only matches where the entire span is at most 4 positions wide.
Positions 7–9 (width 3) passes; positions 1–9 (width 9) would not.

### When WITHIN adds value

`(hops NEAR/4 malt) WITHIN 6` gives the same results as `hops NEAR/4 malt`:
two single-word terms with at most four intervening positions span at most six
positions. A smaller WITHIN limit would narrow the results.

WITHIN becomes useful in two situations:

**Three or more chained terms.**  Each pairwise gap can be satisfied while the
overall match stretches across the document:

```
(beer NEAR/10 hops NEAR/10 malt) WITHIN 8
```

Without WITHIN, beer at position 1, hops at 11, and malt at 21 would match.
Each proximity step allows that gap, but the full span covers 21 positions.
WITHIN 8 rejects it.

**Multi-position operands.**  When an operand is a phrase or nested proximity
expression, it occupies more than one position.  The gap is measured from the
end of the left span to the start of the right, but WITHIN measures the full
span from leftmost to rightmost position:

```
("big bad" NEAR/2 wolf) WITHIN 5
```

"big bad" occupies 2 positions, so the match span is wider than the gap
alone would suggest.
