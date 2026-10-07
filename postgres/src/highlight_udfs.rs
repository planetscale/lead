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
use crate::analysis::{Resolution, encode, resolve, sibling_function, text_const, unbound_query};
use crate::highlight::{highlight_text, highlight_text_ansi, query_positions, rewrap_text};
use crate::tinql::parse_tinql_to_query;
use crate::udfs::TokenizeOptions;
use pgrx::{FromDatum, Internal, IntoDatum, PgList, default, pg_extern, pg_guard, pg_sys};
use std::borrow::Cow;
use std::ffi::{CStr, c_void};
use tinql::runtime::Query;
use tokenizer::CompiledTokenizerPipeline;

const ANALYSIS_ARGS: usize = 8;

#[derive(Clone, Copy)]
struct AnalysisArgs<'a> {
    tokenizer: Option<&'a str>,
    case_folding: Option<&'a str>,
    accent_folding: Option<&'a str>,
    long_tokens: Option<&'a str>,
    max_token_bytes: Option<i32>,
    graphemes: Option<&'a str>,
    position_gaps: Option<&'a str>,
    stemmer: Option<&'a str>,
}

impl AnalysisArgs<'_> {
    /// Unset arguments mean the default analysis, or the searched index's
    /// analysis once the planner has bound the call to one.
    fn pipeline(self) -> CompiledTokenizerPipeline {
        TokenizeOptions {
            tokenizer: self.tokenizer.unwrap_or("unicode"),
            case_folding: self.case_folding.unwrap_or("fold"),
            accent_folding: self.accent_folding.unwrap_or("fold"),
            long_tokens: self.long_tokens.unwrap_or("split"),
            max_token_bytes: self.max_token_bytes.unwrap_or(256),
            graphemes: self.graphemes.unwrap_or("emoji"),
            position_gaps: self.position_gaps.unwrap_or("preserve"),
            stemmer: self.stemmer,
        }
        .into_spec()
        .and_then(|spec| spec.compile().map_err(|error| error.to_string()))
        .unwrap_or_else(|error| pgrx::error!("{error}"))
    }
}

fn render_highlight(
    pipeline: &CompiledTokenizerPipeline,
    text: &str,
    begin_tag: &str,
    end_tag: &str,
    query: Option<&Query>,
) -> String {
    // Without an explicit query or a tin index to bind one from, tin leaves
    // the text unmarked.
    let positions = query
        .map(|query| query_positions(pipeline, query, text))
        .unwrap_or_default();
    highlight_text(pipeline, text, begin_tag, end_tag, &positions)
        .unwrap_or_else(|error| pgrx::error!("{error}"))
}

fn render_highlight_ansi(
    pipeline: &CompiledTokenizerPipeline,
    text: &str,
    wrap_to: Option<i32>,
    query: Option<&Query>,
) -> String {
    let text = match wrap_to {
        Some(width) if width <= 0 => pgrx::error!("wrap_to must be positive"),
        Some(width) => Cow::Owned(rewrap_text(text, width as usize)),
        None => Cow::Borrowed(text),
    };
    let positions = query
        .map(|query| query_positions(pipeline, query, text.as_ref()))
        .unwrap_or_default();
    if positions.is_empty() {
        return text.into_owned();
    }
    highlight_text_ansi(pipeline, text.as_ref(), &positions)
        .unwrap_or_else(|error| pgrx::error!("{error}"))
}

/// Parses an explicit highlight query. A query that does not parse marks
/// nothing.
fn explicit_query(pipeline: &CompiledTokenizerPipeline, query: Option<&str>) -> Option<Query> {
    parse_tinql_to_query(query?, pipeline).ok()
}

/// Combines the search texts that `highlight_support` bound. A NULL text
/// matches no rows, so it marks nothing.
fn bound_query(
    pipeline: &CompiledTokenizerPipeline,
    queries: Vec<Option<String>>,
) -> Option<Query> {
    let texts = queries
        .into_iter()
        .map(|text| text.map(|text| crate::analysis::raw_query(&text).to_owned()))
        .collect::<Option<Vec<_>>>()?;
    Some(crate::operator::parse_searches(&texts, pipeline))
}

#[pg_extern(name = "highlight_v1_0_3", immutable, parallel_safe)]
fn highlight_v1_0_3(
    text: Option<&str>,
    begin_tag: default!(&str, "'<b>'"),
    end_tag: default!(&str, "'</b>'"),
    query: default!(Option<&str>, "NULL"),
) -> Option<String> {
    let pipeline = tokenizer::presets::default_pipeline();
    Some(render_highlight(
        pipeline,
        text?,
        begin_tag,
        end_tag,
        explicit_query(pipeline, query).as_ref(),
    ))
}

