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
#[allow(unused_imports)]
use crate::am::amhandler;
use pgrx::{extension_sql, pg_extern, pg_sys};
use tinql::runtime::{
    Query, SimplificationProfile, evaluate, lower::lower, simplify, subtokenize::sub_tokenize,
    tokenize_doc,
};
use tokenizer::Tokenizer;
use tokenizer::presets::default_pipeline;

fn parse_search<T: Tokenizer>(query_text: &str, tokenizer: &T) -> Result<Query, String> {
    let parsed = crate::tinql::parse(query_text).map_err(|e| e.to_string())?;
    let analyzed = sub_tokenize(parsed, tokenizer).map_err(|e| e.to_string())?;
    lower(&analyzed).map_err(|e| e.to_string())
}

/// Returns the OID of Lead's `==>(text, text)` operator, or `InvalidOid` if
/// it does not exist. It is looked up on each call because the extension can
/// be dropped and recreated.
fn search_operator() -> pg_sys::Oid {
    unsafe {
        let mut names = std::ptr::null_mut();
        for name in [c"pg_catalog", c"==>"] {
            names = pg_sys::lappend(
                names,
                pg_sys::makeString(pg_sys::pstrdup(name.as_ptr())).cast(),
            );
        }
        pg_sys::OpernameGetOprid(names, pg_sys::TEXTOID, pg_sys::TEXTOID)
    }
}

/// Returns the document and query operands of `node` when it is a search
/// with Lead's `==>` operator. Operators of the same name in other schemas
/// or for other types are not searches.
///
/// # Safety
/// `node` must be a valid expression node.
pub(crate) unsafe fn search_operands(
    node: *mut pg_sys::Node,
) -> Option<(*mut pg_sys::Node, *mut pg_sys::Node)> {
    unsafe {
        if (*node).type_ != pg_sys::NodeTag::T_OpExpr {
            return None;
        }
        let op = &*node.cast::<pg_sys::OpExpr>();
        if op.opno != search_operator() || pg_sys::list_length(op.args) != 2 {
            return None;
        }
        let left = pg_sys::list_nth(op.args, 0).cast::<pg_sys::Node>();
        let right = pg_sys::list_nth(op.args, 1).cast::<pg_sys::Node>();
        (!left.is_null()).then_some((left, right))
    }
}

fn invalid_search(error: String) -> ! {
    pgrx::error!("invalid ==> query: {error}")
}

/// Combines the search texts of several `==>` quals into the query that
/// scoring and highlighting evaluate. Each text is parsed on its own, so one
/// text cannot change how another parses, and a text `==>` rejects raises the
/// error `==>` raises for it. The parsed texts are ORed and simplified the way
/// lowering simplifies `a OR b`.
pub(crate) fn parse_searches<T: Tokenizer>(texts: &[String], tokenizer: &T) -> Query {
    let mut queries = texts
        .iter()
        .map(|text| parse_search(text, tokenizer).unwrap_or_else(|error| invalid_search(error)))
        .collect::<Vec<_>>();
    if queries.len() == 1 {
        return queries.remove(0);
    }
    simplify(
        Query::Disjunction {
            min: 1,
            children: queries,
        },
        SimplificationProfile::Structural,
    )
}

fn evaluate_text(document: &str, query_text: &str) -> Result<bool, String> {
    let (config, query_text) = crate::analysis::split_tag(query_text);
    match config {
        Some(config) => {
            let pipeline = crate::analysis::pipeline_for(config)?;
            evaluate_with(&pipeline, document, query_text)
        }
        None => evaluate_with(default_pipeline(), document, query_text),
    }
}

fn evaluate_with(
    pipeline: &tokenizer::CompiledTokenizerPipeline,
    document: &str,
    query_text: &str,
) -> Result<bool, String> {
    let query = parse_search(query_text, pipeline)?;
    let document = tokenize_doc(document, pipeline);
    evaluate(&query, &document)
        .map(|result| result.matched)
        .map_err(|e| e.to_string())
}

#[pg_extern(immutable, parallel_safe)]
pub fn tin_text_cmpfunc(document: &str, query: &str) -> bool {
    evaluate_text(document, query).unwrap_or_else(|error| invalid_search(error))
}

extension_sql!(
    r#"
CREATE OPERATOR pg_catalog.==> (
    PROCEDURE = @extschema@.tin_text_cmpfunc,
    LEFTARG = pg_catalog.text,
    RIGHTARG = pg_catalog.text
);

CREATE OPERATOR CLASS @extschema@.tin_text_ops DEFAULT FOR TYPE pg_catalog.text USING tin AS
    OPERATOR 1 pg_catalog.==>(pg_catalog.text, pg_catalog.text),
    STORAGE pg_catalog.text;
"#,
    name = "tin_text_operator",
    requires = [amhandler, tin_text_cmpfunc]
);

#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use super::evaluate_text;
    use pgrx::pg_test;

    #[pg_test]
    fn boolean_and_positional_queries_are_exact() {
        assert!(evaluate_text("A craft beer bar", "craft AND beer").unwrap());
        assert!(evaluate_text("A craft beer bar", "\"craft beer\"").unwrap());
        assert!(!evaluate_text("Beer for craft fans", "\"craft beer\"").unwrap());
    }

    #[pg_test]
    fn expansions_use_the_document_term_universe() {
        assert!(evaluate_text("brewhouse", "brew*").unwrap());
        assert!(evaluate_text("jalapeno", "jalapeño~1").unwrap());
        assert!(!evaluate_text("winery", "brew*").unwrap());
    }

    #[pg_test]
    fn empty_documents_do_not_match_match_all() {
        assert!(!evaluate_text("...", "*").unwrap());
    }

    #[pg_test]
    fn bound_analysis_stems_the_raw_query_once() {
        let mut spec = tokenizer::TokenizerPipelineSpec::tin_default();
        spec.stemmer = Some(tokenizer::Stemmer::English);
        let bound = crate::analysis::tag(&spec, "accidental");
        assert!(evaluate_text("an accidental", &bound).unwrap());
        assert!(!evaluate_text("an accident", &bound).unwrap());
        assert!(!evaluate_text("an accidental", "accident").unwrap());
    }

    #[pg_test]
    fn invalid_queries_are_reported() {
        assert!(evaluate_text("beer", "beer OR").is_err());
    }
}
