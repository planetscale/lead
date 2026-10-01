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
use crate::udfs::TokenizeOptions;
use pgrx::{Internal, IntoDatum, PgList, Spi, pg_extern, pg_sys};
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::rc::Rc;
use tokenizer::{
    CompiledTokenizerPipeline, Folding, GraphemeMode, LongTokenMode, PositionGapMode,
    TokenizerPipelineSpec, TokenizerSpec,
};

/// Marks a `==>` search expression that carries the analysis configuration of
/// the index it was planned against. Snowball stemming is not idempotent, so
/// the expression keeps the user's raw text and the configuration travels next
/// to it instead of being applied to the query early.
const TAG: &str = "\u{1}tin-analysis\u{1}";
const SEPARATOR: char = '\u{1}';

pub(crate) fn encode(spec: &TokenizerPipelineSpec) -> String {
    let folding = |value| match value {
        Folding::Fold => "fold",
        Folding::Preserve => "preserve",
    };
    [
        match spec.tokenizer {
            TokenizerSpec::Unicode => "unicode",
            TokenizerSpec::Whitespace => "whitespace",
        }
        .to_owned(),
        folding(spec.case_folding).to_owned(),
        folding(spec.accent_folding).to_owned(),
        match spec.long_tokens.mode {
            LongTokenMode::Truncate => "truncate",
            LongTokenMode::Discard => "discard",
            LongTokenMode::Split => "split",
        }
        .to_owned(),
        spec.long_tokens.max_bytes.to_string(),
        match spec.graphemes {
            GraphemeMode::Discard => "discard",
            GraphemeMode::Emoji => "emoji",
            GraphemeMode::Retain => "retain",
        }
        .to_owned(),
        match spec.position_gaps {
            PositionGapMode::Collapse => "collapse",
            PositionGapMode::Preserve => "preserve",
        }
        .to_owned(),
        spec.stemmer
            .map(|stemmer| stemmer.as_str().to_owned())
            .unwrap_or_default(),
    ]
    .join(",")
}

pub(crate) fn decode(config: &str) -> Result<TokenizerPipelineSpec, String> {
    let fields: Vec<&str> = config.split(',').collect();
    let [
        tokenizer,
        case_folding,
        accent_folding,
        long_tokens,
        max_token_bytes,
        graphemes,
        position_gaps,
        stemmer,
    ] = fields[..]
    else {
        return Err(format!("invalid analysis configuration {config:?}"));
    };
    TokenizeOptions {
        tokenizer,
        case_folding,
        accent_folding,
        long_tokens,
        max_token_bytes: max_token_bytes
            .parse()
            .map_err(|_| format!("invalid analysis configuration {config:?}"))?,
        graphemes,
        position_gaps,
        stemmer: (!stemmer.is_empty()).then_some(stemmer),
    }
    .into_spec()
}

pub(crate) fn tag(spec: &TokenizerPipelineSpec, raw: &str) -> String {
    format!("{TAG}{}{SEPARATOR}{raw}", encode(spec))
}

/// Splits a possibly tagged `==>` search expression into the analysis
/// configuration it was bound to and the user's raw query text.
pub(crate) fn split_tag(text: &str) -> (Option<&str>, &str) {
    match text
        .strip_prefix(TAG)
        .and_then(|rest| rest.split_once(SEPARATOR))
    {
        Some((config, raw)) => (Some(config), raw),
        None => (None, text),
    }
}

pub(crate) fn raw_query(text: &str) -> &str {
    split_tag(text).1
}

thread_local! {
    static PIPELINE: RefCell<Option<(String, Rc<CompiledTokenizerPipeline>)>> =
        const { RefCell::new(None) };
}

/// Compiles the pipeline named by a tag, reusing the previous one: a scan
/// evaluates the same configuration for every row.
pub(crate) fn pipeline_for(config: &str) -> Result<Rc<CompiledTokenizerPipeline>, String> {
    PIPELINE.with_borrow_mut(|slot| {
        if let Some((cached, pipeline)) = slot
            && cached == config
        {
            return Ok(Rc::clone(pipeline));
        }
        let pipeline = Rc::new(
            decode(config)?
                .compile()
                .map_err(|error| error.to_string())?,
        );
        *slot = Some((config.to_owned(), Rc::clone(&pipeline)));
        Ok(pipeline)
    })
}

