# boldi-vigna

`boldi-vigna` is a small, pure-Rust engine for evaluating positional queries
using Boldi-Vigna minimal-interval semantics.

Given the sorted token positions for each query term in a single document, it
produces the document's minimal matching intervals: inclusive `[start, end]`
spans that witness why the query matched. Instead of answering only "does this
document match?", the crate returns the antichain of minimal witnesses, which
is exactly what makes phrase, proximity, snippet extraction, span frequency, and
proximity-aware scoring compose cleanly.

This crate is intentionally narrow in scope. It does not tokenize text, scan
postings lists, or rank whole corpora. It just evaluates positional operator
trees over already-decoded term-position streams.

## What makes it distinctive

- It implements minimal-interval semantics, not ad-hoc span matching. Results
  are sorted and minimal under inclusion.
- It compiles a `SpanQuery` once into a reusable `SpanSolver`, then reuses that
  solver across many documents.
- Exact term-only phrases compile to a specialized fast path instead of paying
  the full generic minimal-interval evaluator on every document.
- Evaluation is lazy: `intervals()` yields one minimal interval at a time.
- Inputs are borrowed through `TermPositions`, so callers can feed existing
  position buffers without copying.
- It is pure Rust and storage-agnostic, so it fits equally well in tests, query
  evaluators, and search engines.

## Query model

The public API is deliberately small:

- `SpanQuery`: operator tree describing the positional condition.
- `SpanSolver`: compiled evaluator for a `SpanQuery`.
- `Interval`: one minimal witness span.
- `TermPositions`: trait supplying sorted positions for `SpanQuery::Term(i)`.

The supported operators are:

| Operator                          | Meaning                                                    |
|-----------------------------------|------------------------------------------------------------|
| `Term(i)`                         | Point intervals for term `i`                               |
| `Ordered([...])`                  | Children must appear in document order, non-overlapping    |
| `Unordered([...])`                | Minimal window containing one interval from each child     |
| `Or([...])`                       | Disjunction, with antichain filtering of non-minimal spans |
| `MaxGaps { .. }`                  | Filter by uncovered positions between sub-intervals        |
| `MaxWidth { .. }`                 | Filter by total interval width                             |
| `WithinPositions { .. }`          | Restrict matches to a token-position window                |
| `Containing`, `ContainedBy`       | Containment relations between interval streams             |
| `NotContaining`, `NotContainedBy` | Negative containment relations                             |
| `Overlapping`, `NonOverlapping`   | Overlap relations                                          |
| `Before`, `After`                 | Relative order relations                                   |

There are also a few convenience constructors:

- `SpanQuery::phrase([..])` for exact phrases
- `SpanQuery::sloppy_phrase([..], max_gaps)` for phrase-with-slop
- `SpanQuery::first_n(inner, n)` for "must occur in the first `n` positions"

## Example

```rust
use boldi_vigna::{Interval, SpanQuery, SpanSolver};

let query = SpanQuery::phrase([0, 1, 2]); // exact phrase over three terms
let mut solver = SpanSolver::new(&query).unwrap();

// Term 0 at positions 0 and 10, term 1 at 1 and 11, term 2 at 2 and 12.
// The term index in SpanQuery::Term(i) corresponds to the slot in this array.
let positions = [vec![0, 10], vec![1, 11], vec![2, 12]];

let matches: Vec<Interval> = solver.intervals(&positions).collect();
assert_eq!(matches, vec![Interval::new(0, 2), Interval::new(10, 12)]);

// Reuse the same compiled solver for another document.
let other_doc = [vec![5], vec![6], vec![20]];
assert_eq!(solver.span_freq(&other_doc), 0);
```

`TermPositions` has blanket impls for common borrowed and owned layouts such as
`Vec<Vec<u32>>`, `Vec<&[u32]>`, `[Vec<u32>; N]`, and `[&[u32]; N]`.

## How it is used in this repository

This workspace uses `boldi-vigna` as the positional evaluation core:

- `tinql` lowers user-facing span syntax into `boldi_vigna::SpanQuery`.
- `postgres` performs document-level candidate selection first, then decodes
  positions only for surviving documents and runs `SpanSolver` to verify the
  positional constraint and compute span frequency / proximity features.

That split is the sweet spot for this crate: let the index narrow down
candidates cheaply, then let minimal-interval semantics do the precise
document-local positional reasoning.

## Implementation notes

- Intervals are inclusive and 0-indexed.
- Outputs are sorted by `(start, end)`.
- Outputs form an antichain: no returned interval strictly contains another.
- `SpanSolver::new()` validates the query tree's arity and term references.
- `TermPositions` is expected to provide sorted ascending positions for each
  referenced term.

## References

The implementation is based primarily on the Boldi-Vigna work on
minimal-interval semantics and antichains of intervals:

1. Paolo Boldi and Sebastiano Vigna, _Efficient lazy algorithms for
   minimal-interval semantics_. SPIRE 2006.
   - DOI: <https://doi.org/10.1007/11880561_12>
   - AIR entry: <https://air.unimi.it/handle/2434/22231>
   - Historical first paper for the lazy interval algorithms. Vigna's papers
     page notes that this version is superseded by the later "optimally lazy"
     treatment.

2. Paolo Boldi and Sebastiano Vigna, _Efficient optimally lazy algorithms for
   minimal-interval semantics_. _Theoretical Computer Science_ 648, 2016.
   - DOI: <https://doi.org/10.1016/j.tcs.2016.07.036>
   - AIR entry: <https://air.unimi.it/handle/2434/433315>
   - arXiv preprint: <https://arxiv.org/abs/0710.1525>
   - This is the main algorithmic reference for the core minimal-interval
     operators implemented here.

3. Paolo Boldi and Sebastiano Vigna, _On the lattice of antichains of finite
   intervals_. _Order_ 38(1), 2018.
   - DOI: <https://doi.org/10.1007/s11083-016-9418-8>
   - AIR entry: <https://air.unimi.it/handle/2434/462753>
   - arXiv preprint: <https://arxiv.org/abs/1510.03675>
   - Useful for the algebraic view of antichains of intervals and for thinking
     about interval-set operators as first-class objects rather than one-off
     query hacks.
