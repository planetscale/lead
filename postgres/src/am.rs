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
use pgrx::{PgBox, pg_extern, pg_guard, pg_sys};
use std::ffi::c_void;

#[pg_extern(sql = "
    CREATE OR REPLACE FUNCTION @extschema@.amhandler(internal)
        RETURNS index_am_handler
        PARALLEL SAFE IMMUTABLE STRICT
        LANGUAGE c AS 'MODULE_PATHNAME', '@FUNCTION_NAME@';
    CREATE ACCESS METHOD tin TYPE INDEX HANDLER @extschema@.amhandler;
")]
pub(crate) fn amhandler(_fcinfo: pg_sys::FunctionCallInfo) -> PgBox<pg_sys::IndexAmRoutine> {
    let mut routine =
        unsafe { PgBox::<pg_sys::IndexAmRoutine>::alloc_node(pg_sys::NodeTag::T_IndexAmRoutine) };
    routine.amstrategies = 1;
    routine.amsupport = 0;
    routine.amcanmulticol = false;
    routine.amsearcharray = false;
    routine.amkeytype = pg_sys::InvalidOid;
    routine.amvalidate = Some(amvalidate);
    routine.ambuild = Some(ambuild);
    routine.ambuildempty = Some(ambuildempty);
    routine.aminsert = Some(aminsert);
    routine.ambulkdelete = Some(ambulkdelete);
    routine.amvacuumcleanup = Some(amvacuumcleanup);
    routine.amcostestimate = Some(amcostestimate);
    routine.amoptions = Some(crate::options::amoptions);
    routine.ambeginscan = Some(ambeginscan);
    routine.amrescan = Some(amrescan);
    routine.amgetbitmap = Some(amgetbitmap);
    routine.amendscan = Some(amendscan);
    routine.into_pg_boxed()
}

#[pg_guard]
unsafe extern "C-unwind" fn amvalidate(_opclassoid: pg_sys::Oid) -> bool {
    true
}

#[pg_guard]
unsafe extern "C-unwind" fn ambuild(
    heap: pg_sys::Relation,
    index: pg_sys::Relation,
    index_info: *mut pg_sys::IndexInfo,
) -> *mut pg_sys::IndexBuildResult {
    let mut index_tuples = 0_u64;
    let heap_tuples = unsafe {
        pg_sys::table_index_build_scan(
            heap,
            index,
            index_info,
            true,
            true,
            Some(build_callback),
            (&mut index_tuples as *mut u64).cast(),
            std::ptr::null_mut(),
        )
    };
    let mut result = unsafe { PgBox::<pg_sys::IndexBuildResult>::alloc0() };
    result.heap_tuples = heap_tuples;
    result.index_tuples = index_tuples as f64;
    result.into_pg_boxed().into_pg()
}

#[pg_guard]
unsafe extern "C-unwind" fn build_callback(
    _index: pg_sys::Relation,
    _tid: pg_sys::ItemPointer,
    _values: *mut pg_sys::Datum,
    _isnull: *mut bool,
    _tuple_is_alive: bool,
    state: *mut c_void,
) {
    unsafe { *state.cast::<u64>() += 1 };
}

#[pg_guard]
unsafe extern "C-unwind" fn ambuildempty(_index: pg_sys::Relation) {}

#[pg_guard]
#[expect(
    clippy::too_many_arguments,
    reason = "PostgreSQL index AM callback signature"
)]
unsafe extern "C-unwind" fn aminsert(
    _index: pg_sys::Relation,
    _values: *mut pg_sys::Datum,
    _isnull: *mut bool,
    _heap_tid: pg_sys::ItemPointer,
    _heap: pg_sys::Relation,
    _check_unique: pg_sys::IndexUniqueCheck::Type,
    _index_unchanged: bool,
    _index_info: *mut pg_sys::IndexInfo,
) -> bool {
    false
}

struct ScanState {
    active: bool,
}

#[pg_guard]
unsafe extern "C-unwind" fn ambeginscan(
    index: pg_sys::Relation,
    nkeys: i32,
    norderbys: i32,
) -> pg_sys::IndexScanDesc {
    let scan = unsafe { pg_sys::RelationGetIndexScan(index, nkeys, norderbys) };
    unsafe {
        (*scan).opaque = Box::into_raw(Box::new(ScanState { active: false })).cast();
    }
    scan
}