#[pg_extern(name = "highlight_ansi_v1_0_3", immutable, parallel_safe)]
fn highlight_ansi_v1_0_3(
    text: Option<&str>,
    wrap_to: default!(Option<i32>, "NULL"),
    query: default!(Option<&str>, "NULL"),
) -> Option<String> {
    let pipeline = tokenizer::presets::default_pipeline();
    Some(render_highlight_ansi(
        pipeline,
        text?,
        wrap_to,
        explicit_query(pipeline, query).as_ref(),
    ))
}

#[pg_extern(name = "highlight", immutable, parallel_safe)]
#[expect(clippy::too_many_arguments, reason = "TIN-compatible SQL signature")]
fn highlight(
    text: Option<&str>,
    begin_tag: default!(&str, "'<b>'"),
    end_tag: default!(&str, "'</b>'"),
    query: default!(Option<&str>, "NULL"),
    tokenizer: default!(Option<&str>, "NULL"),
    case_folding: default!(Option<&str>, "NULL"),
    accent_folding: default!(Option<&str>, "NULL"),
    long_tokens: default!(Option<&str>, "NULL"),
    max_token_bytes: default!(Option<i32>, "NULL"),
    graphemes: default!(Option<&str>, "NULL"),
    position_gaps: default!(Option<&str>, "NULL"),
    stemmer: default!(Option<&str>, "NULL"),
) -> Option<String> {
    let pipeline = AnalysisArgs {
        tokenizer,
        case_folding,
        accent_folding,
        long_tokens,
        max_token_bytes,
        graphemes,
        position_gaps,
        stemmer,
    }
    .pipeline();
    Some(render_highlight(
        &pipeline,
        text?,
        begin_tag,
        end_tag,
        explicit_query(&pipeline, query).as_ref(),
    ))
}

#[pg_extern(name = "highlight_ansi", immutable, parallel_safe)]
#[expect(clippy::too_many_arguments, reason = "TIN-compatible SQL signature")]
fn highlight_ansi(
    text: Option<&str>,
    wrap_to: default!(Option<i32>, "NULL"),
    query: default!(Option<&str>, "NULL"),
    tokenizer: default!(Option<&str>, "NULL"),
    case_folding: default!(Option<&str>, "NULL"),
    accent_folding: default!(Option<&str>, "NULL"),
    long_tokens: default!(Option<&str>, "NULL"),
    max_token_bytes: default!(Option<i32>, "NULL"),
    graphemes: default!(Option<&str>, "NULL"),
    position_gaps: default!(Option<&str>, "NULL"),
    stemmer: default!(Option<&str>, "NULL"),
) -> Option<String> {
    let pipeline = AnalysisArgs {
        tokenizer,
        case_folding,
        accent_folding,
        long_tokens,
        max_token_bytes,
        graphemes,
        position_gaps,
        stemmer,
    }
    .pipeline();
    Some(render_highlight_ansi(
        &pipeline,
        text?,
        wrap_to,
        explicit_query(&pipeline, query).as_ref(),
    ))
}

/// `highlight` with the search texts of the quals `highlight_support` bound.
#[pg_extern(immutable, parallel_safe)]
#[expect(
    clippy::too_many_arguments,
    reason = "SQL signature used by the support function"
)]
fn highlight_bound(
    text: Option<&str>,
    begin_tag: &str,
    end_tag: &str,
    queries: Vec<Option<String>>,
    tokenizer: Option<&str>,
    case_folding: Option<&str>,
    accent_folding: Option<&str>,
    long_tokens: Option<&str>,
    max_token_bytes: Option<i32>,
    graphemes: Option<&str>,
    position_gaps: Option<&str>,
    stemmer: Option<&str>,
) -> Option<String> {
    let pipeline = AnalysisArgs {
        tokenizer,
        case_folding,
        accent_folding,
        long_tokens,
        max_token_bytes,
        graphemes,
        position_gaps,
        stemmer,
    }
    .pipeline();
    Some(render_highlight(
        &pipeline,
        text?,
        begin_tag,
        end_tag,
        bound_query(&pipeline, queries).as_ref(),
    ))
}

