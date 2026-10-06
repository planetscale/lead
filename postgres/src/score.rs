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
use crate::bm25::{
    Bm25Overrides, DenseRatio, ScoreStopWords, ScoringTermInput, TermScorer, TermSetEdit,
    compile_scoring_terms, sum_scores_in_order,
};
use crate::tinql::parse_tinql_to_query;
use pgrx::iter::TableIterator;
use pgrx::{
    FromDatum, Internal, IntoDatum, PgBox, PgList, PgMemoryContexts, PgRelation, Spi, default,
    name, pg_extern, pg_guard, pg_sys,
};
use rustc_hash::FxHashMap;
use std::ffi::{CStr, CString, c_void};
use tinql::runtime::{Query, SpanTermSlot, evaluate, tokenize_doc};
use tokenizer::{CompiledTokenizerPipeline, Tokenizer};

/// Which scorer `score_support` rewrote into `score_bound`. It crosses the SQL
/// boundary as the `int4` mode argument.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(i32)]
enum ScoreMode {
    /// `tin.score`: dense-ratio elision and score stop words apply.
    Score = 0,
    /// `tin.full_score`: every query term scores.
    FullScore = 1,
    /// `tin.max_score` on a relation that `tin.score` also scores, under that
    /// call's policy.
    MaxScore = 2,
    /// `tin.max_score` on any other relation, under the `tin.full_score`
    /// policy.
    MaxFullScore = 3,
}

impl ScoreMode {
    fn is_full(self) -> bool {
        matches!(self, Self::FullScore | Self::MaxFullScore)
    }

    fn is_max(self) -> bool {
        matches!(self, Self::MaxScore | Self::MaxFullScore)
    }
}

impl TryFrom<i32> for ScoreMode {
    type Error = i32;

    fn try_from(mode: i32) -> Result<Self, i32> {
        match mode {
            0 => Ok(Self::Score),
            1 => Ok(Self::FullScore),
            2 => Ok(Self::MaxScore),
            3 => Ok(Self::MaxFullScore),
            _ => Err(mode),
        }
    }
}

/// One tin index that a `score_bound` call site scores rows against, with the
/// searches bound to it. `None` stands for a NULL search expression, which
/// matches no rows. A required group's searches restrict every output row.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GroupKey {
    index_oid: u32,
    queries: Option<Vec<String>>,
    required: bool,
}

/// Identifies the searches that a `score_bound` call site scores rows
/// against. The search texts can vary per row, as in `t.body ==> q.text`
/// driven by a join, so a call site may hold several of these.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CorpusKey {
    heap_oid: u32,
    groups: Vec<GroupKey>,
    mode: ScoreMode,
    dense: u32,
    k1: Option<u32>,
    b: Option<u32>,
    add: Option<Vec<String>>,
    replace: Option<Vec<String>>,
}

/// Corpus-wide statistics for the searches bound to one tin index: the
/// retained terms with N, avgdl, and df baked into their scorers.
struct GroupCorpus {
    tokenizer: CompiledTokenizerPipeline,
    query: Query,
    scorers: Vec<(String, TermScorer)>,
}

impl GroupCorpus {
    /// Scores `document` against this index's searches. A document they do
    /// not match, which another index's searches admitted, gets no score.
    fn score(&self, document: &str) -> Option<f32> {
        evaluate(&self.query, &tokenize_doc(document, &self.tokenizer))
            .unwrap_or_else(|error| pgrx::error!("tin score query evaluation failed: {error}"))
            .matched
            .then(|| score_tokens(&self.scorers, &tokenize(&self.tokenizer, document)))
    }
}

/// The groups of one call site, aligned with its `documents` argument, and
/// the best score among matching rows for the `max_score` modes.
struct ScoreCorpus {
    groups: Vec<Option<GroupCorpus>>,
    max: f32,
}

impl ScoreCorpus {
    /// Returns the score of each group whose searches match the row.
    fn group_scores(&self, documents: &[Option<String>]) -> Vec<(usize, f32)> {
        self.groups
            .iter()
            .zip(documents)
            .enumerate()
            .filter_map(|(position, (group, document))| {
                Some((position, group.as_ref()?.score(document.as_deref()?)?))
            })
            .collect()
    }
}

/// Sums per-index scores the way tin does: exactly, rounding once to `f32`,
/// so the total does not depend on the order of the indexes.
fn sum_group_scores(scores: impl IntoIterator<Item = f32>) -> f32 {
    scores
        .into_iter()
        .fold(0.0_f64, |total, score| total + f64::from(score)) as f32
}

type CorpusMemo = FxHashMap<CorpusKey, ScoreCorpus>;

fn score_context_error(function: &str) -> ! {
    pgrx::error!("{function} requires a tin index scan and cannot be used in this query context")
}

#[pg_extern(immutable, parallel_unsafe)]
fn full_score(ctid: pg_sys::ItemPointerData) -> Option<f32> {
    let _ = ctid;
    score_context_error("tin.full_score()")
}

#[pg_extern(name = "full_score", immutable, parallel_unsafe)]
fn full_score_with_bm25(
    ctid: pg_sys::ItemPointerData,
    k1: Option<f32>,
    b: Option<f32>,
) -> Option<f32> {
    let _ = (ctid, k1, b);
    score_context_error("tin.full_score()")
}

#[pg_extern(immutable, parallel_unsafe)]
fn score(
    ctid: pg_sys::ItemPointerData,
    dense_ratio: default!(Option<f32>, 0.10),
    k1: default!(Option<f32>, "NULL"),
    b: default!(Option<f32>, "NULL"),
    term_add: default!(Option<Vec<String>>, "NULL"),
    term_replace: default!(Option<Vec<String>>, "NULL"),
) -> Option<f32> {
    let _ = (ctid, dense_ratio, k1, b, term_add, term_replace);
    score_context_error("tin.score()")
}

#[pg_extern(immutable, parallel_unsafe)]
fn max_score(ctid: pg_sys::ItemPointerData) -> Option<f32> {
    let _ = ctid;
    score_context_error("tin.max_score()")
}

fn bits(value: Option<f32>) -> Option<u32> {
    value.map(f32::to_bits)
}

#[pg_extern(volatile, parallel_unsafe)]
#[expect(
    clippy::too_many_arguments,
    reason = "SQL signature used by the scoring support function"
)]
fn score_bound(
    documents: Vec<Option<String>>,
    query: Vec<Option<String>>,
    query_groups: Vec<i32>,
    heap_oid: i32,
    index_oids: Vec<i32>,
    required: Vec<bool>,
    mode: i32,
    dense_ratio: Option<f32>,
    k1: Option<f32>,
    b: Option<f32>,
    term_add: Option<Vec<String>>,
    term_replace: Option<Vec<String>>,
    fcinfo: pg_sys::FunctionCallInfo,
) -> Option<f32> {
    let mode = ScoreMode::try_from(mode)
        .unwrap_or_else(|mode| pgrx::error!("tin.score_bound(): unknown score mode {mode}"));
    let mut groups = index_oids
        .iter()
        .zip(&required)
        .map(|(&index_oid, &required)| GroupKey {
            index_oid: index_oid as u32,
            queries: Some(Vec::new()),
            required,
        })
        .collect::<Vec<_>>();
    for (search, group) in query.into_iter().zip(query_groups) {
        let group = &mut groups[group as usize];
        group.queries = group
            .queries
            .take()
            .zip(search)
            .map(|(mut queries, search)| {
                queries.push(search);
                queries
            });
    }
    let key = CorpusKey {
        heap_oid: heap_oid as u32,
        groups,
        mode,
        dense: dense_ratio.unwrap_or(DenseRatio::DEFAULT).to_bits(),
        k1: bits(k1),
        b: bits(b),
        add: term_add.clone(),
        replace: term_replace.clone(),
    };
    let memo = unsafe { corpus_memo(fcinfo) };
    let corpus = memo
        .entry(key)
        .or_insert_with_key(|key| build_corpus(key, k1, b, term_add, term_replace));
    if mode.is_max() {
        return Some(corpus.max);
    }
    // Like tin, a row that no search admits, only another qual such as the
    // `id = 8` of `body ==> 'beer' OR id = 8`, has no score.
    let scores = corpus.group_scores(&documents);
    (!scores.is_empty()).then(|| sum_group_scores(scores.into_iter().map(|(_, score)| score)))
}