#[pg_extern(immutable, parallel_safe)]
fn bind_query_analysis(query: &str, analysis: &str) -> String {
    format!("{TAG}{analysis}{SEPARATOR}{}", raw_query(query))
}

pub(crate) unsafe fn sibling_function(
    funcid: pg_sys::Oid,
    name: &str,
    types: &[pg_sys::Oid],
) -> pg_sys::Oid {
    let schema = unsafe { pg_sys::get_namespace_name(pg_sys::get_func_namespace(funcid)) };
    let name = CString::new(name).expect("function names contain no NUL");
    let qualified = unsafe { pg_sys::quote_qualified_identifier(schema, name.as_ptr()) };
    let names = unsafe { pg_sys::stringToQualifiedNameList(qualified, std::ptr::null_mut()) };
    unsafe { pg_sys::LookupFuncName(names, types.len() as i32, types.as_ptr(), false) }
}

pub(crate) unsafe fn text_const(value: &str) -> *mut pg_sys::Const {
    let datum = value.into_datum().expect("&str is never NULL");
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
    }
}

unsafe fn text_of(node: *mut pg_sys::Node) -> Option<String> {
    if node.is_null() || unsafe { (*node).type_ } != pg_sys::NodeTag::T_Const {
        return None;
    }
    let value = unsafe { &*node.cast::<pg_sys::Const>() };
    if value.constisnull || value.consttype != pg_sys::TEXTOID {
        return None;
    }
    unsafe { <String as pgrx::FromDatum>::from_datum(value.constvalue, false) }
}

unsafe fn is_bind_call(node: *mut pg_sys::Node) -> bool {
    if node.is_null() || unsafe { (*node).type_ } != pg_sys::NodeTag::T_FuncExpr {
        return false;
    }
    let function = unsafe { (*node.cast::<pg_sys::FuncExpr>()).funcid };
    let name = unsafe { pg_sys::get_func_name(function) };
    !name.is_null() && unsafe { CStr::from_ptr(name) }.to_bytes() == b"bind_query_analysis"
}

/// The user's search expression with any analysis binding removed.
pub(crate) unsafe fn unbound_query(node: *mut pg_sys::Node) -> *mut pg_sys::Node {
    if unsafe { is_bind_call(node) } {
        return unsafe { pg_sys::list_nth((*node.cast::<pg_sys::FuncExpr>()).args, 0).cast() };
    }
    match unsafe { text_of(node) } {
        Some(text) if split_tag(&text).0.is_some() => {
            unsafe { text_const(raw_query(&text)) }.cast()
        }
        _ => node,
    }
}

unsafe fn is_bound_query(node: *mut pg_sys::Node) -> bool {
    (unsafe { is_bind_call(node) })
        || unsafe { text_of(node) }.is_some_and(|text| split_tag(&text).0.is_some())
}

#[derive(Default)]
struct Leaves {
    specs: Vec<TokenizerPipelineSpec>,
    indexes: Vec<pg_sys::Oid>,
}

pub(crate) enum Resolution {
    /// The operand is not covered by any tin index.
    Unindexed,
    Bound {
        spec: TokenizerPipelineSpec,
        indexes: Vec<pg_sys::Oid>,
    },
    /// Members of a partitioned, inherited, or UNION ALL relation analyze
    /// the operand differently.
    Conflict,
}

/// Finds the analysis configuration of the tin index that covers `operand`,
/// looking through partitions, inheritance children, and UNION ALL arms.
pub(crate) unsafe fn resolve(
    root: *mut pg_sys::PlannerInfo,
    operand: *mut pg_sys::Node,
) -> Resolution {
    let mut leaves = Leaves::default();
    unsafe { collect(root, operand, &mut leaves) };
    let Some(&first) = leaves.specs.first() else {
        return Resolution::Unindexed;
    };
    if leaves.specs.iter().any(|spec| *spec != first) {
        return Resolution::Conflict;
    }
    if leaves.indexes.is_empty() {
        return Resolution::Unindexed;
    }
    Resolution::Bound {
        spec: first,
        indexes: leaves.indexes,
    }
}