/// `highlight_ansi` with the search texts of the quals `highlight_support`
/// bound.
#[pg_extern(immutable, parallel_safe)]
#[expect(
    clippy::too_many_arguments,
    reason = "SQL signature used by the support function"
)]
fn highlight_ansi_bound(
    text: Option<&str>,
    wrap_to: Option<i32>,
    queries: Vec<Option<String>>,
    tokenizer: Option<&str>,
    case_folding: Option<&str>,
    accent_folding: Option<&str>,
    long_tokens: Option<&str>,
    max_token_bytes: Option<i32>,
    graphemes: Option<&str>,
    position_gaps: Option<&str>,
    stemmer: Option<&str>,
) -> Option<String> {
    let pipeline = AnalysisArgs {
        tokenizer,
        case_folding,
        accent_folding,
        long_tokens,
        max_token_bytes,
        graphemes,
        position_gaps,
        stemmer,
    }
    .pipeline();
    Some(render_highlight_ansi(
        &pipeline,
        text?,
        wrap_to,
        bound_query(&pipeline, queries).as_ref(),
    ))
}

struct QueryContext {
    document: *mut pg_sys::Node,
    queries: Vec<*mut pg_sys::Node>,
    /// Whether a `==> ANY(...)` qual searches the document. Like tin, only
    /// `==>` quals supply an implicit query, but any search on the document
    /// binds the analysis of the index that covers it.
    array_search: bool,
}

#[pg_guard]
unsafe extern "C-unwind" fn collect_queries(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
    if node.is_null()
        || unsafe { (*node).type_ } == pg_sys::NodeTag::T_Query
        || crate::score::is_negation(node)
    {
        return false;
    }
    let context = unsafe { &mut *context.cast::<QueryContext>() };
    if let Some((document, query)) = unsafe { crate::operator::search_operands(node) }
        && unsafe { crate::score::same_operand(document, context.document) }
    {
        context.queries.push(unsafe { unbound_query(query) });
    }
    if unsafe { (*node).type_ } == pg_sys::NodeTag::T_ScalarArrayOpExpr {
        let saop = node.cast::<pg_sys::ScalarArrayOpExpr>();
        if unsafe { (*saop).opno } == crate::operator::search_operator()
            && unsafe { pg_sys::list_length((*saop).args) } == 2
            && unsafe {
                crate::score::same_operand(
                    pg_sys::list_nth((*saop).args, 0).cast(),
                    context.document,
                )
            }
        {
            context.array_search = true;
        }
    }
    unsafe {
        pg_sys::expression_tree_walker(
            node,
            Some(collect_queries),
            (context as *mut QueryContext).cast(),
        )
    }
}

fn unhandled() -> Internal {
    Internal::from(Some(pg_sys::Datum::from(0_usize)))
}

struct TargetSearch {
    document: *mut pg_sys::Node,
    found: bool,
}

#[pg_guard]
unsafe extern "C-unwind" fn find_highlight_call(
    node: *mut pg_sys::Node,
    context: *mut c_void,
) -> bool {
    if node.is_null() || unsafe { (*node).type_ } == pg_sys::NodeTag::T_Query {
        return false;
    }
    let search = unsafe { &mut *context.cast::<TargetSearch>() };
    if unsafe { (*node).type_ } == pg_sys::NodeTag::T_FuncExpr {
        let call = node.cast::<pg_sys::FuncExpr>();
        let name = unsafe { pg_sys::get_func_name((*call).funcid) };
        let first = unsafe { pg_sys::list_nth((*call).args, 0).cast::<pg_sys::Node>() };
        if !name.is_null()
            && unsafe { CStr::from_ptr(name) }
                .to_bytes()
                .starts_with(b"highlight")
            && unsafe { crate::score::same_operand(first, search.document) }
        {
            search.found = true;
            return true;
        }
    }
    unsafe { pg_sys::expression_tree_walker(node, Some(find_highlight_call), context) }
}

/// A query that reads other relations can only be injected where the scan's
/// output is formed. Join and ordering expressions are planned before then and
/// have to receive the query explicitly.
unsafe fn query_needs_other_relations(
    root: *mut pg_sys::PlannerInfo,
    call: *mut pg_sys::FuncExpr,
    document: *mut pg_sys::Node,
    queries: &[*mut pg_sys::Node],
) -> bool {
    unsafe {
        let own = pg_sys::pull_varnos(root, call.cast());
        let needed = queries
            .iter()
            .any(|&query| !pg_sys::bms_is_subset(pg_sys::pull_varnos(root, query), own));
        if !needed {
            return false;
        }
        let mut search = TargetSearch {
            document,
            found: false,
        };
        let targets = (*(*root).parse).targetList.cast::<pg_sys::Node>();
        find_highlight_call(targets, (&raw mut search).cast());
        !search.found
    }
}

