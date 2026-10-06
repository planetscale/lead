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
use crate::highlight::{highlight_text, highlight_text_ansi, query_positions, rewrap_text};
use pgrx::{Internal, IntoDatum, PgList, default, pg_extern, pg_guard, pg_sys};
use std::borrow::Cow;
use std::ffi::{CStr, c_void};
use tinql::runtime::{Query, parse_tinql_to_query_default};
use tokenizer::presets::default_pipeline;

fn render_highlight(text: &str, begin_tag: &str, end_tag: &str, query: Option<&Query>) -> String {
    // Without an explicit query or a tin index to bind one from, tin leaves
    // the text unmarked.
    let positions = query
        .map(|query| query_positions(query, text))
        .unwrap_or_default();
    highlight_text(text, begin_tag, end_tag, &positions)
        .unwrap_or_else(|error| pgrx::error!("{error}"))
}

fn render_highlight_ansi(text: &str, wrap_to: Option<i32>, query: Option<&Query>) -> String {
    let text = match wrap_to {
        Some(width) if width <= 0 => pgrx::error!("wrap_to must be positive"),
        Some(width) => Cow::Owned(rewrap_text(text, width as usize)),
        None => Cow::Borrowed(text),
    };
    let positions = query
        .map(|query| query_positions(query, text.as_ref()))
        .unwrap_or_default();
    if positions.is_empty() {
        return text.into_owned();
    }
    highlight_text_ansi(text.as_ref(), &positions).unwrap_or_else(|error| pgrx::error!("{error}"))
}

/// Parses an explicit highlight query. A query that does not parse marks
/// nothing.
fn explicit_query(query: Option<&str>) -> Option<Query> {
    parse_tinql_to_query_default(query?).ok()
}

/// Combines the search texts that `highlight_support` bound. A NULL text
/// matches no rows, so it marks nothing.
fn bound_query(queries: Vec<Option<String>>) -> Option<Query> {
    let texts = queries.into_iter().collect::<Option<Vec<_>>>()?;
    Some(crate::operator::parse_searches(&texts, default_pipeline()))
}

#[pg_extern(name = "highlight", immutable, parallel_safe)]
fn highlight(
    text: Option<&str>,
    begin_tag: default!(&str, "'<b>'"),
    end_tag: default!(&str, "'</b>'"),
    query: default!(Option<&str>, "NULL"),
) -> Option<String> {
    Some(render_highlight(
        text?,
        begin_tag,
        end_tag,
        explicit_query(query).as_ref(),
    ))
}

#[pg_extern(name = "highlight_ansi", immutable, parallel_safe)]
fn highlight_ansi(
    text: Option<&str>,
    wrap_to: default!(Option<i32>, "NULL"),
    query: default!(Option<&str>, "NULL"),
) -> Option<String> {
    Some(render_highlight_ansi(
        text?,
        wrap_to,
        explicit_query(query).as_ref(),
    ))
}

/// `highlight` with the search texts of the quals `highlight_support` bound.
#[pg_extern(immutable, parallel_safe)]
fn highlight_bound(
    text: Option<&str>,
    begin_tag: &str,
    end_tag: &str,
    queries: Vec<Option<String>>,
) -> Option<String> {
    Some(render_highlight(
        text?,
        begin_tag,
        end_tag,
        bound_query(queries).as_ref(),
    ))
}

/// `highlight_ansi` with the search texts of the quals `highlight_support`
/// bound.
#[pg_extern(immutable, parallel_safe)]
fn highlight_ansi_bound(
    text: Option<&str>,
    wrap_to: Option<i32>,
    queries: Vec<Option<String>>,
) -> Option<String> {
    Some(render_highlight_ansi(
        text?,
        wrap_to,
        bound_query(queries).as_ref(),
    ))
}