unsafe fn collect(root: *mut pg_sys::PlannerInfo, operand: *mut pg_sys::Node, leaves: &mut Leaves) {
    let plain = |leaves: &mut Leaves| leaves.specs.push(TokenizerPipelineSpec::tin_default());
    let Some(varno) = (unsafe { crate::score::single_varno(operand) }) else {
        return plain(leaves);
    };
    let parse = unsafe { (*root).parse };
    if varno < 1 || varno > unsafe { pg_sys::list_length((*parse).rtable) } {
        return plain(leaves);
    }
    let rte =
        unsafe { pg_sys::list_nth((*parse).rtable, varno - 1) }.cast::<pg_sys::RangeTblEntry>();
    match unsafe { (*rte).rtekind } {
        pg_sys::RTEKind::RTE_RELATION => unsafe {
            let relid = (*rte).relid;
            let members = if (*rte).inh {
                descendants(relid)
            } else {
                vec![relid]
            };
            for member in members {
                match leaf_index((*root).parse, member, relid, varno, operand) {
                    Some((index, spec)) => {
                        leaves.indexes.push(index);
                        leaves.specs.push(spec);
                    }
                    None => plain(leaves),
                }
            }
        },
        pg_sys::RTEKind::RTE_SUBQUERY if unsafe { (*rte).inh } => unsafe {
            let arms = PgList::<pg_sys::AppendRelInfo>::from_pg((*root).append_rel_list);
            let mut found = false;
            for arm in arms.iter_ptr() {
                if (*arm).parent_relid != varno as pg_sys::Index {
                    continue;
                }
                found = true;
                let mut appinfo = arm;
                let translated = pg_sys::adjust_appendrel_attrs(
                    root,
                    pg_sys::copyObjectImpl(operand.cast()).cast(),
                    1,
                    &raw mut appinfo,
                );
                collect(root, translated, leaves);
            }
            if !found {
                plain(leaves);
            }
        },
        _ => plain(leaves),
    }
}

/// The relation and, for inheritance trees, every descendant that stores rows.
unsafe fn descendants(relid: pg_sys::Oid) -> Vec<pg_sys::Oid> {
    let has_children = unsafe {
        let class = pg_sys::SearchSysCache1(
            pg_sys::SysCacheIdentifier::RELOID as i32,
            pg_sys::Datum::from(relid),
        );
        if class.is_null() {
            return vec![relid];
        }
        let form = pg_sys::GETSTRUCT(class).cast::<pg_sys::FormData_pg_class>();
        let flag = (*form).relhassubclass;
        pg_sys::ReleaseSysCache(class);
        flag
    };
    if !has_children {
        return vec![relid];
    }
    let sql = format!(
        "WITH RECURSIVE tree(oid) AS (
           SELECT {}::pg_catalog.oid
           UNION ALL
           SELECT i.inhrelid FROM pg_catalog.pg_inherits i JOIN tree ON i.inhparent = tree.oid)
         SELECT tree.oid FROM tree JOIN pg_catalog.pg_class c ON c.oid = tree.oid
         WHERE c.relkind <> 'p'",
        relid.to_u32()
    );
    Spi::connect(|client| {
        client
            .select(&sql, None, &[])
            .expect("inheritance tree lookup")
            .filter_map(|row| row.get::<pg_sys::Oid>(1).ok().flatten())
            .collect()
    })
}