/// Returns the corpora memoized on this call site's `FmgrInfo`. The executor
/// builds that `FmgrInfo`, and the `fn_mcxt` it lives in, for one execution
/// of the statement, so a corpus never outlives the snapshot it was read
/// under.
unsafe fn corpus_memo<'a>(fcinfo: pg_sys::FunctionCallInfo) -> &'a mut CorpusMemo {
    unsafe {
        let flinfo = (*fcinfo).flinfo;
        if (*flinfo).fn_extra.is_null() {
            (*flinfo).fn_extra = PgMemoryContexts::For((*flinfo).fn_mcxt)
                .leak_and_drop_on_delete(CorpusMemo::default())
                .cast();
        }
        &mut *(*flinfo).fn_extra.cast::<CorpusMemo>()
    }
}

fn build_corpus(
    key: &CorpusKey,
    k1: Option<f32>,
    b: Option<f32>,
    term_add: Option<Vec<String>>,
    term_replace: Option<Vec<String>>,
) -> ScoreCorpus {
    let heap_oid = pg_sys::Oid::from(key.heap_oid);
    let indexes = key
        .groups
        .iter()
        .map(|group| {
            let index = unsafe {
                PgRelation::with_lock(
                    pg_sys::Oid::from(group.index_oid),
                    pg_sys::AccessShareLock as _,
                )
            };
            if unsafe { pg_sys::IndexGetRelation(index.oid(), false) } != heap_oid {
                pgrx::error!("tin score index no longer belongs to the scored relation");
            }
            index
        })
        .collect::<Vec<_>>();
    let rows = load_documents(
        heap_oid,
        &indexes.iter().map(PgRelation::oid).collect::<Vec<_>>(),
    );
    let groups = key
        .groups
        .iter()
        .zip(&indexes)
        .enumerate()
        .map(|(position, (group, index))| {
            let queries = group.queries.as_deref()?;
            let documents = rows.iter().filter_map(|row| row[position].as_deref());
            Some(build_group(
                key,
                index,
                queries,
                documents,
                k1,
                b,
                term_add.clone(),
                term_replace.clone(),
            ))
        })
        .collect::<Vec<_>>();
    let mut corpus = ScoreCorpus { groups, max: 0.0 };
    if key.mode.is_max() {
        for (row, documents) in rows.iter().enumerate() {
            if row.is_multiple_of(10) {
                pgrx::check_for_interrupts!();
            }
            let scores = corpus.group_scores(documents);
            let complete = key.groups.iter().enumerate().all(|(position, group)| {
                !group.required || scores.iter().any(|&(matched, _)| matched == position)
            });
            if complete && !scores.is_empty() {
                let score = sum_group_scores(scores.into_iter().map(|(_, score)| score));
                corpus.max = corpus.max.max(score);
            }
        }
    }
    corpus
}

#[expect(
    clippy::too_many_arguments,
    reason = "one index's share of the scoring call's arguments"
)]
fn build_group<'a>(
    key: &CorpusKey,
    index: &PgRelation,
    queries: &[String],
    documents: impl Iterator<Item = &'a str>,
    k1: Option<f32>,
    b: Option<f32>,
    term_add: Option<Vec<String>>,
    term_replace: Option<Vec<String>>,
) -> GroupCorpus {
    let full = key.mode.is_full();
    let tokenizer = unsafe { crate::options::tokenizer(index.as_ptr()) };
    let defaults = unsafe { crate::options::bm25(index.as_ptr()) };
    let stop_csv = unsafe { crate::options::score_stop_words(index.as_ptr()) };
    let params = Bm25Overrides { k1, b }
        .resolve(defaults)
        .checked()
        .unwrap_or_else(|error| pgrx::error!("tin score parameters: {error}"));
    let dense = DenseRatio::new(Some(f32::from_bits(key.dense)));
    if !full && !dense.is_valid() {
        pgrx::error!("dense_ratio must be finite and non-negative");
    }
    let query = crate::operator::parse_searches(queries, &tokenizer);
    let mut inputs = Vec::new();
    collect_score_terms(&query, 1.0, false, &mut inputs);
    let edit = TermSetEdit::from_bound_arrays(term_add, term_replace)
        .unwrap_or_else(|error| pgrx::error!("tin.score(): {error}"))
        .analyzed_with(|text| {
            tokenizer
                .tokenize(text)
                .map(|token| token.text.into_owned())
                .collect::<Vec<_>>()
        });
    let stop = if full {
        None
    } else {
        stop_csv.as_deref().and_then(ScoreStopWords::from_csv)
    };
    let terms = compile_scoring_terms(inputs, &edit, stop.as_ref());
    let tokenized = tokenize_documents(documents, &tokenizer);
    let total_docs = tokenized.len() as u64;
    let average_length = if total_docs == 0 {
        1.0
    } else {
        tokenized.iter().map(Vec::len).sum::<usize>() as f32 / total_docs as f32
    };
    let mut scorers = Vec::new();
    for term in terms {
        let df = tokenized
            .iter()
            .filter(|tokens| tokens.iter().any(|token| token == term.text()))
            .count() as u64;
        let ratio = (!full).then_some(dense);
        if !term.is_retained(df, df, total_docs, ratio) {
            continue;
        }
        let scorer =
            TermScorer::from_statistics(total_docs, df, term.boost(), params, average_length)
                .unwrap_or_else(|error| pgrx::error!("tin score parameters: {error}"));
        scorers.push((term.text().to_owned(), scorer));
    }
    GroupCorpus {
        tokenizer,
        query,
        scorers,
    }
}

fn score_tokens(scorers: &[(String, TermScorer)], tokens: &[String]) -> f32 {
    sum_scores_in_order(scorers.iter().map(|(term, scorer)| {
        let tf = tokens.iter().filter(|token| *token == term).count() as u32;
        if tf == 0 {
            0.0
        } else {
            scorer.score_count(tf, tokens.len() as u32)
        }
    }))
}