struct QueryContext {
    document: *mut pg_sys::Node,
    queries: Vec<*mut pg_sys::Node>,
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
    if unsafe { (*node).type_ } == pg_sys::NodeTag::T_OpExpr {
        let op = node.cast::<pg_sys::OpExpr>();
        let name = unsafe { pg_sys::get_opname((*op).opno) };
        if !name.is_null()
            && unsafe { CStr::from_ptr(name) }.to_bytes() == b"==>"
            && unsafe { pg_sys::list_length((*op).args) } == 2
        {
            let left = unsafe { pg_sys::list_nth((*op).args, 0).cast::<pg_sys::Node>() };
            if unsafe { pg_sys::equal(left.cast(), context.document.cast()) } {
                context
                    .queries
                    .push(unsafe { pg_sys::list_nth((*op).args, 1).cast::<pg_sys::Node>() });
            }
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
        let (query_position, bound, types): (_, _, &[pg_sys::Oid]) = if name == b"highlight" {
            (
                3,
                c"tin.highlight_bound",
                &[
                    pg_sys::TEXTOID,
                    pg_sys::TEXTOID,
                    pg_sys::TEXTOID,
                    pg_sys::TEXTARRAYOID,
                ],
            )
        } else if name == b"highlight_ansi" {
            (
                2,
                c"tin.highlight_ansi_bound",
                &[pg_sys::TEXTOID, pg_sys::INT4OID, pg_sys::TEXTARRAYOID],
            )
        } else {
            return unhandled();
        };
        if pg_sys::list_length((*request.fcall).args) <= query_position {
            return unhandled();
        }
        let supplied_query =
            pg_sys::list_nth((*request.fcall).args, query_position).cast::<pg_sys::Node>();
        if supplied_query.is_null() || (*supplied_query).type_ != pg_sys::NodeTag::T_Const {
            return unhandled();
        }
        if !(*supplied_query.cast::<pg_sys::Const>()).constisnull {
            return unhandled();
        }
        let document = pg_sys::list_nth((*request.fcall).args, 0).cast::<pg_sys::Node>();
        let Some(varno) = crate::score::single_varno(document) else {
            return unhandled();
        };
        let parse = (*request.root).parse;
        let rte = pg_sys::list_nth((*parse).rtable, varno - 1).cast::<pg_sys::RangeTblEntry>();
        if rte.is_null()
            || (*rte).rtekind != pg_sys::RTEKind::RTE_RELATION
            || crate::score::find_matching_tin_index(parse, (*rte).relid, varno, document).is_none()
        {
            return unhandled();
        }
        let mut binding = QueryContext {
            document,
            queries: Vec::new(),
        };
        // Pulled-up subqueries leave their quals in nested FromExpr nodes.
        collect_queries(
            (*parse).jointree.cast::<pg_sys::Node>(),
            (&mut binding as *mut QueryContext).cast(),
        );
        if binding.queries.is_empty() {
            return unhandled();
        }
        // Each search text is parsed on its own when the plan runs, since
        // parameters and other run-time expressions have no text until then.
        let mut queries = PgList::<pg_sys::Node>::new();
        for &query in &binding.queries {
            queries.push(query);
        }
        let query = crate::score::make_text_array(queries);
        let replacement = pg_sys::copyObjectImpl(request.fcall.cast()).cast::<pg_sys::FuncExpr>();
        (*replacement).funcid = crate::score::lookup_bound_function(bound, types);
        let mut args = PgList::<pg_sys::Node>::new();
        for position in 0..pg_sys::list_length((*request.fcall).args) {
            let argument = if position == query_position {
                query
            } else {
                pg_sys::list_nth((*request.fcall).args, position).cast::<pg_sys::Node>()
            };
            args.push(pg_sys::copyObjectImpl(argument.cast()).cast());
        }
        (*replacement).args = args.into_pg();
        Internal::from(Some(pg_sys::Datum::from(replacement as usize)))
    }
}

pgrx::extension_sql!(
    r#"
ALTER FUNCTION @extschema@.highlight(pg_catalog.text, pg_catalog.text, pg_catalog.text, pg_catalog.text)
    SUPPORT @extschema@.highlight_support;
ALTER FUNCTION @extschema@.highlight_ansi(pg_catalog.text, pg_catalog.int4, pg_catalog.text)
    SUPPORT @extschema@.highlight_support;
"#,
    name = "highlight_support_bindings",
    requires = [
        highlight,
        highlight_ansi,
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
            highlight(Some("Hi there"), "<b>", "</b>", Some("hi")),
            Some("<b>Hi</b> there".into())
        );
        let ansi = highlight_ansi(Some("hi there"), None, Some("hi")).unwrap();
        assert!(ansi.contains("\x1b["));
        assert!(ansi.contains("hi"));
    }
}