const CONFLICT: &str = "tin.highlight() analysis conflicts with the searched index; matching \
     columns, including UNION ALL and partition children, must use the same analysis options";

/// Whether a supplied analysis argument leaves the index's analysis in force.
/// NULL inherits it; any other value has to be the same constant.
unsafe fn agrees(supplied: *mut pg_sys::Node, configured: *mut pg_sys::Node) -> bool {
    unsafe {
        if supplied.is_null() || (*supplied).type_ != pg_sys::NodeTag::T_Const {
            return false;
        }
        let supplied = &*supplied.cast::<pg_sys::Const>();
        let configured = &*configured.cast::<pg_sys::Const>();
        if supplied.constisnull {
            return true;
        }
        if configured.constisnull || supplied.consttype != configured.consttype {
            return false;
        }
        if supplied.consttype == pg_sys::INT4OID {
            i32::from_datum(supplied.constvalue, false)
                == i32::from_datum(configured.constvalue, false)
        } else {
            <&str>::from_datum(supplied.constvalue, false)
                == <&str>::from_datum(configured.constvalue, false)
        }
    }
}

unsafe fn configuration_args(spec: &tokenizer::TokenizerPipelineSpec) -> Vec<*mut pg_sys::Node> {
    let encoded = encode(spec);
    let fields: Vec<&str> = encoded.split(',').collect();
    assert_eq!(
        fields.len(),
        ANALYSIS_ARGS,
        "encoded analysis has eight fields"
    );
    fields
        .into_iter()
        .enumerate()
        .map(|(position, field)| unsafe {
            match position {
                4 => crate::score::make_int4_const(
                    field.parse().expect("encoded token limit is an integer"),
                )
                .cast(),
                7 if field.is_empty() => crate::score::make_null_const(pg_sys::TEXTOID).cast(),
                _ => text_const(field).cast(),
            }
        })
        .collect()
}

