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
//! Binds index analysis to `==> ANY(...)`.
//!
//! Postgres never asks a support function to simplify a `ScalarArrayOpExpr`,
//! so `tin_text_support` cannot reach `body ==> ANY(ARRAY[...])`. This module
//! covers it from `get_relation_info_hook`. The planner has already
//! preprocessed the query's expressions when it first opens a relation, and
//! has not yet distributed the quals to relations, so the hook can still
//! rewrite the array of every search in place. The hook runs for the first
//! query of a session too: Postgres reads it after opening the relation's
//! indexes, which is when it loads this library.

use crate::analysis::{bind_texts, bound_analysis, encode, sibling_function};
use pgrx::{FromDatum, IntoDatum, PgList, pg_guard, pg_sys};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::{CStr, c_void};

static mut PREVIOUS_HOOK: pg_sys::get_relation_info_hook_type = None;

thread_local! {
    static PROCESSED: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

/// Forgets a planner root when the memory context that holds it goes away,
/// since a later statement can allocate a new root at the same address.
struct ProcessedRoot(usize);

impl Drop for ProcessedRoot {
    fn drop(&mut self) {
        PROCESSED.with_borrow_mut(|processed| processed.remove(&self.0));
    }
}

pub(crate) fn init() {
    unsafe {
        PREVIOUS_HOOK = pg_sys::get_relation_info_hook;
        pg_sys::get_relation_info_hook = Some(relation_info_hook);
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn relation_info_hook(
    root: *mut pg_sys::PlannerInfo,
    relation_oid: pg_sys::Oid,
    inhparent: bool,
    rel: *mut pg_sys::RelOptInfo,
) {
    unsafe {
        if let Some(previous) = PREVIOUS_HOOK {
            previous(root, relation_oid, inhparent, rel);
        }
        bind_arrays(root);
    }
}

/// Binds the arrays of every `==> ANY(...)` in `root`'s query, once per root.
unsafe fn bind_arrays(root: *mut pg_sys::PlannerInfo) {
    if root.is_null() || unsafe { (*root).parse.is_null() } {
        return;
    }
    if !PROCESSED.with_borrow_mut(|processed| processed.insert(root as usize)) {
        return;
    }
    unsafe {
        let home = pg_sys::GetMemoryChunkContext(root.cast());
        pgrx::PgMemoryContexts::For(home).leak_and_drop_on_delete(ProcessedRoot(root as usize));
        let operator = crate::operator::search_operator();
        if operator == pg_sys::InvalidOid {
            return;
        }
        let mut context = Context { root, operator };
        // Subqueries that survive pull-up are planned with their own roots.
        pg_sys::query_tree_walker_impl((*root).parse, Some(walk), (&raw mut context).cast(), 0);
    }
}

struct Context {
    root: *mut pg_sys::PlannerInfo,
    operator: pg_sys::Oid,
}

#[pg_guard]
unsafe extern "C-unwind" fn walk(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
    if node.is_null() || unsafe { (*node).type_ } == pg_sys::NodeTag::T_Query {
        return false;
    }
    if unsafe { (*node).type_ } == pg_sys::NodeTag::T_ScalarArrayOpExpr {
        unsafe { bind_search(node.cast(), &*context.cast::<Context>()) };
    }
    unsafe { pg_sys::expression_tree_walker(node, Some(walk), context) }
}

unsafe fn bind_search(saop: *mut pg_sys::ScalarArrayOpExpr, context: &Context) {
    unsafe {
        if (*saop).opno != context.operator || pg_sys::list_length((*saop).args) != 2 {
            return;
        }
        let left = pg_sys::list_nth((*saop).args, 0).cast::<pg_sys::Node>();
        let array = pg_sys::list_nth((*saop).args, 1).cast::<pg_sys::Node>();
        if left.is_null() || array.is_null() || is_bound_array(array) {
            return;
        }
        if (*array).type_ == pg_sys::NodeTag::T_Const
            && (*array.cast::<pg_sys::Const>()).constisnull
        {
            return;
        }
        let Some(spec) = bound_analysis(context.root, left) else {
            return;
        };
        let analysis = encode(&spec);
        let bound = if (*array).type_ == pg_sys::NodeTag::T_Const {
            let Some(bound) = bind_constant(array.cast(), &analysis) else {
                return;
            };
            bound
        } else {
            pg_sys::set_sa_opfuncid(saop);
            bind_expression(array, &analysis, (*saop).opfuncid)
        };
        (*pg_sys::list_nth_cell((*saop).args, 1)).ptr_value = bound.cast();
    }
}

unsafe fn is_bound_array(node: *mut pg_sys::Node) -> bool {
    if unsafe { (*node).type_ } != pg_sys::NodeTag::T_FuncExpr {
        return false;
    }
    let function = unsafe { (*node.cast::<pg_sys::FuncExpr>()).funcid };
    let name = unsafe { pg_sys::get_func_name(function) };
    !name.is_null() && unsafe { CStr::from_ptr(name) }.to_bytes() == b"bind_query_array_analysis"
}

/// The constant array with every text bound, or `None` when nothing changes.
unsafe fn bind_constant(array: *mut pg_sys::Const, analysis: &str) -> Option<*mut pg_sys::Node> {
    unsafe {
        let texts = Vec::<Option<String>>::from_datum((*array).constvalue, false)?;
        let bound = bind_texts(analysis, texts.clone());
        if bound == texts {
            return None;
        }
        let constant = pg_sys::makeConst(
            (*array).consttype,
            (*array).consttypmod,
            (*array).constcollid,
            (*array).constlen,
            bound.into_datum()?,
            false,
            (*array).constbyval,
        );
        Some(constant.cast())
    }
}

/// Calls `bind_query_array_analysis` on an array that has no value until the
/// plan runs, such as a parameter.
unsafe fn bind_expression(
    array: *mut pg_sys::Node,
    analysis: &str,
    operator_function: pg_sys::Oid,
) -> *mut pg_sys::Node {
    unsafe {
        let function = sibling_function(
            operator_function,
            "bind_query_array_analysis",
            &[pg_sys::TEXTARRAYOID, pg_sys::TEXTOID],
        );
        let mut args = PgList::<pg_sys::Node>::new();
        args.push(array);
        args.push(crate::analysis::text_const(analysis).cast());
        pg_sys::makeFuncExpr(
            function,
            pg_sys::TEXTARRAYOID,
            args.into_pg(),
            pg_sys::DEFAULT_COLLATION_OID,
            pg_sys::DEFAULT_COLLATION_OID,
            pg_sys::CoercionForm::COERCE_EXPLICIT_CALL,
        )
        .cast()
    }
}
