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
use pgrx::iter::TableIterator;
use pgrx::{
    FromDatum, Internal, IntoDatum, PgList, PgRelation, Spi, default, name, pg_extern, pg_guard,
    pg_sys,
};
use rustc_hash::FxHashMap;
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_void};
use tinql::runtime::{Query, SpanTermSlot, evaluate, parse_tinql_to_query, tokenize_doc};
use tokenizer::{CompiledTokenizerPipeline, Tokenizer};

#[derive(Clone, Debug, Eq, PartialEq)]
struct CacheKey {
    transaction: u32,
    command: u32,
    heap_oid: u32,
    index_oid: u32,
    query: String,
    full: bool,
    dense: u32,
    k1: Option<u32>,
    b: Option<u32>,
    add: Option<Vec<String>>,
    replace: Option<Vec<String>>,
}

struct ScoreCorpus {
    key: CacheKey,
    by_document: FxHashMap<String, f32>,
    max: f32,
}

thread_local! {
    static SCORE_CACHE: RefCell<Option<ScoreCorpus>> = const { RefCell::new(None) };
}

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
    document: &str,
    query: &str,
    heap_oid: i32,
    index_oid: i32,
    mode: i32,
    dense_ratio: Option<f32>,
    k1: Option<f32>,
    b: Option<f32>,
    term_add: Option<Vec<String>>,
    term_replace: Option<Vec<String>>,
) -> f32 {
    let key = CacheKey {
        transaction: unsafe { pg_sys::GetTopTransactionIdIfAny().into_inner() },
        command: unsafe { pg_sys::GetCurrentCommandId(false) },
        heap_oid: heap_oid as u32,
        index_oid: index_oid as u32,
        query: query.to_owned(),
        full: mode == 1 || mode == 3,
        dense: dense_ratio.unwrap_or(DenseRatio::DEFAULT).to_bits(),
        k1: bits(k1),
        b: bits(b),
        add: term_add.clone(),
        replace: term_replace.clone(),
    };
    SCORE_CACHE.with_borrow_mut(|slot| {
        if slot.as_ref().is_none_or(|corpus| corpus.key != key) {
            *slot = Some(build_corpus(key.clone(), k1, b, term_add, term_replace));
        }
        let corpus = slot.as_ref().expect("score corpus was just populated");
        if mode == 2 || mode == 3 {
            corpus.max
        } else {
            corpus.by_document.get(document).copied().unwrap_or(0.0)
        }
    })
}

fn build_corpus(
    key: CacheKey,
    k1: Option<f32>,
    b: Option<f32>,
    term_add: Option<Vec<String>>,
    term_replace: Option<Vec<String>>,
) -> ScoreCorpus {
    let heap_oid = pg_sys::Oid::from(key.heap_oid);
    let index = unsafe {
        PgRelation::with_lock(
            pg_sys::Oid::from(key.index_oid),
            pg_sys::AccessShareLock as _,
        )
    };
    if unsafe { pg_sys::IndexGetRelation(index.oid(), false) } != heap_oid {
        pgrx::error!("tin score index no longer belongs to the scored relation");
    }
    let tokenizer = unsafe { crate::options::tokenizer(index.as_ptr()) };
    let defaults = unsafe { crate::options::bm25(index.as_ptr()) };
    let stop_csv = unsafe { crate::options::score_stop_words(index.as_ptr()) };
    let params = Bm25Overrides { k1, b }
        .resolve(defaults)
        .checked()
        .unwrap_or_else(|error| pgrx::error!("tin score parameters: {error}"));
    let dense = DenseRatio::new(Some(f32::from_bits(key.dense)));
    if !key.full && !dense.is_valid() {
        pgrx::error!("dense_ratio must be finite and non-negative");
    }
    let query = parse_tinql_to_query(&key.query, &tokenizer)
        .unwrap_or_else(|error| pgrx::error!("TIN score query error: {error}"));
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
    let stop = if key.full {
        None
    } else {
        stop_csv.as_deref().and_then(ScoreStopWords::from_csv)
    };
    let terms = compile_scoring_terms(inputs, &edit, stop.as_ref());
    let documents = load_documents(heap_oid, index.oid());
    let tokenized = tokenize_documents(&documents, &tokenizer);
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
        let ratio = (!key.full).then_some(dense);
        if !term.is_retained(df, df, total_docs, ratio) {
            continue;
        }
        let scorer =
            TermScorer::from_statistics(total_docs, df, term.boost(), params, average_length)
                .unwrap_or_else(|error| pgrx::error!("tin score parameters: {error}"));
        scorers.push((term.text().to_owned(), scorer));
    }
    let mut by_document = FxHashMap::default();
    let mut max = 0.0_f32;
    for (document, tokens) in documents.into_iter().zip(tokenized) {
        let score = sum_scores_in_order(scorers.iter().map(|(term, scorer)| {
            let tf = tokens.iter().filter(|token| *token == term).count() as u32;
            if tf == 0 {
                0.0
            } else {
                scorer.score_count(tf, tokens.len() as u32)
            }
        }));
        let matched = evaluate(&query, &tokenize_doc(&document, &tokenizer))
            .unwrap_or_else(|error| pgrx::error!("tin score query evaluation failed: {error}"))
            .matched;
        if matched {
            max = max.max(score);
        }
        by_document.insert(document, score);
    }
    ScoreCorpus {
        key,
        by_document,
        max,
    }
}