#[pg_guard]
unsafe extern "C-unwind" fn amrescan(
    scan: pg_sys::IndexScanDesc,
    keys: pg_sys::ScanKey,
    nkeys: i32,
    _orderbys: pg_sys::ScanKey,
    _norderbys: i32,
) {
    let state = unsafe { &mut *(*scan).opaque.cast::<ScanState>() };
    if nkeys <= 0 {
        state.active = false;
        return;
    }
    let keys = if keys.is_null() {
        unsafe { (*scan).keyData }
    } else {
        keys
    };
    if keys.is_null() {
        state.active = false;
        return;
    }
    state.active = unsafe { std::slice::from_raw_parts(keys, nkeys as usize) }
        .iter()
        .all(|key| key.sk_flags & pg_sys::SK_ISNULL as i32 == 0);
}

#[pg_guard]
unsafe extern "C-unwind" fn amgetbitmap(
    scan: pg_sys::IndexScanDesc,
    bitmap: *mut pg_sys::TIDBitmap,
) -> i64 {
    let state = unsafe { &*(*scan).opaque.cast::<ScanState>() };
    if !state.active {
        return 0;
    }
    let index = unsafe { (*scan).indexRelation };
    let heap_oid = unsafe { (*(*index).rd_index).indrelid };
    let heap = unsafe { pg_sys::table_open(heap_oid, pg_sys::NoLock as _) };
    let heap_blocks =
        unsafe { pg_sys::RelationGetNumberOfBlocksInFork(heap, pg_sys::ForkNumber::MAIN_FORKNUM) };
    unsafe { pg_sys::table_close(heap, pg_sys::NoLock as _) };
    // Lossy pages make PostgreSQL check every visible tuple against the
    // original query, including partial-index predicates and expressions.
    for block in 0..heap_blocks {
        pgrx::check_for_interrupts!();
        unsafe { pg_sys::tbm_add_page(bitmap, block) };
    }
    // Like BRIN, estimate ten tuples per page for scan statistics only.
    i64::from(heap_blocks) * 10
}

#[pg_guard]
unsafe extern "C-unwind" fn amendscan(scan: pg_sys::IndexScanDesc) {
    let state = unsafe { (*scan).opaque.cast::<ScanState>() };
    if !state.is_null() {
        unsafe { drop(Box::from_raw(state)) };
        unsafe { (*scan).opaque = std::ptr::null_mut() };
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn ambulkdelete(
    _info: *mut pg_sys::IndexVacuumInfo,
    stats: *mut pg_sys::IndexBulkDeleteResult,
    _callback: pg_sys::IndexBulkDeleteCallback,
    _callback_state: *mut c_void,
) -> *mut pg_sys::IndexBulkDeleteResult {
    stats
}

#[pg_guard]
unsafe extern "C-unwind" fn amvacuumcleanup(
    _info: *mut pg_sys::IndexVacuumInfo,
    stats: *mut pg_sys::IndexBulkDeleteResult,
) -> *mut pg_sys::IndexBulkDeleteResult {
    stats
}

#[pg_guard]
#[expect(
    clippy::too_many_arguments,
    reason = "PostgreSQL index AM callback signature"
)]
unsafe extern "C-unwind" fn amcostestimate(
    _root: *mut pg_sys::PlannerInfo,
    path: *mut pg_sys::IndexPath,
    loop_count: f64,
    startup: *mut pg_sys::Cost,
    total: *mut pg_sys::Cost,
    selectivity: *mut pg_sys::Selectivity,
    correlation: *mut f64,
    pages: *mut f64,
) {
    let tuples = unsafe { (*(*(*path).indexinfo).rel).tuples.max(1.0) };
    unsafe {
        *startup = pg_sys::seq_page_cost * loop_count;
        *total = *startup + tuples * pg_sys::cpu_operator_cost * loop_count;
        *selectivity = 0.1;
        *correlation = 0.0;
        *pages = (tuples / 512.0).ceil();
    }
}
