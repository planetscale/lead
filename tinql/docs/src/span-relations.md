# Span Relations

Span relation operators filter the left expression's spans by their position
relative to spans from the right expression.

The operands must produce spans. Match-all (`*`) cannot be used in a relation
expression.

## ENCLOSES

`A ENCLOSES B` keeps only the spans of A that enclose at least one span of B
somewhere within them.

```
(security NEAR/10 threat) ENCLOSES critical
```

Find spans where "security" and "threat" appear with at most 10 positions
between them, then keep only those spans that also enclose the word "critical"
inside them.

## NOT ENCLOSES

`A NOT ENCLOSES B` keeps only the spans of A that do **not** enclose any span
of B.

```
(security NEAR/10 threat) NOT ENCLOSES [buy purchase pricing]
```

Find security-threat spans that don't mention buying, purchasing, or pricing.
Useful for filtering out commercial content when you want informational results.

## ENCLOSED BY

`A ENCLOSED BY B` keeps only the spans of A that fall entirely within some span
of B.

```
critical ENCLOSED BY (security NEAR/10 threat)
```

Find occurrences of "critical" that appear inside a security-threat span. This
returns the inner span of "critical". ENCLOSES returns the enclosing span.

### ENCLOSES vs. ENCLOSED BY

Both express the same relationship, but they differ in *which spans are
returned*:

| Operator | Returns | Condition |
|---|---|---|
| `A ENCLOSES B` | Spans of A | where A encloses B |
| `A ENCLOSED BY B` | Spans of A | where A is inside B |

Choose based on which span you want to carry forward into further operations.

## NOT ENCLOSED BY

`A NOT ENCLOSED BY B` keeps only the spans of A that are **not** inside any
span of B.

```
price NOT ENCLOSED BY (disclaimer NEAR/20 terms)
```

Find mentions of "price" that are not inside a disclaimer section.

## OVERLAPPING

`A OVERLAPPING B` keeps only the spans of A that overlap with at least one span
of B. Two spans overlap if they share at least one word position.

```
(beer NEAR/5 craft) OVERLAPPING (IPA NEAR/5 hops)
```

Find regions where the two proximity matches share at least one position.

## NOT OVERLAPPING

`A NOT OVERLAPPING B` keeps only the spans of A that do **not** share any
positions with any span of B.

```
(beer NEAR/5 craft) NOT OVERLAPPING bud
```

Beer-craft spans that don't overlap with any occurrence of "bud".

## BEFORE

`A BEFORE B` keeps only the spans of A where there exists at least one span of
B that starts at a later position.

```
introduction BEFORE conclusion
```

Occurrences of "introduction" that have a "conclusion" somewhere later in the
document. The comparison uses the start positions of A and B, so the spans
can overlap.

## AFTER

`A AFTER B` keeps only the spans of A where there exists at least one span of
B that starts at an earlier position.

```
results AFTER methods
```

Occurrences of "results" that appear after "methods" in the document.

## Negated variants

Every relation operator (except BEFORE and AFTER) has a `NOT` variant:

| Positive | Negated |
|---|---|
| `A ENCLOSES B` | `A NOT ENCLOSES B` |
| `A ENCLOSED BY B` | `A NOT ENCLOSED BY B` |
| `A OVERLAPPING B` | `A NOT OVERLAPPING B` |

In a negated relation, B only filters: its terms add nothing to the score, as
with `AND NOT`. A term that also appears in A, or elsewhere in the query,
still scores through those occurrences.

## Combining relations with other operators

When combining relation operators with proximity and positional filters, you
often want to parenthesize the span-producing expression on the left:

```
(security NEAR/5 threat) NOT ENCLOSES pricing IN FIRST 200 WORDS
```

This reads as: find security-threat proximity spans that don't enclose "pricing",
then filter to the first 200 words of the document.

## A practical example

Here is a query that combines several relation operators into a precise filter:

```
(
    (security NEAR/5 [threat vulnerability risk])
    NOT ENCLOSES [buy purchase subscribe pricing]
) IN FIRST 200 WORDS
AND compliance
```

Step by step:

1. `security NEAR/5 [threat vulnerability risk]`: find spans where
   "security" appears near any of these risk-related terms.
2. `NOT ENCLOSES [buy purchase subscribe pricing]`: exclude spans that
   enclose commercial terms.
3. `IN FIRST 200 WORDS`: keep only matches in the opening section.
4. `AND compliance`: additionally require the word "compliance" somewhere in
   the document.