/// The tin index on `member` that covers `operand`, with its analysis.
/// `operand` is expressed in the columns of `parent`; inheritance children
/// may order their columns differently.
unsafe fn leaf_index(
    parse: *mut pg_sys::Query,
    member: pg_sys::Oid,
    parent: pg_sys::Oid,
    varno: i32,
    operand: *mut pg_sys::Node,
) -> Option<(pg_sys::Oid, TokenizerPipelineSpec)> {
    let index = if member == parent {
        unsafe { crate::score::find_matching_tin_index(parse, member, varno, operand) }
    } else {
        unsafe { translate_to_member(member, parent, varno, operand) }.and_then(
            |translated| unsafe {
                crate::score::find_matching_tin_index(std::ptr::null_mut(), member, 1, translated)
            },
        )
    }?;
    let spec = unsafe {
        let relation = pg_sys::index_open(index, pg_sys::AccessShareLock as _);
        let spec = crate::options::tokenizer_spec(relation);
        pg_sys::index_close(relation, pg_sys::AccessShareLock as _);
        spec
    };
    Some((index, spec))
}

unsafe fn translate_to_member(
    member: pg_sys::Oid,
    parent: pg_sys::Oid,
    varno: i32,
    operand: *mut pg_sys::Node,
) -> Option<*mut pg_sys::Node> {
    unsafe {
        let parent_rel = pg_sys::table_open(parent, pg_sys::AccessShareLock as _);
        let member_rel = pg_sys::table_open(member, pg_sys::AccessShareLock as _);
        // Maps parent column numbers to the member's.
        let map =
            pg_sys::build_attrmap_by_name_if_req((*member_rel).rd_att, (*parent_rel).rd_att, false);
        let normalized = pg_sys::copyObjectImpl(operand.cast()).cast::<pg_sys::Node>();
        pg_sys::ChangeVarNodes(normalized, varno, 1, 0);
        let mut whole_row = false;
        let translated = if map.is_null() {
            normalized
        } else {
            pg_sys::map_variable_attnos(
                normalized,
                1,
                0,
                map,
                (*(*member_rel).rd_rel).reltype,
                &mut whole_row,
            )
        };
        pg_sys::table_close(member_rel, pg_sys::AccessShareLock as _);
        pg_sys::table_close(parent_rel, pg_sys::AccessShareLock as _);
        (!whole_row).then_some(translated)
    }
}

/// Keeps cached plans honest: changing or rebuilding a covering index
/// invalidates any plan that bound its analysis.
pub(crate) unsafe fn depend_on_indexes(root: *mut pg_sys::PlannerInfo, indexes: &[pg_sys::Oid]) {
    unsafe {
        let glob = (*root).glob;
        for &index in indexes {
            if !pg_sys::list_member_oid((*glob).relationOids, index) {
                (*glob).relationOids = pg_sys::lappend_oid((*glob).relationOids, index);
            }
        }
    }
}

pub(crate) unsafe fn conflict_scope(
    root: *mut pg_sys::PlannerInfo,
    operand: *mut pg_sys::Node,
) -> String {
    unsafe {
        let varno = crate::score::single_varno(operand).unwrap_or(1);
        let rte =
            pg_sys::list_nth((*(*root).parse).rtable, varno - 1).cast::<pg_sys::RangeTblEntry>();
        let name = if (*rte).rtekind == pg_sys::RTEKind::RTE_RELATION {
            pg_sys::get_rel_name((*rte).relid)
        } else if !(*rte).eref.is_null() {
            (*(*rte).eref).aliasname
        } else {
            std::ptr::null_mut()
        };
        if name.is_null() {
            String::new()
        } else {
            CStr::from_ptr(name).to_string_lossy().into_owned()
        }
    }
}

fn unhandled() -> Internal {
    Internal::from(Some(pg_sys::Datum::from(0_usize)))
}

