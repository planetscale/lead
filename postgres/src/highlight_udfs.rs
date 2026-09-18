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
use crate::highlight::{highlight_text, highlight_text_ansi, positions_from_query, rewrap_text};
use pgrx::{FromDatum, Internal, IntoDatum, PgList, default, pg_extern, pg_guard, pg_sys};
use std::borrow::Cow;
use std::ffi::{CStr, c_void};

fn missing_binding(function: &str) -> ! {
    pgrx::error!("{function} requires an explicit query or a matching tin index scan")
}

fn render_highlight(
    text: Option<&str>,
    begin_tag: &str,
    end_tag: &str,
    query: Option<&str>,
) -> Option<String> {
    let text = text?;
    let query = query.unwrap_or_else(|| missing_binding("tin.highlight()"));
    let positions = positions_from_query(query, text);
    highlight_text(text, begin_tag, end_tag, &positions)
        .map(Some)
        .unwrap_or_else(|error| pgrx::error!("{error}"))
}

fn render_highlight_ansi(
    text: Option<&str>,
    wrap_to: Option<i32>,
    query: Option<&str>,
) -> Option<String> {
    let text = text?;
    let query = query.unwrap_or_else(|| missing_binding("tin.highlight_ansi()"));
    let text = match wrap_to {
        Some(width) if width <= 0 => pgrx::error!("wrap_to must be positive"),
        Some(width) => Cow::Owned(rewrap_text(text, width as usize)),
        None => Cow::Borrowed(text),
    };
    let positions = positions_from_query(query, text.as_ref());
    if positions.is_empty() {
        return Some(text.into_owned());
    }
    highlight_text_ansi(text.as_ref(), &positions)
        .map(Some)
        .unwrap_or_else(|error| pgrx::error!("{error}"))
}

#[pg_extern(name = "highlight", immutable, parallel_safe)]
fn highlight(
    text: Option<&str>,
    begin_tag: default!(&str, "'<b>'"),
    end_tag: default!(&str, "'</b>'"),
    query: default!(Option<&str>, "NULL"),
) -> Option<String> {
    render_highlight(text, begin_tag, end_tag, query)
}

#[pg_extern(name = "highlight_ansi", immutable, parallel_safe)]
fn highlight_ansi(
    text: Option<&str>,
    wrap_to: default!(Option<i32>, "NULL"),
    query: default!(Option<&str>, "NULL"),
) -> Option<String> {
    render_highlight_ansi(text, wrap_to, query)
}

struct VarContext {
    varno: i32,
    seen: bool,
    valid: bool,
}

#[pg_guard]
unsafe extern "C-unwind" fn collect_varno(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
    if node.is_null() {
        return false;
    }
    let context = unsafe { &mut *context.cast::<VarContext>() };
    if unsafe { (*node).type_ } == pg_sys::NodeTag::T_Var {
        let var = unsafe { &*node.cast::<pg_sys::Var>() };
        if var.varlevelsup != 0 || (context.seen && context.varno != var.varno) {
            context.valid = false;
        } else {
            context.varno = var.varno;
            context.seen = true;
        }
        return false;
    }
    unsafe {
        pg_sys::expression_tree_walker(
            node,
            Some(collect_varno),
            (context as *mut VarContext).cast(),
        )
    }
}

struct QueryContext {
    document: *mut pg_sys::Node,
    queries: Vec<*mut pg_sys::Node>,
}

#[pg_guard]
unsafe extern "C-unwind" fn collect_queries(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
    if node.is_null() {
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

unsafe fn combined_query(queries: &[*mut pg_sys::Node]) -> *mut pg_sys::Node {
    if queries.len() < 2 {
        return unsafe { pg_sys::copyObjectImpl(queries[0].cast()).cast() };
    }
    let mut text = Vec::with_capacity(queries.len());
    for &query in queries {
        if query.is_null() || unsafe { (*query).type_ } != pg_sys::NodeTag::T_Const {
            return unsafe { pg_sys::copyObjectImpl(queries[0].cast()).cast() };
        }
        let value = unsafe { &*query.cast::<pg_sys::Const>() };
        if value.constisnull || value.consttype != pg_sys::TEXTOID {
            return unsafe { pg_sys::copyObjectImpl(queries[0].cast()).cast() };
        }
        let Some(value) = (unsafe { String::from_datum(value.constvalue, false) }) else {
            return unsafe { pg_sys::copyObjectImpl(queries[0].cast()).cast() };
        };
        text.push(format!("({value})"));
    }
    let datum = text
        .join(" OR ")
        .into_datum()
        .expect("String is never NULL");
    unsafe {
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
    }
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
        let query_position = if name == b"highlight" {
            3
        } else if name == b"highlight_ansi" {
            2
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
        let mut vars = VarContext {
            varno: 0,
            seen: false,
            valid: true,
        };
        collect_varno(document, (&mut vars as *mut VarContext).cast());
        if !vars.valid || !vars.seen {
            return unhandled();
        }
        let parse = (*request.root).parse;
        let rte = pg_sys::list_nth((*parse).rtable, vars.varno - 1).cast::<pg_sys::RangeTblEntry>();
        if rte.is_null()
            || (*rte).rtekind != pg_sys::RTEKind::RTE_RELATION
            || crate::score::find_matching_tin_index((*rte).relid, vars.varno, document).is_none()
        {
            return unhandled();
        }
        let mut binding = QueryContext {
            document,
            queries: Vec::new(),
        };
        collect_queries(
            (*(*parse).jointree).quals.cast::<pg_sys::Node>(),
            (&mut binding as *mut QueryContext).cast(),
        );
        if binding.queries.is_empty() {
            return unhandled();
        }
        let query = combined_query(&binding.queries);
        let replacement = pg_sys::copyObjectImpl(request.fcall.cast()).cast::<pg_sys::FuncExpr>();
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
    requires = [highlight, highlight_ansi, highlight_support]
);

#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use super::*;
    use pgrx::pg_test;

    #[pg_test]
    fn explicit_html_and_ansi_highlighting_render_matches() {
        assert_eq!(
            render_highlight(Some("Hi there"), "<b>", "</b>", Some("hi")),
            Some("<b>Hi</b> there".into())
        );
        let ansi = render_highlight_ansi(Some("hi there"), None, Some("hi")).unwrap();
        assert!(ansi.contains("\x1b["));
        assert!(ansi.contains("hi"));
    }
}