/// Reads the documents of the given tin indexes on `heap_oid`, one row per
/// heap row that any of them covers, with each index's document in its
/// position, or `None` where the row is NULL or outside that index.
fn load_documents(heap_oid: pg_sys::Oid, index_oids: &[pg_sys::Oid]) -> Vec<Vec<Option<String>>> {
    unsafe {
        let relname = pg_sys::get_rel_name(heap_oid);
        let namespace = pg_sys::get_namespace_name(pg_sys::get_rel_namespace(heap_oid));
        if relname.is_null() || namespace.is_null() {
            pgrx::error!("tin score relation no longer exists");
        }
        let qualified = pg_sys::quote_qualified_identifier(namespace, relname);
        let mut columns = Vec::new();
        let mut conditions = Vec::new();
        for index_oid in index_oids {
            let index_sql = format!(
                "SELECT CASE WHEN i.indkey[0] = 0 \
                 THEN pg_catalog.pg_get_expr(i.indexprs, i.indrelid) \
                 ELSE pg_catalog.quote_ident(a.attname) END, \
                 pg_catalog.pg_get_expr(i.indpred, i.indrelid) \
                 FROM pg_catalog.pg_index i \
                 LEFT JOIN pg_catalog.pg_attribute a \
                   ON a.attrelid=i.indrelid AND a.attnum=i.indkey[0] \
                 WHERE i.indexrelid={}::oid AND i.indrelid={}::oid",
                index_oid.to_u32(),
                heap_oid.to_u32(),
            );
            let mut definition = select_read_only(&index_sql, "tin score index lookup failed")
                .pop()
                .unwrap_or_else(|| pgrx::error!("tin score index lookup failed: index not found"))
                .into_iter();
            let expression = definition
                .next()
                .flatten()
                .unwrap_or_else(|| pgrx::error!("tin score index expression no longer exists"));
            let predicate = definition
                .next()
                .flatten()
                .map(|predicate| format!(" AND ({predicate})"))
                .unwrap_or_default();
            let condition = format!("({expression}) IS NOT NULL{predicate}");
            columns.push(format!(
                "CASE WHEN {condition} THEN ({expression})::text END"
            ));
            conditions.push(format!("({condition})"));
        }
        let sql = format!(
            "SELECT {} FROM {} WHERE {}",
            columns.join(", "),
            CStr::from_ptr(qualified).to_string_lossy(),
            conditions.join(" OR "),
        );
        select_read_only(&sql, "tin score corpus scan failed")
    }
}

/// Runs a query through SPI in read-only mode and returns its rows as text.
/// Read-only execution keeps the calling statement's snapshot, so the corpus
/// matches the rows that statement sees. Writable execution would take a new
/// snapshot for each query under READ COMMITTED, and pgrx picks writable
/// execution once the transaction has an XID.
fn select_read_only(sql: &str, failure: &str) -> Vec<Vec<Option<String>>> {
    let sql = CString::new(sql).unwrap_or_else(|_| pgrx::error!("{failure}: query contains NUL"));
    Spi::connect(|_| unsafe {
        let status = pg_sys::SPI_execute(sql.as_ptr(), true, 0);
        if status != pg_sys::SPI_OK_SELECT as i32 {
            pgrx::error!("{failure}: SPI_execute returned {status}");
        }
        let table = &*pg_sys::SPI_tuptable;
        let columns = (*table.tupdesc).natts;
        (0..pg_sys::SPI_processed as usize)
            .map(|row| {
                let tuple = *table.vals.add(row);
                (1..=columns)
                    .map(|column| {
                        let mut is_null = false;
                        let datum =
                            pg_sys::SPI_getbinval(tuple, table.tupdesc, column, &mut is_null);
                        String::from_datum(datum, is_null)
                    })
                    .collect()
            })
            .collect()
    })
}

fn tokenize_documents<'a>(
    documents: impl Iterator<Item = &'a str>,
    tokenizer: &CompiledTokenizerPipeline,
) -> Vec<Vec<String>> {
    documents
        .enumerate()
        .map(|(row, document)| {
            if row.is_multiple_of(10) {
                pgrx::check_for_interrupts!();
            }
            tokenize(tokenizer, document)
        })
        .collect()
}

fn tokenize(tokenizer: &CompiledTokenizerPipeline, document: &str) -> Vec<String> {
    tokenizer
        .tokenize(document)
        .map(|token| token.text.into_owned())
        .collect()
}

fn collect_score_terms<'a>(
    query: &'a Query,
    boost: f32,
    explicitly_boosted: bool,
    out: &mut Vec<ScoringTermInput<'a>>,
) {
    let mut push = |text: &'a str| {
        out.push(ScoringTermInput {
            text,
            boost,
            explicitly_boosted,
        });
    };
    match query {
        Query::Term(text) | Query::Fuzzy { term: text, .. } => push(text),
        Query::Span { term_slots, .. } | Query::SpanExpr { term_slots, .. } => {
            for slot in term_slots {
                if let SpanTermSlot::Term(text) | SpanTermSlot::Fuzzy { term: text, .. } = slot {
                    push(text);
                }
            }
        }
        Query::And(left, right) | Query::Or(left, right) => {
            collect_score_terms(left, boost, explicitly_boosted, out);
            collect_score_terms(right, boost, explicitly_boosted, out);
        }
        Query::Conjunction(children)
        | Query::Disjunction { children, .. }
        | Query::AtLeast { children, .. } => {
            for child in children {
                collect_score_terms(child, boost, explicitly_boosted, out);
            }
        }
        Query::Not(inner) => collect_score_terms(inner, boost, explicitly_boosted, out),
        Query::Boost { factor, inner } => {
            collect_score_terms(inner, boost * *factor, true, out);
        }
        Query::MatchAll | Query::Regex(_) | Query::Range { .. } => {}
    }
}

#[pg_extern(stable, parallel_unsafe)]
fn score_inspect(
    index: Option<PgRelation>,
    query: Option<&str>,
    dense_ratio: default!(Option<f32>, 0.10),
    term_add: default!(Option<Vec<Option<String>>>, "NULL"),
    term_replace: default!(Option<Vec<Option<String>>>, "NULL"),
) -> TableIterator<'static, (name!(term, String), name!(weight, f32))> {
    let (Some(index), Some(query)) = (index, query) else {
        return TableIterator::new(Vec::new());
    };
    let tin_name = CString::new("tin").expect("static access method name is valid");
    let tin_am = unsafe { pg_sys::get_index_am_oid(tin_name.as_ptr(), false) };
    if unsafe { (*(*index.as_ptr()).rd_rel).relam } != tin_am {
        pgrx::error!("tin.score_inspect() requires a tin index");
    }
    let unwrap = |which: &str, values: Option<Vec<Option<String>>>| {
        values.map(|values| {
            values
                .into_iter()
                .map(|value| {
                    value.unwrap_or_else(|| {
                        pgrx::error!("tin.score_inspect() {which} array elements must not be NULL")
                    })
                })
                .collect::<Vec<_>>()
        })
    };
    let heap_oid = unsafe { pg_sys::IndexGetRelation(index.oid(), false) };
    let acl = unsafe {
        pg_sys::pg_class_aclcheck(heap_oid, pg_sys::GetUserId(), pg_sys::ACL_SELECT as _)
    };
    if acl != pg_sys::AclResult::ACLCHECK_OK {
        unsafe {
            pg_sys::aclcheck_error(
                acl,
                pg_sys::ObjectType::OBJECT_TABLE,
                pg_sys::get_rel_name(heap_oid),
            )
        };
    }
    let tokenizer = unsafe { crate::options::tokenizer(index.as_ptr()) };
    let parsed = parse_tinql_to_query(query, &tokenizer)
        .unwrap_or_else(|error| pgrx::error!("tin.score_inspect() query error: {error}"));
    let mut inputs = Vec::new();
    collect_score_terms(&parsed, 1.0, false, &mut inputs);
    let edit = TermSetEdit::from_bound_arrays(
        unwrap("term_add", term_add),
        unwrap("term_replace", term_replace),
    )
    .unwrap_or_else(|error| pgrx::error!("tin.score_inspect(): {error}"))
    .analyzed_with(|text| {
        tokenizer
            .tokenize(text)
            .map(|t| t.text.into_owned())
            .collect::<Vec<_>>()
    });
    let stop_csv = unsafe { crate::options::score_stop_words(index.as_ptr()) };
    let stop = stop_csv.as_deref().and_then(ScoreStopWords::from_csv);
    let terms = compile_scoring_terms(inputs, &edit, stop.as_ref());
    let ratio = DenseRatio::new(dense_ratio);
    if !ratio.is_valid() {
        pgrx::error!("dense_ratio must be finite and non-negative");
    }
    let docs = load_documents(heap_oid, &[index.oid()]);
    let tokenized = tokenize_documents(docs.iter().filter_map(|row| row[0].as_deref()), &tokenizer);
    let n = tokenized.len() as u64;
    let rows = terms
        .into_iter()
        .filter_map(|term| {
            let df = tokenized
                .iter()
                .filter(|doc| doc.iter().any(|t| t == term.text()))
                .count() as u64;
            term.is_retained(df, df, n, Some(ratio))
                .then(|| (term.text().to_owned(), term.boost()))
        })
        .collect::<Vec<_>>();
    TableIterator::new(rows)
}

