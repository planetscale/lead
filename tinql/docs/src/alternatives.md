# Alternatives, AT LEAST & ALL OF

## Alternatives

Square brackets enclose a whitespace-separated list of expressions. A document
matches if it matches **any** of the alternatives.

```
[beer ale lager]
```

This is equivalent to `beer OR ale OR lager`.

Whitespace is the separator inside brackets, so implicit boolean insertion is
disabled there. If an alternative needs multiple tokens, write it with explicit
operators, grouping, or quotes.

Commas also separate elements, with or without surrounding spaces.
`[beer, ale, lager]` is the same list as `[beer ale lager]`. A comma between
two digits is treated as a numeric separator, not a list separator, so
`[47,000 48,000]` is two numeric terms.

Each item inside the brackets is a full expression, so you can use phrases,
proximity operators, and boolean logic:

```
["craft beer" ale stout]

[house NEAR/2 brick cabin]

[A AND B C OR D]
```

Alternatives are especially useful as operands to proximity and relation
operators:

```
security NEAR/5 [threat vulnerability risk]
```

This matches "security" within 5 words of any of "threat", "vulnerability",
or "risk".

### Empty alternatives

`[]` is an error. You must provide at least one expression.

## AT LEAST

AT LEAST sets a minimum number or percentage of matching alternatives.

```
AT LEAST 2 OF [beer wine cheese bread]
```

This matches documents that contain at least 2 of the 4 listed terms. A
document with "beer" and "cheese" matches. A document with only "wine" does not.

### Absolute count

```
AT LEAST 3 OF [a b c d e]
```

At least 3 of the 5 expressions must match.

### Percentage

```
AT LEAST 50% OF [a b c d]
```

At least 50% of the alternatives must match. The percentage is applied to the
number of items in the list, so 50% of 4 items means at least 2 must match.
When the percentage does not divide evenly, the required count rounds up:
50% of 5 items means at least 3 must match.

Place `%` immediately after the number.

### Repeated alternatives

Each listed alternative counts on its own, even when two are the same
expression:

```
AT LEAST 2 OF [beer beer wine]
```

A document that contains only "beer" matches, because it satisfies two of
the three listed alternatives. A percentage counts repeats in the list
length too, so `AT LEAST 50% OF [beer beer wine ale]` requires 2.

### Thresholds outside the list

`AT LEAST 0 OF [...]` matches every document. A threshold larger than the
list, such as `AT LEAST 4 OF [a b c]`, matches no document.

### Syntax

```
AT LEAST <n> OF [<expr> <expr> ...]
AT LEAST <n>% OF [<expr> <expr> ...]
```

Keywords must be UPPER CASE: `AT LEAST 3 OF [...]`.

Each expression inside the brackets can be anything the language supports:

```
AT LEAST 1 OF ["craft beer" hops THEN/0 malt lager~2]
```

## ALL OF

ALL OF requires every alternative to match. It is equivalent to writing
`AT LEAST N OF [...]` where N equals the list length.

```
ALL OF [beer wine spirits]
```

This matches documents that contain all three terms, equivalent to
`beer AND wine AND spirits`. Longer lists can use one expression per line:

```
ALL OF [
    "machine learning"
    "neural network"
    "training data"
    "model accuracy"
]
```

### Syntax

```
ALL OF [<expr> <expr> ...]
```

Keywords must be UPPER CASE: `ALL OF [...]`.

## When to use these operators

- **`[a b c]` (alternatives)**: any one of these is good enough. Same as
  `a OR b OR c`.
- **`AT LEAST N OF [...]`**: flexible middle ground. Require a minimum number
  of hits without demanding every single one.
- **`AT LEAST N% OF [...]`**: same idea, but scales with the list length.
  Useful when the list might change and you want a proportional threshold.
- **`ALL OF [...]`**: every single one must match. Same as chaining AND, but
  clearer when reading.