#[pg_extern(immutable, parallel_unsafe)]
fn highlight_support(request: Internal) -> Internal {
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
        let function_name = pg_sys::get_func_name((*request.fcall).funcid);
        if function_name.is_null() {
            return unhandled();
        }
        let name = CStr::from_ptr(function_name).to_bytes();
        let (query_position, legacy) = match name {
            b"highlight" => (3, false),
            b"highlight_v1_0_3" => (3, true),
            b"highlight_ansi" => (2, false),
            b"highlight_ansi_v1_0_3" => (2, true),
            _ => return unhandled(),
        };
        let args = PgList::<pg_sys::Node>::from_pg((*request.fcall).args);
        let expected = query_position + 1 + if legacy { 0 } else { ANALYSIS_ARGS };
        if args.len() != expected {
            return unhandled();
        }
        let document = args.get_ptr(0).expect("highlight has a text argument");
        if crate::score::single_varno(document).is_none() {
            return unhandled();
        }
        let (spec, indexes) = match resolve(request.root, document) {
            Resolution::Unindexed => return unhandled(),
            Resolution::Conflict => pgrx::error!("{CONFLICT}"),
            Resolution::Bound { spec, indexes } => (spec, indexes),
        };
        let mut binding = QueryContext {
            document,
            queries: Vec::new(),
            array_search: false,
        };
        // Pulled-up subqueries leave their quals in nested FromExpr nodes.
        collect_queries(
            (*(*request.root).parse).jointree.cast::<pg_sys::Node>(),
            (&mut binding as *mut QueryContext).cast(),
        );
        if binding.queries.is_empty() && !binding.array_search {
            return unhandled();
        }
        crate::analysis::depend_on_indexes(request.root, &indexes);
        let supplied_query = args
            .get_ptr(query_position)
            .expect("highlight has a query argument");
        let implicit = (*supplied_query).type_ == pg_sys::NodeTag::T_Const
            && (*supplied_query.cast::<pg_sys::Const>()).constisnull;
        if implicit && binding.queries.is_empty() {
            return unhandled();
        }
        if implicit
            && query_needs_other_relations(request.root, request.fcall, document, &binding.queries)
        {
            pgrx::error!(
                "tin.highlight() cannot infer its runtime query in this join or ordering \
                 expression: the query requires additional relations; pass the tinql query \
                 text explicitly as the highlight() query argument"
            );
        }
        let configured = configuration_args(&spec);
        let mut rewritten = PgList::<pg_sys::Node>::new();
        for position in 0..query_position {
            rewritten.push(args.get_ptr(position).expect("highlight argument exists"));
        }
        rewritten.push(if implicit {
            // Each search text is parsed on its own when the plan runs, since
            // parameters and other run-time expressions have no text until then.
            let mut queries = PgList::<pg_sys::Node>::new();
            for &query in &binding.queries {
                queries.push(pg_sys::copyObjectImpl(query.cast()).cast());
            }
            crate::score::make_text_array(queries)
        } else {
            supplied_query
        });
        for (offset, configured) in configured.into_iter().enumerate() {
            if !legacy {
                let supplied = args
                    .get_ptr(query_position + 1 + offset)
                    .expect("highlight analysis argument exists");
                if !agrees(supplied, configured) {
                    pgrx::error!("{CONFLICT}");
                }
            }
            rewritten.push(configured);
        }
        let ansi = query_position == 2;
        let mut types = vec![pg_sys::TEXTOID; query_position + 1];
        if ansi {
            types[1] = pg_sys::INT4OID;
        }
        if implicit {
            types[query_position] = pg_sys::TEXTARRAYOID;
        }
        types.extend([
            pg_sys::TEXTOID,
            pg_sys::TEXTOID,
            pg_sys::TEXTOID,
            pg_sys::TEXTOID,
            pg_sys::INT4OID,
            pg_sys::TEXTOID,
            pg_sys::TEXTOID,
            pg_sys::TEXTOID,
        ]);
        if !implicit
            && !legacy
            && pg_sys::equal((*request.fcall).args.cast(), rewritten.as_ptr().cast())
        {
            return unhandled();
        }
        let replacement = pg_sys::copyObjectImpl(request.fcall.cast()).cast::<pg_sys::FuncExpr>();
        if implicit {
            let bound = if ansi {
                c"tin.highlight_ansi_bound"
            } else {
                c"tin.highlight_bound"
            };
            (*replacement).funcid = crate::score::lookup_bound_function(bound, &types);
        } else if legacy {
            let base = if ansi { "highlight_ansi" } else { "highlight" };
            (*replacement).funcid = sibling_function((*request.fcall).funcid, base, &types);
        }
        (*replacement).args = rewritten.into_pg();
        Internal::from(Some(pg_sys::Datum::from(replacement as usize)))
    }
}

pgrx::extension_sql!(
    r#"
ALTER FUNCTION @extschema@.highlight_v1_0_3(pg_catalog.text, pg_catalog.text, pg_catalog.text, pg_catalog.text)
    SUPPORT @extschema@.highlight_support;
ALTER FUNCTION @extschema@.highlight_ansi_v1_0_3(pg_catalog.text, pg_catalog.int4, pg_catalog.text)
    SUPPORT @extschema@.highlight_support;
ALTER FUNCTION @extschema@.highlight(pg_catalog.text, pg_catalog.text, pg_catalog.text, pg_catalog.text,
    pg_catalog.text, pg_catalog.text, pg_catalog.text, pg_catalog.text, pg_catalog.int4, pg_catalog.text, pg_catalog.text, pg_catalog.text)
    SUPPORT @extschema@.highlight_support;
ALTER FUNCTION @extschema@.highlight_ansi(pg_catalog.text, pg_catalog.int4, pg_catalog.text,
    pg_catalog.text, pg_catalog.text, pg_catalog.text, pg_catalog.text, pg_catalog.int4, pg_catalog.text, pg_catalog.text, pg_catalog.text)
    SUPPORT @extschema@.highlight_support;
"#,
    name = "highlight_support_bindings",
    requires = [
        highlight,
        highlight_ansi,
        highlight_v1_0_3,
        highlight_ansi_v1_0_3,
        highlight_bound,
        highlight_ansi_bound,
        highlight_support
    ]
);

