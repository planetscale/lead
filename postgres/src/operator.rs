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
use pgrx::{extension_sql, pg_extern};
use tinql::runtime::{
    Query, SimplificationProfile, evaluate, lower::lower, simplify, subtokenize::sub_tokenize,
    tokenize_doc,
};
use tokenizer::Tokenizer;
use tokenizer::presets::default_pipeline;

fn parse_search<T: Tokenizer>(query_text: &str, tokenizer: &T) -> Result<Query, String> {
    let parsed = tinql::parse(query_text, tinql::ImplicitOp::And).map_err(|e| e.to_string())?;
    let analyzed = sub_tokenize(parsed, tokenizer).map_err(|e| e.to_string())?;
    lower(&analyzed).map_err(|e| e.to_string())
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
    let pipeline = default_pipeline();
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

#[cfg(test)]
mod tests {
    use super::evaluate_text;

    #[test]
    fn boolean_and_positional_queries_are_exact() {
        assert!(evaluate_text("A craft beer bar", "craft AND beer").unwrap());
        assert!(evaluate_text("A craft beer bar", "\"craft beer\"").unwrap());
        assert!(!evaluate_text("Beer for craft fans", "\"craft beer\"").unwrap());
    }

    #[test]
    fn expansions_use_the_document_term_universe() {
        assert!(evaluate_text("brewhouse", "brew*").unwrap());
        assert!(evaluate_text("jalapeno", "jalapeño~1").unwrap());
        assert!(!evaluate_text("winery", "brew*").unwrap());
    }

    #[test]
    fn empty_documents_do_not_match_match_all() {
        assert!(!evaluate_text("...", "*").unwrap());
    }

    #[test]
    fn invalid_queries_are_reported() {
        assert!(evaluate_text("beer", "beer OR").is_err());
    }
}