struct QualBinding {
    matches: Vec<(*mut pg_sys::Node, *mut pg_sys::Node)>,
}

/// Search expressions under a `NOT` exclude rows rather than describe them, so
/// they contribute neither scoring terms nor highlight marks.
pub(crate) fn is_negation(node: *mut pg_sys::Node) -> bool {
    unsafe {
        (*node).type_ == pg_sys::NodeTag::T_BoolExpr
            && (*node.cast::<pg_sys::BoolExpr>()).boolop == pg_sys::BoolExprType::NOT_EXPR
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn find_qual(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
    if node.is_null() || unsafe { (*node).type_ } == pg_sys::NodeTag::T_Query || is_negation(node) {
        return false;
    }
    let binding = unsafe { &mut *context.cast::<QualBinding>() };
    if let Some(search) = unsafe { crate::operator::search_operands(node) } {
        binding.matches.push(search);
    }
    unsafe { pg_sys::expression_tree_walker(node, Some(find_qual), context) }
}

struct VarnoBinding {
    varno: i32,
    seen: bool,
    valid: bool,
}

#[pg_guard]
unsafe extern "C-unwind" fn collect_varno(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
    if node.is_null() {
        return false;
    }
    let binding = unsafe { &mut *context.cast::<VarnoBinding>() };
    if unsafe { (*node).type_ } == pg_sys::NodeTag::T_Var {
        let var = unsafe { &*node.cast::<pg_sys::Var>() };
        if var.varlevelsup != 0 || (binding.seen && binding.varno != var.varno) {
            binding.valid = false;
        } else {
            binding.varno = var.varno;
            binding.seen = true;
        }
        return false;
    }
    unsafe { pg_sys::expression_tree_walker(node, Some(collect_varno), context) }
}

/// Returns the range table entry that every `Var` in `node` belongs to, or
/// `None` when the node spans several entries, an outer query level, or none.
pub(crate) unsafe fn single_varno(node: *mut pg_sys::Node) -> Option<i32> {
    let mut binding = VarnoBinding {
        varno: 0,
        seen: false,
        valid: true,
    };
    unsafe { collect_varno(node, (&raw mut binding).cast()) };
    (binding.valid && binding.seen).then_some(binding.varno)
}

/// Collects the qual expressions that every output row's entry for `varno`
/// satisfies, and reports whether `varno` lies under `node`. WHERE and inner
/// join quals restrict every relation below them. An outer join's ON clause
/// restricts only its nullable side, since the preserved side keeps its rows
/// whether or not the clause holds.
unsafe fn collect_restrictions(
    node: *mut pg_sys::Node,
    varno: i32,
    quals: &mut Vec<*mut pg_sys::Node>,
) -> bool {
    if node.is_null() {
        return false;
    }
    unsafe {
        match (*node).type_ {
            pg_sys::NodeTag::T_RangeTblRef => {
                (*node.cast::<pg_sys::RangeTblRef>()).rtindex == varno
            }
            pg_sys::NodeTag::T_FromExpr => {
                let from = &*node.cast::<pg_sys::FromExpr>();
                let mut contains = false;
                for child in PgList::<pg_sys::Node>::from_pg(from.fromlist).iter_ptr() {
                    contains |= collect_restrictions(child, varno, quals);
                }
                if contains {
                    quals.push(from.quals);
                }
                contains
            }
            pg_sys::NodeTag::T_JoinExpr => {
                let join = &*node.cast::<pg_sys::JoinExpr>();
                let left = collect_restrictions(join.larg, varno, quals);
                let right = collect_restrictions(join.rarg, varno, quals);
                let restricted = match join.jointype {
                    pg_sys::JoinType::JOIN_INNER => left || right,
                    pg_sys::JoinType::JOIN_LEFT => right,
                    pg_sys::JoinType::JOIN_RIGHT | pg_sys::JoinType::JOIN_SEMI => left,
                    _ => false,
                };
                if restricted {
                    quals.push(join.quals);
                }
                left || right
            }
            _ => false,
        }
    }
}

/// Returns the clauses, in the form `predicate_implied_by` compares against an
/// index predicate, that restrict relation `varno` in `parse`. Scoring
/// support runs while the target list is preprocessed, before the jointree
/// quals are simplified, so they are simplified here the way the planner
/// would. Outer join nulling marks are dropped: a row that a join null-extends
/// has no document to score or highlight, and every other row carries its
/// own values.
unsafe fn restriction_clauses(parse: *mut pg_sys::Query, varno: i32) -> *mut pg_sys::List {
    unsafe {
        let mut quals = Vec::new();
        collect_restrictions((*parse).jointree.cast(), varno, &mut quals);
        let every_relation = pg_sys::bms_add_range(
            std::ptr::null_mut(),
            1,
            pg_sys::list_length((*parse).rtable),
        );
        let mut clauses = std::ptr::null_mut();
        for qual in quals {
            if qual.is_null() {
                continue;
            }
            let qual = pg_sys::copyObjectImpl(qual.cast()).cast::<pg_sys::Node>();
            let qual = pg_sys::remove_nulling_relids(qual, every_relation, std::ptr::null());
            // Without a planner root, support functions decline to rewrite,
            // so this cannot reenter scoring support.
            let qual = pg_sys::eval_const_expressions(std::ptr::null_mut(), qual);
            let qual = pg_sys::canonicalize_qual(qual.cast(), false);
            clauses = pg_sys::list_concat(clauses, pg_sys::make_ands_implicit(qual));
        }
        clauses
    }
}

/// Returns the tin index that scoring and highlighting bind `operand` to, the
/// way tin selects one: a partial index qualifies only when the query's quals
/// on the relation imply its predicate. Lead's indexes store no pages, so of
/// the qualifying indexes the newest partial one wins, then the newest full
/// one.
pub(crate) unsafe fn find_matching_tin_index(
    parse: *mut pg_sys::Query,
    heap_oid: pg_sys::Oid,
    query_varno: i32,
    operand: *mut pg_sys::Node,
) -> Option<pg_sys::Oid> {
    // The operand is normalized to varno 1 below to compare it against stored
    // index expressions, so quals on other relations have to be rejected here.
    if unsafe { single_varno(operand) } != Some(query_varno) {
        return None;
    }
    let tin_name = CString::new("tin").expect("static access method name is valid");
    let tin_am = unsafe { pg_sys::get_index_am_oid(tin_name.as_ptr(), false) };
    let normalized = unsafe { pg_sys::copyObjectImpl(operand.cast()).cast::<pg_sys::Node>() };
    unsafe { pg_sys::ChangeVarNodes(normalized, query_varno, 1, 0) };
    let normalized = unsafe { pg_sys::strip_implicit_coercions(normalized) };
    let heap = unsafe { pg_sys::table_open(heap_oid, pg_sys::AccessShareLock as _) };
    let indexes = unsafe { PgList::<pg_sys::Oid>::from_pg(pg_sys::RelationGetIndexList(heap)) };
    let mut clauses = None;
    let mut matched: Option<(bool, u32)> = None;
    for index_oid in indexes.iter_oid() {
        let index = unsafe { pg_sys::index_open(index_oid, pg_sys::AccessShareLock as _) };
        let metadata = unsafe { &*(*index).rd_index };
        let is_tin = unsafe { (*(*index).rd_rel).relam } == tin_am;
        let suitable =
            is_tin && metadata.indisvalid && metadata.indisready && metadata.indnkeyatts == 1;
        let matches = if suitable {
            let key = unsafe { *metadata.indkey.values.as_ptr() };
            if key > 0 {
                !normalized.is_null()
                    && unsafe { (*normalized).type_ } == pg_sys::NodeTag::T_Var
                    && unsafe {
                        let var = &*normalized.cast::<pg_sys::Var>();
                        var.varno == 1 && var.varlevelsup == 0 && var.varattno == key
                    }
            } else {
                let expressions = unsafe { pg_sys::RelationGetIndexExpressions(index) };
                if unsafe { pg_sys::list_length(expressions) } != 1 {
                    false
                } else {
                    let indexed =
                        unsafe { pg_sys::list_nth(expressions, 0).cast::<pg_sys::Node>() };
                    let indexed = unsafe { pg_sys::strip_implicit_coercions(indexed) };
                    unsafe { pg_sys::equal(normalized.cast(), indexed.cast()) }
                }
            }
        } else {
            false
        };
        let predicate = if matches {
            unsafe { pg_sys::RelationGetIndexPredicate(index) }
        } else {
            std::ptr::null_mut()
        };
        let partial = !predicate.is_null();
        let eligible = matches
            && (!partial
                || unsafe {
                    pg_sys::ChangeVarNodes(predicate.cast(), 1, query_varno, 0);
                    let clauses =
                        *clauses.get_or_insert_with(|| restriction_clauses(parse, query_varno));
                    pg_sys::predicate_implied_by(predicate, clauses, false)
                });
        unsafe { pg_sys::index_close(index, pg_sys::AccessShareLock as _) };
        let choice = (partial, index_oid.to_u32());
        if eligible && matched.is_none_or(|best| choice > best) {
            matched = Some(choice);
        }
    }
    unsafe { pg_sys::table_close(heap, pg_sys::AccessShareLock as _) };
    matched.map(|(_, index_oid)| pg_sys::Oid::from(index_oid))
}

/// Refuses scoring when none of the relation's search expressions has a tin
/// index to score against, with the error tin raises for the same query.
unsafe fn refuse_unindexed_scoring(
    rte: *mut pg_sys::RangeTblEntry,
    varno: i32,
    operands: &[*mut pg_sys::Node],
) -> ! {
    let expressions = unsafe {
        let context = pg_sys::deparse_context_for(pg_sys::get_rel_name((*rte).relid), (*rte).relid);
        operands
            .iter()
            .map(|&operand| deparse_search_expression(operand, varno, context))
            .collect::<Option<Vec<_>>>()
    };
    let detail = match expressions {
        Some(mut expressions) if !expressions.is_empty() => {
            expressions.sort_unstable();
            expressions.dedup();
            format!("No matching tin index for: {}.", expressions.join(", "))
        }
        _ => "One or more search expressions have no matching tin index.".to_owned(),
    };
    pg_sys::panic::ErrorReport::new(
        pg_sys::errcodes::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
        "cannot compute scores for this query",
        pgrx::function_name!(),
    )
    .set_detail(detail)
    .set_hint("Add a matching USING tin index, or check the definition of an existing index.")
    .report(pgrx::PgLogLevel::ERROR);
    unreachable!("ERROR reports do not return")
}

/// The searches on the scored relation that bind to one tin index.
struct SearchGroup {
    index_oid: pg_sys::Oid,
    document: *mut pg_sys::Node,
    queries: Vec<*mut pg_sys::Node>,
    /// Whether every output row satisfies one of these searches.
    required: bool,
}

/// Groups the relation's searches by the tin index each binds to, in index
/// OID order, so that a row's score sums the same per-index scores whatever
/// the order of the quals. Searches without a tin index do not score, and
/// scoring is refused when none of them has one, as tin does.
unsafe fn bind_search_groups(
    parse: *mut pg_sys::Query,
    rte: *mut pg_sys::RangeTblEntry,
    varno: i32,
    searches: &[(*mut pg_sys::Node, *mut pg_sys::Node)],
) -> Vec<SearchGroup> {
    let mut bound: Vec<(*mut pg_sys::Node, Option<pg_sys::Oid>)> = Vec::new();
    let mut groups: Vec<SearchGroup> = Vec::new();
    let mut unindexed = Vec::new();
    for &(document, query) in searches {
        let index_oid = match bound
            .iter()
            .find(|(operand, _)| unsafe { pg_sys::equal((*operand).cast(), document.cast()) })
        {
            Some(&(_, index_oid)) => index_oid,
            None => {
                let index_oid =
                    unsafe { find_matching_tin_index(parse, (*rte).relid, varno, document) };
                bound.push((document, index_oid));
                index_oid
            }
        };
        let Some(index_oid) = index_oid else {
            unindexed.push(document);
            continue;
        };
        match groups.iter_mut().find(|group| group.index_oid == index_oid) {
            Some(group) => group.queries.push(query),
            None => groups.push(SearchGroup {
                index_oid,
                document,
                queries: vec![query],
                required: false,
            }),
        }
    }
    if groups.is_empty()
        || (!unindexed.is_empty() && unsafe { has_unscannable_search(parse, rte, varno) })
    {
        unsafe { refuse_unindexed_scoring(rte, varno, &unindexed) };
    }
    groups.sort_by_key(|group| group.index_oid.to_u32());
    groups
}

/// Reports whether a search without a tin index is an alternative to one
/// with a tin index, as in `title ==> 'a' OR body ==> 'b'` with only `body`
/// indexed. tin cannot scan such a disjunction, so it cannot score the rows
/// either alternative admits. A search without an index elsewhere only
/// filters the rows that the indexed searches find.
unsafe fn has_unscannable_search(
    parse: *mut pg_sys::Query,
    rte: *mut pg_sys::RangeTblEntry,
    varno: i32,
) -> bool {
    let indexed =
        |node| unsafe { search_index(parse, rte, varno, node).map(|index| index.is_some()) };
    let searches_index = |node: *mut pg_sys::Node| {
        let mut found = false;
        visit_searches(node, &mut |search| found |= indexed(search) == Some(true));
        found
    };
    let mut unscannable = false;
    let clauses = unsafe { restriction_clauses(parse, varno) };
    for clause in unsafe { PgList::<pg_sys::Node>::from_pg(clauses) }.iter_ptr() {
        visit_disjunctions(clause, &mut |arms| {
            unscannable |= arms.iter().enumerate().any(|(position, &arm)| {
                indexed(arm) == Some(false)
                    && arms.iter().enumerate().any(|(other, &alternative)| {
                        other != position && searches_index(alternative)
                    })
            });
        });
    }
    unscannable
}

/// Calls `visit` with the arms of every `OR` in `node` outside a `NOT`.
fn visit_disjunctions(node: *mut pg_sys::Node, visit: &mut impl FnMut(&[*mut pg_sys::Node])) {
    if node.is_null()
        || is_negation(node)
        || unsafe { (*node).type_ } != pg_sys::NodeTag::T_BoolExpr
    {
        return;
    }
    let expression = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
    let arms = unsafe { PgList::<pg_sys::Node>::from_pg(expression.args) }
        .iter_ptr()
        .collect::<Vec<_>>();
    if expression.boolop == pg_sys::BoolExprType::OR_EXPR {
        visit(&arms);
    }
    for arm in arms {
        visit_disjunctions(arm, visit);
    }
}

/// Calls `visit` with every node in `node` outside a `NOT`.
fn visit_searches(node: *mut pg_sys::Node, visit: &mut impl FnMut(*mut pg_sys::Node)) {
    if node.is_null() || is_negation(node) {
        return;
    }
    visit(node);
    if unsafe { (*node).type_ } == pg_sys::NodeTag::T_BoolExpr {
        let expression = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
        for arm in unsafe { PgList::<pg_sys::Node>::from_pg(expression.args) }.iter_ptr() {
            visit_searches(arm, visit);
        }
    }
}

/// Marks the groups that the quals restricting every output row require a
/// match of. `max_score` considers only documents that match every such
/// group.
unsafe fn mark_required_groups(
    parse: *mut pg_sys::Query,
    rte: *mut pg_sys::RangeTblEntry,
    varno: i32,
    groups: &mut [SearchGroup],
) {
    let bound = |node| unsafe { search_index(parse, rte, varno, node) };
    let clauses = unsafe { restriction_clauses(parse, varno) };
    for clause in unsafe { PgList::<pg_sys::Node>::from_pg(clauses) }.iter_ptr() {
        for group in groups.iter_mut() {
            group.required |= requires_index(clause, group.index_oid, &bound);
        }
    }
}

/// Returns the tin index that a search on relation `varno` binds to, with
/// `Some(None)` for a search without one, or `None` when `node` is not a
/// search on the relation.
unsafe fn search_index(
    parse: *mut pg_sys::Query,
    rte: *mut pg_sys::RangeTblEntry,
    varno: i32,
    node: *mut pg_sys::Node,
) -> Option<Option<pg_sys::Oid>> {
    unsafe {
        let (document, _) = crate::operator::search_operands(node)?;
        (single_varno(document) == Some(varno))
            .then(|| find_matching_tin_index(parse, (*rte).relid, varno, document))
    }
}

/// Reports whether every row that satisfies `node` matches a search bound to
/// `index_oid`.
fn requires_index(
    node: *mut pg_sys::Node,
    index_oid: pg_sys::Oid,
    bound: &impl Fn(*mut pg_sys::Node) -> Option<Option<pg_sys::Oid>>,
) -> bool {
    if node.is_null() || is_negation(node) {
        return false;
    }
    if unsafe { (*node).type_ } != pg_sys::NodeTag::T_BoolExpr {
        return bound(node) == Some(Some(index_oid));
    }
    let expression = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
    let arms = unsafe { PgList::<pg_sys::Node>::from_pg(expression.args) };
    let mut arms = arms.iter_ptr();
    if expression.boolop == pg_sys::BoolExprType::OR_EXPR {
        arms.all(|arm| requires_index(arm, index_oid, bound))
    } else {
        arms.any(|arm| requires_index(arm, index_oid, bound))
    }
}

/// Renders a search expression on relation `varno` against the relation's
/// own name. Returns `None` for expressions a single-relation deparse context
/// cannot describe.
unsafe fn deparse_search_expression(
    expression: *mut pg_sys::Node,
    varno: i32,
    context: *mut pg_sys::List,
) -> Option<String> {
    #[pg_guard]
    unsafe extern "C-unwind" fn prepare(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
        if node.is_null() {
            return false;
        }
        unsafe {
            match (*node).type_ {
                pg_sys::NodeTag::T_Var => {
                    let var = &mut *node.cast::<pg_sys::Var>();
                    var.varno = 1;
                    var.varnosyn = 0;
                    var.varattnosyn = 0;
                    var.varnullingrels = std::ptr::null_mut();
                    false
                }
                pg_sys::NodeTag::T_Param => {
                    (*node.cast::<pg_sys::Param>()).paramkind != pg_sys::ParamKind::PARAM_EXTERN
                }
                pg_sys::NodeTag::T_PlaceHolderVar
                | pg_sys::NodeTag::T_SubLink
                | pg_sys::NodeTag::T_SubPlan
                | pg_sys::NodeTag::T_AlternativeSubPlan => true,
                _ => pg_sys::expression_tree_walker(node, Some(prepare), context),
            }
        }
    }
    unsafe {
        if single_varno(expression) != Some(varno) {
            return None;
        }
        let copied = pg_sys::copyObjectImpl(expression.cast()).cast();
        if prepare(copied, std::ptr::null_mut()) {
            return None;
        }
        let rendered = pg_sys::deparse_expression(copied, context, true, false);
        Some(CStr::from_ptr(rendered).to_string_lossy().into_owned())
    }
}

/// The `dense_ratio`, `term_add`, and `term_replace` arguments of a
/// `tin.score` call, which a scan shares between its `tin.score` calls and
/// the `tin.max_score` that adapts to them.
const SCAN_ARGUMENTS: [(i32, &str); 3] = [(1, "dense_ratio"), (4, "term_add"), (5, "term_replace")];

/// The scoring calls on one relation, found in the query either as written
/// or already rewritten into `score_bound` by earlier clauses.
struct ScoringCalls {
    ctid: *const pg_sys::Var,
    documents: *mut pg_sys::Node,
    support: pg_sys::Oid,
    bound: pg_sys::Oid,
    full: bool,
    /// The scan arguments of each `tin.score` call, simplified.
    score: Vec<[*mut pg_sys::Node; 3]>,
}

#[pg_guard]
unsafe extern "C-unwind" fn collect_scoring_calls(
    node: *mut pg_sys::Node,
    context: *mut c_void,
) -> bool {
    unsafe {
        if node.is_null() || (*node).type_ == pg_sys::NodeTag::T_Query {
            return false;
        }
        let calls = &mut *context.cast::<ScoringCalls>();
        if (*node).type_ == pg_sys::NodeTag::T_FuncExpr {
            let function = &*node.cast::<pg_sys::FuncExpr>();
            if function.funcid == calls.bound {
                let mode = pg_sys::list_nth(function.args, 6).cast::<pg_sys::Const>();
                if (*mode).xpr.type_ == pg_sys::NodeTag::T_Const
                    && pg_sys::equal(pg_sys::list_nth(function.args, 0), calls.documents.cast())
                {
                    let argument = |position| pg_sys::list_nth(function.args, position).cast();
                    match ScoreMode::try_from((*mode).constvalue.value() as i32) {
                        Ok(ScoreMode::FullScore) => calls.full = true,
                        Ok(ScoreMode::Score) => {
                            calls.score.push([argument(7), argument(10), argument(11)])
                        }
                        _ => {}
                    }
                }
            } else if pg_sys::get_func_support(function.funcid) == calls.support {
                let name = CStr::from_ptr(pg_sys::get_func_name(function.funcid)).to_bytes();
                if name == b"score" || name == b"full_score" {
                    let args = expanded_arguments(function);
                    if pg_sys::equal(pg_sys::list_nth(args, 0), calls.ctid.cast()) {
                        if name == b"full_score" {
                            calls.full = true;
                        } else {
                            calls.score.push(SCAN_ARGUMENTS.map(|(position, _)| {
                                pg_sys::eval_const_expressions(
                                    std::ptr::null_mut(),
                                    pg_sys::list_nth(args, position).cast(),
                                )
                            }));
                        }
                    }
                }
            }
        }
        pg_sys::expression_tree_walker(node, Some(collect_scoring_calls), context)
    }
}

/// Returns a call's arguments in positional order with defaults filled in,
/// the form the planner gives the support function.
unsafe fn expanded_arguments(function: &pg_sys::FuncExpr) -> *mut pg_sys::List {
    unsafe {
        let tuple = pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::PROCOID as i32,
            pg_sys::Datum::from(function.funcid),
        );
        if tuple.is_null() {
            pgrx::error!(
                "cache lookup failed for function {}",
                function.funcid.to_u32()
            );
        }
        let args = pg_sys::expand_function_arguments(
            pg_sys::copyObjectImpl(function.args.cast()).cast(),
            false,
            function.funcresulttype,
            tuple,
        );
        pg_sys::ReleaseSysCache(tuple);
        args
    }
}

/// Reports whether a disjunction in the quals restricting relation `varno`
/// combines one of its searches with a qual that is not purely searches, as
/// in `body ==> 'a' OR id = 1`. tin scans such a disjunction with several
/// scans, and computes no `max_score` for them under the `tin.score`
/// policy.
unsafe fn has_mixed_disjunction(parse: *mut pg_sys::Query, varno: i32) -> bool {
    let is_search = |node| unsafe {
        crate::operator::search_operands(node)
            .is_some_and(|(document, _)| single_varno(document) == Some(varno))
    };
    let mut mixed = false;
    let clauses = unsafe { restriction_clauses(parse, varno) };
    for clause in unsafe { PgList::<pg_sys::Node>::from_pg(clauses) }.iter_ptr() {
        visit_disjunctions(clause, &mut |arms| {
            let searches = arms.iter().any(|&arm| {
                let mut found = false;
                visit_searches(arm, &mut |node| found |= is_search(node));
                found
            });
            mixed |= searches && !arms.iter().all(|&arm| only_searches(arm, &is_search));
        });
    }
    mixed
}

/// Reports whether `node` combines nothing but searches with `AND` and `OR`.
fn only_searches(node: *mut pg_sys::Node, is_search: &impl Fn(*mut pg_sys::Node) -> bool) -> bool {
    if node.is_null() || is_negation(node) {
        return false;
    }
    if unsafe { (*node).type_ } != pg_sys::NodeTag::T_BoolExpr {
        return is_search(node);
    }
    let expression = unsafe { &*node.cast::<pg_sys::BoolExpr>() };
    unsafe { PgList::<pg_sys::Node>::from_pg(expression.args) }
        .iter_ptr()
        .all(|arm| only_searches(arm, is_search))
}

#[pg_extern(immutable, parallel_unsafe)]
fn score_support(request: Internal) -> Internal {
    let unhandled = || Internal::from(Some(pg_sys::Datum::from(0_usize)));
    let Some(datum) = request.into_datum() else {
        return unhandled();
    };
    unsafe {
        let node = datum.cast_mut_ptr::<pg_sys::Node>();
        if node.is_null() || (*node).type_ != pg_sys::NodeTag::T_SupportRequestSimplify {
            return unhandled();
        }
        let request = &*node.cast::<pg_sys::SupportRequestSimplify>();
        if request.root.is_null() || request.fcall.is_null() {
            return unhandled();
        }
        let ctid = pg_sys::list_nth((*request.fcall).args, 0).cast::<pg_sys::Node>();
        if ctid.is_null() || (*ctid).type_ != pg_sys::NodeTag::T_Var {
            return unhandled();
        }
        let ctid = &*ctid.cast::<pg_sys::Var>();
        if ctid.varattno != pg_sys::SelfItemPointerAttributeNumber as i16 || ctid.varlevelsup != 0 {
            return unhandled();
        }
        let parse = (*request.root).parse;
        let mut binding = QualBinding {
            matches: Vec::new(),
        };
        // Pulled-up subqueries leave their quals in nested FromExpr nodes, so
        // walk the whole jointree rather than only its top-level quals.
        let jointree = (*parse).jointree.cast::<pg_sys::Node>();
        find_qual(jointree, (&mut binding as *mut QualBinding).cast());
        let rte = pg_sys::list_nth((*parse).rtable, (ctid.varno - 1) as i32)
            .cast::<pg_sys::RangeTblEntry>();
        if rte.is_null() || (*rte).rtekind != pg_sys::RTEKind::RTE_RELATION {
            return unhandled();
        }
        let searches = binding
            .matches
            .iter()
            .copied()
            .filter(|&(document, _)| single_varno(document) == Some(ctid.varno))
            .collect::<Vec<_>>();
        if searches.is_empty() {
            return unhandled();
        }
        let mut groups = bind_search_groups(parse, rte, ctid.varno, &searches);
        let mut documents = PgList::<pg_sys::Node>::new();
        for group in &groups {
            documents.push(pg_sys::copyObjectImpl(group.document.cast()).cast());
        }
        let documents = make_text_array(documents);
        let original_nargs = pg_sys::list_length((*request.fcall).args);
        let function_name = pg_sys::get_func_name((*request.fcall).funcid);
        let fname = CStr::from_ptr(function_name).to_string_lossy();
        let mut calls = ScoringCalls {
            ctid,
            documents,
            support: pg_sys::get_func_support((*request.fcall).funcid),
            bound: lookup_score_bound(),
            full: false,
            score: Vec::new(),
        };
        pg_sys::query_tree_walker(
            parse,
            Some(collect_scoring_calls),
            (&mut calls as *mut ScoringCalls).cast(),
            pg_sys::QTW_IGNORE_RC_SUBQUERIES as i32,
        );
        if calls.full && !calls.score.is_empty() {
            pgrx::error!(
                "tin.score() and tin.full_score() cannot be combined on one scanned relation; \
                 use one scoring function per relation (tin.max_score() adapts to either)"
            );
        }
        for (index, (_, name)) in SCAN_ARGUMENTS.iter().enumerate() {
            if calls
                .score
                .iter()
                .any(|call| !pg_sys::equal(call[index].cast(), calls.score[0][index].cast()))
            {
                pgrx::error!(
                    "tin.score() calls on one relation must use identical {name} arguments"
                );
            }
        }
        // tin.max_score adapts to the relation's tin.score calls, and
        // otherwise reports the full_score maximum.
        let mode = match fname.as_ref() {
            "full_score" => ScoreMode::FullScore,
            "max_score" if calls.score.is_empty() => ScoreMode::MaxFullScore,
            "max_score" => ScoreMode::MaxScore,
            _ => ScoreMode::Score,
        };
        if mode == ScoreMode::MaxScore
            && (groups.len() > 1 || has_mixed_disjunction(parse, ctid.varno))
        {
            return Internal::from(Some(pg_sys::Datum::from(
                make_null_const(pg_sys::FLOAT4OID) as usize,
            )));
        }
        if mode.is_max() {
            mark_required_groups(parse, rte, ctid.varno, &mut groups);
        }
        let mut args = PgList::<pg_sys::Node>::new();
        args.push(documents);
        let mut queries = PgList::<pg_sys::Node>::new();
        let mut query_groups = Vec::new();
        for (position, group) in groups.iter().enumerate() {
            for query in group_queries(&group.queries) {
                queries.push(query);
                query_groups.push(position as i32);
            }
        }
        args.push(make_text_array(queries));
        args.push(make_array_const(query_groups, pg_sys::INT4ARRAYOID));
        args.push(make_int4_const((*rte).relid.to_u32() as i32).cast());
        args.push(make_array_const(
            groups
                .iter()
                .map(|group| group.index_oid.to_u32() as i32)
                .collect(),
            pg_sys::INT4ARRAYOID,
        ));
        args.push(make_array_const(
            groups.iter().map(|group| group.required).collect(),
            pg_sys::BOOLARRAYOID,
        ));
        args.push(make_int4_const(mode as i32).cast());
        let null_float = || make_null_const(pg_sys::FLOAT4OID);
        let null_array = || make_null_const(pg_sys::TEXTARRAYOID);
        if mode == ScoreMode::Score {
            for position in 1..=5 {
                args.push(
                    pg_sys::copyObjectImpl(
                        pg_sys::list_nth((*request.fcall).args, position).cast(),
                    )
                    .cast(),
                );
            }
        } else if mode == ScoreMode::MaxScore {
            let [dense_ratio, term_add, term_replace] = calls.score[0];
            args.push(pg_sys::copyObjectImpl(dense_ratio.cast()).cast());
            args.push(null_float().cast());
            args.push(null_float().cast());
            args.push(pg_sys::copyObjectImpl(term_add.cast()).cast());
            args.push(pg_sys::copyObjectImpl(term_replace.cast()).cast());
        } else {
            args.push(null_float().cast());
            if mode == ScoreMode::FullScore && original_nargs == 3 {
                args.push(
                    pg_sys::copyObjectImpl(pg_sys::list_nth((*request.fcall).args, 1).cast())
                        .cast(),
                );
                args.push(
                    pg_sys::copyObjectImpl(pg_sys::list_nth((*request.fcall).args, 2).cast())
                        .cast(),
                );
            } else {
                args.push(null_float().cast());
                args.push(null_float().cast());
            }
            args.push(null_array().cast());
            args.push(null_array().cast());
        }
        let oid = lookup_score_bound();
        let replacement = pg_sys::makeFuncExpr(
            oid,
            pg_sys::FLOAT4OID,
            args.into_pg(),
            pg_sys::InvalidOid,
            pg_sys::InvalidOid,
            pg_sys::CoercionForm::COERCE_EXPLICIT_CALL,
        );
        Internal::from(Some(pg_sys::Datum::from(replacement as usize)))
    }
}

/// Returns copies of a group's search expressions, which the scorer combines.
/// Passing the expressions as an array keeps parameters and other
/// non-constant nodes, which cannot be combined at plan time, contributing to
/// the scores.
unsafe fn group_queries(queries: &[*mut pg_sys::Node]) -> Vec<*mut pg_sys::Node> {
    let copy = |node: *mut pg_sys::Node| unsafe { pg_sys::copyObjectImpl(node.cast()).cast() };
    let mut elements = queries
        .iter()
        .copied()
        .filter(|&node| !node.is_null() && unsafe { pg_sys::exprType(node) } == pg_sys::TEXTOID)
        .map(copy)
        .collect::<Vec<_>>();
    if elements.is_empty() {
        elements.push(copy(queries[0]));
    }
    elements
}

/// Builds an array constant of `values`.
unsafe fn make_array_const<T: IntoDatum>(
    values: Vec<T>,
    array_type: pg_sys::Oid,
) -> *mut pg_sys::Node {
    let datum = values
        .into_datum()
        .expect("an array of non-null values is not NULL");
    unsafe { pg_sys::makeConst(array_type, -1, pg_sys::InvalidOid, -1, datum, false, false).cast() }
}

/// Builds a `text[]` expression from `text` expressions.
pub(crate) unsafe fn make_text_array(elements: PgList<pg_sys::Node>) -> *mut pg_sys::Node {
    let mut array = unsafe { PgBox::<pg_sys::ArrayExpr>::alloc_node(pg_sys::NodeTag::T_ArrayExpr) };
    array.array_typeid = pg_sys::TEXTARRAYOID;
    array.array_collid = pg_sys::DEFAULT_COLLATION_OID;
    array.element_typeid = pg_sys::TEXTOID;
    array.elements = elements.into_pg();
    array.multidims = false;
    array.location = -1;
    array.into_pg().cast()
}

unsafe fn make_int4_const(value: i32) -> *mut pg_sys::Const {
    unsafe {
        pg_sys::makeConst(
            pg_sys::INT4OID,
            -1,
            pg_sys::InvalidOid,
            4,
            pg_sys::Datum::from(value as usize),
            false,
            true,
        )
    }
}

unsafe fn make_null_const(type_oid: pg_sys::Oid) -> *mut pg_sys::Const {
    unsafe {
        pg_sys::makeConst(
            type_oid,
            -1,
            pg_sys::InvalidOid,
            -1,
            pg_sys::Datum::null(),
            true,
            false,
        )
    }
}

unsafe fn lookup_score_bound() -> pg_sys::Oid {
    let types = [
        pg_sys::TEXTARRAYOID,
        pg_sys::TEXTARRAYOID,
        pg_sys::INT4ARRAYOID,
        pg_sys::INT4OID,
        pg_sys::INT4ARRAYOID,
        pg_sys::BOOLARRAYOID,
        pg_sys::INT4OID,
        pg_sys::FLOAT4OID,
        pg_sys::FLOAT4OID,
        pg_sys::FLOAT4OID,
        pg_sys::TEXTARRAYOID,
        pg_sys::TEXTARRAYOID,
    ];
    unsafe { lookup_bound_function(c"tin.score_bound", &types) }
}

/// Looks up a function that a support function rewrites calls into.
pub(crate) unsafe fn lookup_bound_function(name: &CStr, types: &[pg_sys::Oid]) -> pg_sys::Oid {
    let names = unsafe { pg_sys::stringToQualifiedNameList(name.as_ptr(), std::ptr::null_mut()) };
    let oid = unsafe { pg_sys::LookupFuncName(names, types.len() as i32, types.as_ptr(), true) };
    if oid == pg_sys::InvalidOid {
        pgrx::ereport!(
            ERROR,
            pg_sys::errcodes::PgSqlErrorCode::ERRCODE_UNDEFINED_FUNCTION,
            format!(
                "{}() is missing: the installed tin SQL predates this build",
                name.to_string_lossy()
            ),
            "Lead ships no extension upgrade scripts, so the extension has to be \
             reinstalled: DROP EXTENSION tin CASCADE; CREATE EXTENSION tin; \
             CASCADE also drops every tin index and anything else that depends on \
             them, so recreate those afterwards."
        );
    }
    oid
}

pgrx::extension_sql!(
    r#"
ALTER FUNCTION @extschema@.full_score(pg_catalog.tid) SUPPORT @extschema@.score_support;
ALTER FUNCTION @extschema@.full_score(pg_catalog.tid, pg_catalog.float4, pg_catalog.float4) SUPPORT @extschema@.score_support;
ALTER FUNCTION @extschema@.score(pg_catalog.tid, pg_catalog.float4, pg_catalog.float4, pg_catalog.float4, pg_catalog.text[], pg_catalog.text[]) SUPPORT @extschema@.score_support;
ALTER FUNCTION @extschema@.max_score(pg_catalog.tid) SUPPORT @extschema@.score_support;
REVOKE ALL ON FUNCTION @extschema@.score_bound(pg_catalog.text[], pg_catalog.text[], pg_catalog.int4[], pg_catalog.int4, pg_catalog.int4[], pg_catalog.bool[], pg_catalog.int4, pg_catalog.float4, pg_catalog.float4, pg_catalog.float4, pg_catalog.text[], pg_catalog.text[]) FROM PUBLIC;
"#,
    name = "score_support_bindings",
    requires = [
        full_score,
        full_score_with_bm25,
        score,
        max_score,
        score_bound,
        score_support
    ]
);