#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use super::*;
    use pgrx::pg_test;

    fn create_highlight_table() {
        pgrx::Spi::run(
            "CREATE TABLE lite_highlight_quals (id int, title text);
             INSERT INTO lite_highlight_quals VALUES (1, 'alpha zeta filler');
             CREATE INDEX ON lite_highlight_quals USING tin (title);",
        )
        .unwrap();
    }

    #[pg_test]
    fn parameterized_quals_highlight_every_term() {
        create_highlight_table();
        pgrx::Spi::run(
            "PREPARE lite_highlight(text, text) AS
               SELECT tin.highlight(title) FROM lite_highlight_quals
               WHERE title ==> $1 AND title ==> $2",
        )
        .unwrap();
        let marked = pgrx::Spi::get_one::<String>("EXECUTE lite_highlight('alpha', 'zeta')")
            .unwrap()
            .unwrap();
        assert_eq!(marked, "<b>alpha</b> <b>zeta</b> filler");
    }

    #[pg_test]
    fn several_searches_highlight_like_one_ored_search() {
        create_highlight_table();
        let highlight = |function: &str, quals: &str| {
            pgrx::Spi::get_one::<String>(&format!(
                "SELECT tin.{function}(title) FROM lite_highlight_quals WHERE {quals}"
            ))
            .unwrap()
            .unwrap()
        };
        for (separate, ored, expected) in [
            (
                "title ==> 'alpha' OR title ==> '\"zeta filler\"'",
                "title ==> '(alpha) OR (\"zeta filler\")'",
                "<b>alpha</b> <b>zeta filler</b>",
            ),
            // An empty search matches nothing and leaves the others to mark.
            (
                "title ==> 'zeta' OR title ==> ''",
                "title ==> 'zeta'",
                "alpha <b>zeta</b> filler",
            ),
        ] {
            assert_eq!(highlight("highlight", separate), expected);
            assert_eq!(highlight("highlight", ored), expected);
            assert_eq!(
                highlight("highlight_ansi", separate),
                highlight("highlight_ansi", ored)
            );
        }
    }

    #[pg_test]
    fn highlighting_survives_subquery_pullup() {
        create_highlight_table();
        let marked = pgrx::Spi::get_one::<String>(
            "SELECT h FROM (
               SELECT tin.highlight(title) AS h FROM lite_highlight_quals
               WHERE title ==> 'zeta'
             ) AS marked",
        )
        .unwrap()
        .unwrap();
        assert_eq!(marked, "alpha <b>zeta</b> filler");
    }

    #[pg_test]
    fn highlighting_binds_partial_indexes_like_scoring() {
        pgrx::Spi::run(
            "CREATE TABLE lite_highlight_partial (id int, title text, active boolean);
             INSERT INTO lite_highlight_partial VALUES
               (1, 'alpha zeta', true), (2, 'alpha < omega', false);
             CREATE INDEX ON lite_highlight_partial USING tin (title) WHERE active;
             CREATE TABLE lite_highlight_twin (title text, active boolean);
             INSERT INTO lite_highlight_twin VALUES ('alpha zeta', false);
             CREATE INDEX ON lite_highlight_twin USING tin (title) WHERE active;
             CREATE INDEX ON lite_highlight_twin USING tin (title);",
        )
        .unwrap();
        let highlight = |sql: &str| pgrx::Spi::get_one::<String>(sql).unwrap();
        assert_eq!(
            highlight(
                "SELECT tin.highlight(title) FROM lite_highlight_partial
                 WHERE active = true AND title ==> 'alpha'"
            ),
            Some("<b>alpha</b> zeta".into())
        );
        // Like tin, leave the text unmarked when no index can bind a search.
        for sql in [
            "SELECT tin.highlight(title) FROM lite_highlight_partial
             WHERE id = 2 AND title ==> 'alpha'",
            "SELECT tin.highlight_ansi(title) FROM lite_highlight_partial
             WHERE id = 2 AND title ==> 'alpha'",
            "SELECT tin.highlight(title) FROM lite_highlight_partial WHERE id = 2",
        ] {
            assert_eq!(highlight(sql), Some("alpha < omega".into()), "{sql}");
        }
        assert_eq!(
            highlight(
                "SELECT tin.highlight(NULL::text) FROM lite_highlight_partial
                 WHERE id = 2 AND title ==> 'alpha'"
            ),
            None
        );
        assert_eq!(
            highlight(
                "SELECT tin.highlight(title) FROM lite_highlight_twin WHERE title ==> 'alpha'"
            ),
            Some("<b>alpha</b> zeta".into())
        );
    }

    #[pg_test]
    fn explicit_html_and_ansi_highlighting_render_matches() {
        assert_eq!(
            highlight(
                Some("Hi there"),
                "<b>",
                "</b>",
                Some("hi"),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None
            ),
            Some("<b>Hi</b> there".into())
        );
        let ansi = highlight_ansi(
            Some("hi there"),
            None,
            Some("hi"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(ansi.contains("\x1b["));
        assert!(ansi.contains("hi"));
    }
}