fn load_documents(heap_oid: pg_sys::Oid, index_oid: pg_sys::Oid) -> Vec<String> {
    unsafe {
        let relname = pg_sys::get_rel_name(heap_oid);
        let namespace = pg_sys::get_namespace_name(pg_sys::get_rel_namespace(heap_oid));
        if relname.is_null() || namespace.is_null() {
            pgrx::error!("tin score relation no longer exists");
        }
        let qualified = pg_sys::quote_qualified_identifier(namespace, relname);
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
        let (expression, predicate) = Spi::get_two::<String, String>(&index_sql)
            .unwrap_or_else(|error| pgrx::error!("tin score index lookup failed: {error}"));
        let expression = expression
            .unwrap_or_else(|| pgrx::error!("tin score index expression no longer exists"));
        let predicate = predicate
            .map(|predicate| format!(" AND ({predicate})"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT ({expression})::text FROM {} WHERE ({expression}) IS NOT NULL{predicate}",
            CStr::from_ptr(qualified).to_string_lossy(),
        );
        Spi::connect(|client| {
            client
                .select(&sql, None, &[])
                .unwrap_or_else(|error| pgrx::error!("tin score corpus scan failed: {error}"))
                .map(|row| {
                    row.get::<String>(1)
                        .unwrap_or_else(|error| {
                            pgrx::error!("tin score corpus row failed: {error}")
                        })
                        .expect("corpus query excludes null documents")
                })
                .collect()
        })
    }
}

fn tokenize_documents(
    documents: &[String],
    tokenizer: &CompiledTokenizerPipeline,
) -> Vec<Vec<String>> {
    documents
        .iter()
        .enumerate()
        .map(|(row, document)| {
            if row.is_multiple_of(10) {
                pgrx::check_for_interrupts!();
            }
            tokenizer
                .tokenize(document)
                .map(|token| token.text.into_owned())
                .collect()
        })
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
    let docs = load_documents(heap_oid, index.oid());
    let tokenized = tokenize_documents(&docs, &tokenizer);
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

#[pg_guard]
unsafe extern "C-unwind" fn find_qual(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
    if node.is_null() {
        return false;
    }
    let binding = unsafe { &mut *context.cast::<QualBinding>() };
    if unsafe { (*node).type_ } == pg_sys::NodeTag::T_OpExpr {
        let op = node.cast::<pg_sys::OpExpr>();
        let name = unsafe { pg_sys::get_opname((*op).opno) };
        if !name.is_null()
            && unsafe { CStr::from_ptr(name) }.to_bytes() == b"==>"
            && unsafe { pg_sys::list_length((*op).args) } == 2
        {
            let left = unsafe { pg_sys::list_nth((*op).args, 0).cast::<pg_sys::Node>() };
            let right = unsafe { pg_sys::list_nth((*op).args, 1).cast::<pg_sys::Node>() };
            if !left.is_null() {
                binding.matches.push((left, right));
            }
        }
    }
    unsafe { pg_sys::expression_tree_walker(node, Some(find_qual), context) }
}

pub(crate) unsafe fn find_matching_tin_index(
    heap_oid: pg_sys::Oid,
    query_varno: i32,
    operand: *mut pg_sys::Node,
) -> Option<pg_sys::Oid> {
    let tin_name = CString::new("tin").expect("static access method name is valid");
    let tin_am = unsafe { pg_sys::get_index_am_oid(tin_name.as_ptr(), false) };
    let normalized = unsafe { pg_sys::copyObjectImpl(operand.cast()).cast::<pg_sys::Node>() };
    unsafe { pg_sys::ChangeVarNodes(normalized, query_varno, 1, 0) };
    let normalized = unsafe { pg_sys::strip_implicit_coercions(normalized) };
    let heap = unsafe { pg_sys::table_open(heap_oid, pg_sys::AccessShareLock as _) };
    let indexes = unsafe { PgList::<pg_sys::Oid>::from_pg(pg_sys::RelationGetIndexList(heap)) };
    let mut matched = None;
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
        unsafe { pg_sys::index_close(index, pg_sys::AccessShareLock as _) };
        if matches {
            matched = Some(index_oid);
            break;
        }
    }
    unsafe { pg_sys::table_close(heap, pg_sys::AccessShareLock as _) };
    matched
}

struct FullScoreBinding {
    ctid: *const pg_sys::Var,
    document: *mut pg_sys::Node,
    support: pg_sys::Oid,
    bound: pg_sys::Oid,
}

#[pg_guard]
unsafe extern "C-unwind" fn has_full_score(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
    unsafe {
        if node.is_null() || (*node).type_ == pg_sys::NodeTag::T_Query {
            return false;
        }
        let binding = &*context.cast::<FullScoreBinding>();
        if (*node).type_ == pg_sys::NodeTag::T_FuncExpr {
            let function = &*node.cast::<pg_sys::FuncExpr>();
            // Earlier query clauses may already contain the rewritten scorer.
            if function.funcid == binding.bound {
                let mode = pg_sys::list_nth(function.args, 4).cast::<pg_sys::Const>();
                if (*mode).xpr.type_ == pg_sys::NodeTag::T_Const
                    && (*mode).constvalue.value() == 1
                    && pg_sys::equal(pg_sys::list_nth(function.args, 0), binding.document.cast())
                {
                    return true;
                }
            } else if pg_sys::get_func_support(function.funcid) == binding.support
                && CStr::from_ptr(pg_sys::get_func_name(function.funcid)).to_bytes()
                    == b"full_score"
            {
                for position in 0..pg_sys::list_length(function.args) {
                    let mut argument =
                        pg_sys::list_nth(function.args, position).cast::<pg_sys::Node>();
                    if (*argument).type_ == pg_sys::NodeTag::T_NamedArgExpr {
                        let named = &*argument.cast::<pg_sys::NamedArgExpr>();
                        if named.argnumber != 0 {
                            continue;
                        }
                        argument = named.arg.cast();
                    } else if position != 0 {
                        continue;
                    }
                    if pg_sys::equal(argument.cast(), binding.ctid.cast()) {
                        return true;
                    }
                }
            }
        }
        pg_sys::expression_tree_walker(node, Some(has_full_score), context)
    }
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
        let quals = (*(*parse).jointree).quals.cast::<pg_sys::Node>();
        find_qual(quals, (&mut binding as *mut QualBinding).cast());
        let rte = pg_sys::list_nth((*parse).rtable, (ctid.varno - 1) as i32)
            .cast::<pg_sys::RangeTblEntry>();
        if rte.is_null() || (*rte).rtekind != pg_sys::RTEKind::RTE_RELATION {
            return unhandled();
        }
        let Some((document, first_query, index_oid)) =
            binding.matches.iter().find_map(|&(document, query)| {
                find_matching_tin_index((*rte).relid, ctid.varno, document)
                    .map(|index_oid| (document, query, index_oid))
            })
        else {
            return unhandled();
        };
        let original_nargs = pg_sys::list_length((*request.fcall).args);
        let function_name = pg_sys::get_func_name((*request.fcall).funcid);
        let fname = CStr::from_ptr(function_name).to_string_lossy();
        let mode = if fname.as_ref() == "full_score" {
            1
        } else if fname.as_ref() == "max_score" {
            let mut binding = FullScoreBinding {
                ctid,
                document,
                support: pg_sys::get_func_support((*request.fcall).funcid),
                bound: lookup_score_bound(),
            };
            let full = pg_sys::query_tree_walker(
                parse,
                Some(has_full_score),
                (&mut binding as *mut FullScoreBinding).cast(),
                pg_sys::QTW_IGNORE_RC_SUBQUERIES as i32,
            );
            if full { 3 } else { 2 }
        } else {
            0
        };
        let mut args = PgList::<pg_sys::Node>::new();
        args.push(pg_sys::copyObjectImpl(document.cast()).cast());
        let same_expression = binding
            .matches
            .iter()
            .copied()
            .filter(|(candidate, _)| pg_sys::equal((*candidate).cast(), document.cast()))
            .collect::<Vec<_>>();
        let combined_query = combine_constant_queries(&same_expression)
            .unwrap_or_else(|| pg_sys::copyObjectImpl(first_query.cast()).cast());
        args.push(combined_query);
        args.push(make_int4_const((*rte).relid.to_u32() as i32).cast());
        args.push(make_int4_const(index_oid.to_u32() as i32).cast());
        args.push(make_int4_const(mode).cast());
        let null_float = || make_null_const(pg_sys::FLOAT4OID);
        let null_array = || make_null_const(pg_sys::TEXTARRAYOID);
        if mode == 0 {
            for position in 1..=5 {
                args.push(
                    pg_sys::copyObjectImpl(
                        pg_sys::list_nth((*request.fcall).args, position).cast(),
                    )
                    .cast(),
                );
            }
        } else {
            args.push(null_float().cast());
            if mode == 1 && original_nargs == 3 {
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

unsafe fn combine_constant_queries(
    matches: &[(*mut pg_sys::Node, *mut pg_sys::Node)],
) -> Option<*mut pg_sys::Node> {
    if matches.len() < 2 {
        return None;
    }
    let mut queries = Vec::with_capacity(matches.len());
    for &(_, node) in matches {
        if node.is_null() || unsafe { (*node).type_ } != pg_sys::NodeTag::T_Const {
            return None;
        }
        let value = unsafe { &*node.cast::<pg_sys::Const>() };
        if value.constisnull || value.consttype != pg_sys::TEXTOID {
            return None;
        }
        queries.push(unsafe { String::from_datum(value.constvalue, false)? });
    }
    let combined = queries
        .into_iter()
        .map(|query| format!("({query})"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let datum = combined.into_datum()?;
    Some(unsafe {
        pg_sys::makeConst(
            pg_sys::TEXTOID,
            -1,
            pg_sys::DEFAULT_COLLATION_OID,
            -1,
            datum,
            false,
            false,
        )
        .cast()
    })
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
    let name = CString::new("tin.score_bound").unwrap();
    let names = unsafe { pg_sys::stringToQualifiedNameList(name.as_ptr(), std::ptr::null_mut()) };
    let types = [
        pg_sys::TEXTOID,
        pg_sys::TEXTOID,
        pg_sys::INT4OID,
        pg_sys::INT4OID,
        pg_sys::INT4OID,
        pg_sys::FLOAT4OID,
        pg_sys::FLOAT4OID,
        pg_sys::FLOAT4OID,
        pg_sys::TEXTARRAYOID,
        pg_sys::TEXTARRAYOID,
    ];
    unsafe { pg_sys::LookupFuncName(names, types.len() as i32, types.as_ptr(), false) }
}

pgrx::extension_sql!(
    r#"
ALTER FUNCTION @extschema@.full_score(pg_catalog.tid) SUPPORT @extschema@.score_support;
ALTER FUNCTION @extschema@.full_score(pg_catalog.tid, pg_catalog.float4, pg_catalog.float4) SUPPORT @extschema@.score_support;
ALTER FUNCTION @extschema@.score(pg_catalog.tid, pg_catalog.float4, pg_catalog.float4, pg_catalog.float4, pg_catalog.text[], pg_catalog.text[]) SUPPORT @extschema@.score_support;
ALTER FUNCTION @extschema@.max_score(pg_catalog.tid) SUPPORT @extschema@.score_support;
REVOKE ALL ON FUNCTION @extschema@.score_bound(pg_catalog.text, pg_catalog.text, pg_catalog.int4, pg_catalog.int4, pg_catalog.int4, pg_catalog.float4, pg_catalog.float4, pg_catalog.float4, pg_catalog.text[], pg_catalog.text[]) FROM PUBLIC;
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