/// Planner support for `==>`: binds the analysis of the searched index to the
/// right-hand side so every evaluation of the operator, whether a bitmap
/// recheck, a filter, or a join qual, analyzes text the way the index does.
#[pg_extern(immutable, parallel_unsafe)]
fn tin_text_support(request: Internal) -> Internal {
    let Some(datum) = request.into_datum() else {
        return unhandled();
    };
    unsafe {
        let node = datum.cast_mut_ptr::<pg_sys::Node>();
        if node.is_null() || (*node).type_ != pg_sys::NodeTag::T_SupportRequestSimplify {
            return unhandled();
        }
        let request = &*node.cast::<pg_sys::SupportRequestSimplify>();
        if request.root.is_null()
            || request.fcall.is_null()
            || pg_sys::list_length((*request.fcall).args) != 2
        {
            return unhandled();
        }
        let left = pg_sys::list_nth((*request.fcall).args, 0).cast::<pg_sys::Node>();
        let right = pg_sys::list_nth((*request.fcall).args, 1).cast::<pg_sys::Node>();
        if right.is_null() || is_bound_query(right) {
            return unhandled();
        }
        if (*right).type_ == pg_sys::NodeTag::T_Const
            && (*right.cast::<pg_sys::Const>()).constisnull
        {
            return unhandled();
        }
        if crate::score::single_varno(left).is_none() {
            return unhandled();
        }
        let (spec, indexes) = match resolve(request.root, left) {
            Resolution::Unindexed => return unhandled(),
            Resolution::Conflict => pgrx::error!(
                "tin index tokenization differs across the members of \"{}\" for this search",
                conflict_scope(request.root, left)
            ),
            Resolution::Bound { spec, indexes } => (spec, indexes),
        };
        depend_on_indexes(request.root, &indexes);
        if spec == TokenizerPipelineSpec::tin_default() {
            return unhandled();
        }
        let bound_right: *mut pg_sys::Node = match text_of(right) {
            Some(text) => text_const(&tag(&spec, &text)).cast(),
            None => {
                let function = sibling_function(
                    (*request.fcall).funcid,
                    "bind_query_analysis",
                    &[pg_sys::TEXTOID, pg_sys::TEXTOID],
                );
                let mut args = PgList::<pg_sys::Node>::new();
                args.push(right);
                args.push(text_const(&encode(&spec)).cast());
                pg_sys::makeFuncExpr(
                    function,
                    pg_sys::TEXTOID,
                    args.into_pg(),
                    pg_sys::DEFAULT_COLLATION_OID,
                    pg_sys::DEFAULT_COLLATION_OID,
                    pg_sys::CoercionForm::COERCE_EXPLICIT_CALL,
                )
                .cast()
            }
        };
        let operator = operator_oid();
        let replacement = pg_sys::make_opclause(
            operator,
            pg_sys::BOOLOID,
            false,
            left.cast(),
            bound_right.cast(),
            pg_sys::InvalidOid,
            (*request.fcall).inputcollid,
        );
        pg_sys::set_opfuncid(replacement.cast());
        Internal::from(Some(pg_sys::Datum::from(replacement as usize)))
    }
}

unsafe fn operator_oid() -> pg_sys::Oid {
    let mut name = PgList::<pg_sys::Node>::new();
    name.push(unsafe { pg_sys::makeString(c"pg_catalog".as_ptr().cast_mut()) }.cast());
    name.push(unsafe { pg_sys::makeString(c"==>".as_ptr().cast_mut()) }.cast());
    unsafe {
        pg_sys::LookupOperName(
            std::ptr::null_mut(),
            name.into_pg(),
            pg_sys::TEXTOID,
            pg_sys::TEXTOID,
            false,
            -1,
        )
    }
}

pgrx::extension_sql!(
    r#"
ALTER FUNCTION @extschema@.tin_text_cmpfunc(pg_catalog.text, pg_catalog.text)
    SUPPORT @extschema@.tin_text_support;
"#,
    name = "tin_text_support_binding",
    requires = ["tin_text_operator", tin_text_support, bind_query_analysis]
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_round_trip_the_configuration_and_raw_text() {
        let mut spec = TokenizerPipelineSpec::tin_default();
        spec.stemmer = Some(tokenizer::Stemmer::French);
        let tagged = tag(&spec, "créées OR run*");
        let (config, raw) = split_tag(&tagged);
        assert_eq!(raw, "créées OR run*");
        assert_eq!(decode(config.unwrap()).unwrap(), spec);
        assert_eq!(split_tag("plain text"), (None, "plain text"));
    }
}
