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
use pgrx::pg_guard;

::pgrx::pg_module_magic!(name);

mod am;
mod bm25;
mod highlight;
mod highlight_udfs;
mod match_positions;
mod operator;
pub(crate) mod options;
mod score;
mod tf_bucket;
mod udfs;

#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
    options::init();
}

#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec!["shared_preload_libraries=''"]
    }
}

#[cfg(feature = "pg_test")]
#[pgrx::pg_schema]
mod tests {
    use pgrx::Json;
    use pgrx::prelude::*;

    #[pg_test]
    fn bitmap_index_rechecks_heap_pages_without_preloading() {
        assert_eq!(
            Spi::get_one::<String>("SHOW shared_preload_libraries").unwrap(),
            Some(String::new())
        );
        Spi::run("CREATE TABLE lite_search (id int, body text)").unwrap();
        Spi::run(
            "INSERT INTO lite_search VALUES
               (1, 'craft beer'), (2, 'wine'), (3, 'beer festival')",
        )
        .unwrap();
        Spi::run("CREATE INDEX lite_search_idx ON lite_search USING tin (body)").unwrap();
        Spi::run("SET LOCAL enable_seqscan = off").unwrap();
        let ids = Spi::get_one::<Vec<i32>>(
            "SELECT array_agg(id ORDER BY id) FROM lite_search WHERE body ==> 'beer'",
        )
        .unwrap();
        assert_eq!(ids, Some(vec![1, 3]));
        let plan = Spi::get_one::<Json>(
            "EXPLAIN (ANALYZE, FORMAT JSON)
             SELECT id FROM lite_search WHERE body ==> 'beer'",
        )
        .unwrap()
        .unwrap()
        .0;
        assert_eq!(plan[0]["Plan"]["Node Type"], "Bitmap Heap Scan");
        assert_eq!(plan[0]["Plan"]["Lossy Heap Blocks"], 1);
        assert_eq!(plan[0]["Plan"]["Plans"][0]["Index Name"], "lite_search_idx");
    }

    #[pg_test]
    fn bitmap_scan_follows_heap_growth_and_truncate() {
        Spi::run(
            "CREATE TABLE lite_growth (id int, body text);
             CREATE INDEX lite_growth_idx ON lite_growth USING tin (body);
             SET LOCAL enable_seqscan = off;",
        )
        .unwrap();
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM lite_growth WHERE body ==> 'beer'").unwrap(),
            Some(0)
        );
        Spi::run(
            "INSERT INTO lite_growth
               SELECT n, CASE WHEN n % 50 = 0 THEN 'beer' ELSE 'wine' END
                         || repeat(' filler', 80)
               FROM generate_series(1, 400) AS n;",
        )
        .unwrap();
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM lite_growth WHERE body ==> 'beer'").unwrap(),
            Some(8)
        );
        Spi::run(
            "TRUNCATE lite_growth;
             INSERT INTO lite_growth VALUES (1, 'beer'), (2, 'wine');",
        )
        .unwrap();
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM lite_growth WHERE body ==> 'beer'").unwrap(),
            Some(1)
        );
    }

    #[pg_test]
    fn bitmap_scan_rechecks_partial_index_predicates_and_expressions() {
        Spi::run(
            "CREATE TABLE lite_partial (id int, body text, active boolean);
             INSERT INTO lite_partial VALUES
               (1, 'BEER', true), (2, 'wine', true),
               (3, 'BEER', false), (4, NULL, true);
             CREATE INDEX lite_partial_idx ON lite_partial
               USING tin (lower(body)) WHERE active;
             SET LOCAL enable_seqscan = off;",
        )
        .unwrap();
        let plan = Spi::get_one::<Json>(
            "EXPLAIN (FORMAT JSON)
             SELECT id FROM lite_partial WHERE active AND lower(body) ==> 'beer'",
        )
        .unwrap()
        .unwrap()
        .0;
        assert_eq!(plan[0]["Plan"]["Node Type"], "Bitmap Heap Scan");
        assert_eq!(
            plan[0]["Plan"]["Plans"][0]["Index Name"],
            "lite_partial_idx"
        );
        assert_eq!(
            Spi::get_one::<Vec<i32>>(
                "SELECT array_agg(id ORDER BY id) FROM lite_partial
                 WHERE active AND lower(body) ==> 'beer'"
            )
            .unwrap(),
            Some(vec![1])
        );
        Spi::run("UPDATE lite_partial SET active = true WHERE id = 3").unwrap();
        assert_eq!(
            Spi::get_one::<Vec<i32>>(
                "SELECT array_agg(id ORDER BY id) FROM lite_partial
                 WHERE active AND lower(body) ==> 'beer'"
            )
            .unwrap(),
            Some(vec![1, 3])
        );
    }

    #[pg_test]
    fn bitmap_union_rechecks_both_search_predicates() {
        Spi::run(
            "CREATE TABLE lite_union (id int, title text, body text);
             INSERT INTO lite_union VALUES
               (1, 'beer', 'wine'), (2, 'wine', 'beer'),
               (3, 'beer', 'beer'), (4, 'wine', 'wine');
             CREATE INDEX lite_union_title_idx ON lite_union USING tin (title);
             CREATE INDEX lite_union_body_idx ON lite_union USING tin (body);
             SET LOCAL enable_seqscan = off;",
        )
        .unwrap();
        let plan = Spi::get_one::<Json>(
            "EXPLAIN (FORMAT JSON)
             SELECT id FROM lite_union WHERE title ==> 'beer' OR body ==> 'beer'",
        )
        .unwrap()
        .unwrap()
        .0;
        assert_eq!(plan[0]["Plan"]["Plans"][0]["Node Type"], "BitmapOr");
        assert_eq!(
            Spi::get_one::<Vec<i32>>(
                "SELECT array_agg(id ORDER BY id) FROM lite_union
                 WHERE title ==> 'beer' OR body ==> 'beer'"
            )
            .unwrap(),
            Some(vec![1, 2, 3])
        );
    }

    #[pg_test]
    fn heap_mvcc_owns_updates_and_deletes() {
        Spi::run(
            "CREATE TABLE lite_mvcc (id int, body text);
             INSERT INTO lite_mvcc VALUES (1, 'old term'), (2, 'keep term');
             CREATE INDEX lite_mvcc_idx ON lite_mvcc USING tin (body);
             UPDATE lite_mvcc SET body = 'new term' WHERE id = 1;
             DELETE FROM lite_mvcc WHERE id = 2;
             SET LOCAL enable_seqscan = off;",
        )
        .unwrap();
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM lite_mvcc WHERE body ==> 'old'").unwrap(),
            Some(0)
        );
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM lite_mvcc WHERE body ==> 'new'").unwrap(),
            Some(1)
        );
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM lite_mvcc WHERE body ==> 'keep'").unwrap(),
            Some(0)
        );
        Spi::run("UPDATE lite_mvcc SET id = 3 WHERE id = 1").unwrap();
        assert_eq!(
            Spi::get_one::<Vec<i32>>("SELECT array_agg(id) FROM lite_mvcc WHERE body ==> 'new'")
                .unwrap(),
            Some(vec![3])
        );
    }

    #[pg_test]
    fn scoring_rewrite_orders_matching_rows() {
        Spi::run(
            "CREATE TABLE lite_score (id int, body text);
             INSERT INTO lite_score VALUES
               (1, 'rare'), (2, 'rare rare rare'), (3, 'common');
             CREATE INDEX lite_score_idx ON lite_score USING tin (body);",
        )
        .unwrap();
        let ids = Spi::get_one::<Vec<i32>>(
            "SELECT array_agg(id ORDER BY tin.full_score(ctid) DESC, id)
             FROM lite_score WHERE body ==> 'rare'",
        )
        .unwrap();
        assert_eq!(ids, Some(vec![2, 1]));
    }

    #[pg_test]
    fn scoring_helpers_share_the_same_policy() {
        Spi::run(
            "CREATE TABLE lite_score_helpers (id int, body text);
             INSERT INTO lite_score_helpers VALUES
               (1, 'common rare'), (2, 'common'), (3, 'common');
             CREATE INDEX lite_score_helpers_idx ON lite_score_helpers USING tin (body)",
        )
        .unwrap();
        let full_max = Spi::get_one::<f32>(
            "SELECT max(tin.full_score(ctid))
             FROM lite_score_helpers WHERE body ==> 'rare^1.0'",
        )
        .unwrap()
        .unwrap();
        let reported = Spi::get_one::<f32>(
            "SELECT tin.max_score(ctid)
             FROM lite_score_helpers WHERE body ==> 'rare^1.0' LIMIT 1",
        )
        .unwrap()
        .unwrap();
        assert_eq!(reported, full_max);
        let inspected = Spi::get_one::<Vec<String>>(
            "SELECT array_agg(term ORDER BY term)
             FROM tin.score_inspect('lite_score_helpers_idx', 'common OR rare', 0.5)",
        )
        .unwrap();
        assert_eq!(inspected, Some(vec!["rare".to_owned()]));
    }

    #[pg_test]
    fn parameterized_queries_score_like_literals() {
        Spi::run(
            "CREATE TABLE lite_param_score (id int PRIMARY KEY, title text);
             INSERT INTO lite_param_score VALUES (1, 'lorem ipsum'), (2, 'lorem ipsun');
             CREATE INDEX ON lite_param_score USING tin (title);
             PREPARE lite_ranked(text, text) AS
               SELECT tin.score(ctid) FROM lite_param_score
               WHERE title ==> $1 AND title ==> $2 ORDER BY id",
        )
        .unwrap();
        let literal = Spi::get_one::<f32>(
            "SELECT tin.score(ctid) FROM lite_param_score
             WHERE title ==> 'lorem^4'
               AND title ==> '(ipsum^4 OR ipsum~1^1.4 OR ipsum*^2)'
             ORDER BY id",
        )
        .unwrap();
        let prepared = Spi::get_one::<f32>(
            "EXECUTE lite_ranked('lorem^4', '(ipsum^4 OR ipsum~1^1.4 OR ipsum*^2)')",
        )
        .unwrap();
        assert_eq!(prepared, literal);
    }

    #[pg_test]
    fn score_survives_subquery_aggregate() {
        Spi::run(
            "CREATE TABLE lite_sub_score (id int PRIMARY KEY, title text);
             INSERT INTO lite_sub_score VALUES (1, 'lorem ipsum'), (2, 'lorem ipsun');
             CREATE INDEX ON lite_sub_score USING tin (title);",
        )
        .unwrap();
        let direct = Spi::get_one::<f32>(
            "SELECT max(tin.score(ctid)) FROM lite_sub_score WHERE title ==> 'lorem^4'",
        )
        .unwrap();
        let nested = Spi::get_one::<f32>(
            "SELECT max(score) FROM (
               SELECT tin.score(ctid) AS score FROM lite_sub_score WHERE title ==> 'lorem^4'
             ) AS matches",
        )
        .unwrap();
        assert_eq!(nested, direct);
    }

    #[pg_test]
    fn scoring_binds_search_text_from_another_relation_per_row() {
        Spi::run(
            "CREATE TABLE lite_join_docs (id int, body text);
             INSERT INTO lite_join_docs VALUES
               (1, 'red sofa'), (2, 'blue sofa sofa'), (3, 'oak table'),
               (4, 'pine table'), (5, 'glass table'), (6, 'green chair');
             CREATE INDEX ON lite_join_docs USING tin (body);
             CREATE TABLE lite_join_queries (query text);
             INSERT INTO lite_join_queries VALUES ('sofa'), ('red'), ('red OR table');",
        )
        .unwrap();
        let joined = Spi::connect(|client| {
            client
                .select(
                    "SELECT q.query, d.id, tin.full_score(d.ctid)
                     FROM lite_join_queries q JOIN lite_join_docs d ON d.body ==> q.query
                     ORDER BY 1, 2",
                    None,
                    &[],
                )
                .unwrap()
                .map(|row| {
                    (
                        row.get::<String>(1).unwrap().unwrap(),
                        row.get::<i32>(2).unwrap().unwrap(),
                        row.get::<f32>(3).unwrap().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(joined.len(), 7, "{joined:?}");
        for (query, id, score) in &joined {
            let literal = Spi::get_one::<f32>(&format!(
                "SELECT tin.full_score(ctid) FROM lite_join_docs
                 WHERE body ==> '{query}' AND id = {id}"
            ))
            .unwrap();
            assert_eq!(literal, Some(*score), "{query} / {id}");
        }
        let score = |search: &str, id| {
            joined
                .iter()
                .find(|(query, row, _)| query == search && *row == id)
                .unwrap()
                .2
        };
        assert!(score("red", 1) > 0.0);
        assert_ne!(score("red", 1), score("sofa", 1));
    }

    /// Lets the session tests below start the shared test server.
    #[pg_test]
    fn test_server_is_ready() {}

    #[pg_test(error = "tin.score_bound() is missing: the installed tin SQL predates this build")]
    fn scoring_reports_an_outdated_installed_extension() {
        Spi::run(
            "CREATE TABLE lite_stale (title text);
             CREATE INDEX ON lite_stale USING tin (title);
             ALTER FUNCTION tin.score_bound(text, text[], int4, int4, int4,
               float4, float4, float4, text[], text[]) RENAME TO score_bound_stale;",
        )
        .unwrap();
        Spi::run("SELECT tin.score(ctid) FROM lite_stale WHERE title ==> 'lorem'").unwrap();
    }

    #[pg_test]
    fn quals_on_other_relations_do_not_bind_to_the_scored_relation() {
        Spi::run(
            "CREATE TABLE lite_cross_a (id int, title text);
             INSERT INTO lite_cross_a SELECT g, 'filler word number ' || g
               FROM generate_series(1, 30) g;
             UPDATE lite_cross_a SET title = 'alpha zeta filler' WHERE id = 1;
             CREATE INDEX ON lite_cross_a USING tin (title);
             CREATE TABLE lite_cross_b (id int, title text);
             INSERT INTO lite_cross_b SELECT g, 'padding text number ' || g
               FROM generate_series(1, 30) g;
             UPDATE lite_cross_b SET title = 'kappa padding' WHERE id = 1;
             CREATE INDEX ON lite_cross_b USING tin (title);",
        )
        .unwrap();
        let alone = Spi::get_one::<f32>(
            "SELECT tin.score(b.ctid) FROM lite_cross_b b WHERE b.title ==> 'kappa'",
        )
        .unwrap();
        assert!(alone.is_some_and(|score| score > 0.0), "{alone:?}");
        let joined = Spi::get_one::<f32>(
            "SELECT tin.score(b.ctid) FROM lite_cross_a a, lite_cross_b b
             WHERE a.title ==> 'zeta' AND b.title ==> 'kappa'",
        )
        .unwrap();
        assert_eq!(joined, alone);
    }

    #[pg_test]
    fn negated_quals_do_not_contribute_to_scores() {
        Spi::run(
            "CREATE TABLE lite_negated (id int, title text);
             INSERT INTO lite_negated SELECT g, 'filler word number ' || g
               FROM generate_series(1, 30) g;
             UPDATE lite_negated SET title = 'alpha zeta filler' WHERE id = 1;
             UPDATE lite_negated SET title = 'alpha omega omega omega' WHERE id = 2;
             CREATE INDEX ON lite_negated USING tin (title);",
        )
        .unwrap();
        let baseline = Spi::get_two::<f32, f32>(
            "SELECT tin.max_score(ctid), tin.score(ctid) FROM lite_negated
             WHERE title ==> 'zeta'",
        )
        .unwrap();
        let negated = Spi::get_two::<f32, f32>(
            "SELECT tin.max_score(ctid), tin.score(ctid) FROM lite_negated
             WHERE title ==> 'zeta' AND NOT (title ==> 'omega')",
        )
        .unwrap();
        assert_eq!(negated, baseline);
    }

    #[pg_test]
    fn max_score_excludes_nonmatching_documents() {
        for (name, matching, nonmatching, query) in [
            (
                "boolean",
                "beer wine",
                "beer beer beer beer beer",
                "beer^1 AND wine^0",
            ),
            (
                "phrase",
                "beer beer wine",
                "beer noise beer noise beer noise beer noise wine",
                "\"beer beer\"^1 AND wine^0",
            ),
            (
                "positional",
                "beer wine",
                "wine beer beer beer beer beer",
                "beer BEFORE wine^0",
            ),
        ] {
            Spi::run(&format!(
                "CREATE TABLE lite_max_score_{name} (body text);
                 INSERT INTO lite_max_score_{name} VALUES ('{matching}'), ('{nonmatching}');
                 CREATE INDEX ON lite_max_score_{name} USING tin (body);"
            ))
            .unwrap();
            let (score, max) = Spi::get_two::<f32, f32>(&format!(
                "SELECT tin.full_score(ctid), tin.max_score(ctid)
                 FROM lite_max_score_{name} WHERE body ==> '{query}'"
            ))
            .unwrap();
            assert_eq!(max, score, "{name}");
        }
    }

    #[pg_test]
    fn full_score_normalization_matches_tin() {
        Spi::run(
            "CREATE TABLE lite_normalization (id int, body text);
             INSERT INTO lite_normalization VALUES
               (1, 'I love fuji apples and juicy mangoes'),
               (2, 'Grape tasting notes from the orchard'),
               (3, 'The best juicy fuji apple in town');
             CREATE INDEX lite_normalization_idx ON lite_normalization USING tin (body)",
        )
        .unwrap();
        for expression in [
            "tin.full_score(ctid) / tin.max_score(ctid)",
            "1::real / tin.max_score(ctid) * tin.full_score(ctid)",
        ] {
            let sql = format!(
                "SELECT {expression} FROM lite_normalization
                 WHERE body ==> 'apple OR grape' AND tin.max_score(ctid) > 0 ORDER BY id"
            );
            let scores = Spi::connect(|client| {
                client
                    .select(&sql, None, &[])
                    .unwrap()
                    .map(|row| row.get::<f32>(1).unwrap().unwrap())
                    .collect::<Vec<_>>()
            });
            assert_eq!(scores.len(), 2);
            assert!((scores[0] - 1.0).abs() < 0.000001);
            assert!((scores[1] - 0.9398665).abs() < 0.000001);
        }
    }

    #[pg_test]
    fn scoring_binds_to_expression_indexes() {
        Spi::run(
            "CREATE TABLE lite_expression_score (id int, s1 text, s2 text);
             INSERT INTO lite_expression_score VALUES
               (1, 'hello', 'world 10'),
               (2, 'hello hello', 'world 10'),
               (3, 'unrelated', 'document');
             INSERT INTO lite_expression_score
               SELECT n, 'noise', n::text FROM generate_series(4, 30) AS n;
             CREATE INDEX lite_expression_score_idx ON lite_expression_score
               USING tin (((s1 || ' '::text) || s2));",
        )
        .unwrap();
        let rows = Spi::connect(|client| {
            client
                .select(
                    "SELECT id, tin.score(ctid) AS score
                     FROM lite_expression_score
                     WHERE (s1 || ' ' || s2) ==> 'hello world 10'
                     ORDER BY score DESC, id LIMIT 5",
                    None,
                    &[],
                )
                .unwrap()
                .map(|row| {
                    (
                        row.get::<i32>(1).unwrap().unwrap(),
                        row.get::<f32>(2).unwrap().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [2, 1]);
        assert!(rows[0].1 > rows[1].1);
    }

    #[pg_test]
    fn scoring_and_inspection_respect_partial_index_predicates() {
        Spi::run(
            "CREATE TABLE lite_partial_score (id int, body text, active boolean);
             INSERT INTO lite_partial_score VALUES
               (1, 'beer', true), (2, 'wine', true),
               (3, 'wine', NULL), (4, NULL, true);
             INSERT INTO lite_partial_score
               SELECT n, 'wine', false FROM generate_series(5, 104) AS n;
             CREATE INDEX lite_partial_score_idx ON lite_partial_score
               USING tin (body) WHERE active;
             CREATE TABLE lite_partial_score_control AS
               SELECT id, body FROM lite_partial_score WHERE active;
             CREATE INDEX lite_partial_score_control_idx ON lite_partial_score_control
               USING tin (body);",
        )
        .unwrap();
        let partial = Spi::get_one::<f32>(
            "SELECT tin.full_score(ctid) FROM lite_partial_score
             WHERE active AND body ==> 'beer'",
        )
        .unwrap()
        .unwrap();
        let control = Spi::get_one::<f32>(
            "SELECT tin.full_score(ctid) FROM lite_partial_score_control
             WHERE body ==> 'beer'",
        )
        .unwrap()
        .unwrap();
        assert!(control > 0.0);
        assert_eq!(partial, control);

        // In the indexed population, beer occurs in half the documents and
        // must be elided at the default dense ratio, despite the excluded rows.
        assert_eq!(
            Spi::get_one::<i64>(
                "SELECT count(*) FROM tin.score_inspect('lite_partial_score_idx', 'beer')"
            )
            .unwrap(),
            Some(0)
        );
        assert_eq!(
            Spi::get_one::<f32>(
                "SELECT tin.score(ctid) FROM lite_partial_score
                 WHERE active AND body ==> 'beer'"
            )
            .unwrap(),
            Some(0.0)
        );
    }

    #[pg_test]
    fn scoring_respects_partial_expression_index_predicates() {
        Spi::run(
            "CREATE TABLE lite_partial_expression (id int, body text, active boolean);
             INSERT INTO lite_partial_expression VALUES
               (1, 'BEER', true), (2, 'wine wine', true),
               (3, 'BEER BEER', false), (4, 'excluded', false),
               (5, 'excluded', NULL), (6, NULL, true);
             CREATE INDEX lite_partial_expression_idx ON lite_partial_expression
               USING tin (lower(body)) WHERE active OR id = 3;
             CREATE TABLE lite_partial_expression_control AS
               SELECT id, body FROM lite_partial_expression WHERE active OR id = 3;
             CREATE INDEX lite_partial_expression_control_idx
               ON lite_partial_expression_control USING tin (lower(body));",
        )
        .unwrap();
        let partial = Spi::get_one::<Vec<f32>>(
            "SELECT array_agg(tin.full_score(ctid) ORDER BY id)
             FROM lite_partial_expression
             WHERE (active OR id = 3) AND lower(body) ==> 'beer'",
        )
        .unwrap()
        .unwrap();
        let control = Spi::get_one::<Vec<f32>>(
            "SELECT array_agg(tin.full_score(ctid) ORDER BY id)
             FROM lite_partial_expression_control WHERE lower(body) ==> 'beer'",
        )
        .unwrap()
        .unwrap();
        assert_eq!(control.len(), 2);
        assert_eq!(partial, control);
    }

    fn scores_by_id(sql: &str) -> Vec<f32> {
        Spi::get_one::<Vec<f32>>(sql).unwrap().unwrap()
    }

    #[pg_test]
    fn scoring_binds_partial_indexes_the_quals_imply() {
        Spi::run(
            "CREATE TABLE lite_implied (id int, body text, active boolean);
             INSERT INTO lite_implied VALUES
               (1, 'beer wine', true), (2, 'beer', true), (3, 'wine', true);
             INSERT INTO lite_implied
               SELECT n, 'beer', false FROM generate_series(4, 40) AS n;
             CREATE INDEX lite_implied_idx ON lite_implied USING tin (body) WHERE active;
             CREATE TABLE lite_implied_control AS
               SELECT id, body FROM lite_implied WHERE active;
             CREATE INDEX lite_implied_control_idx ON lite_implied_control
               USING tin (body);",
        )
        .unwrap();
        let control = scores_by_id(
            "SELECT array_agg(tin.full_score(ctid) + tin.max_score(ctid) ORDER BY id)
             FROM lite_implied_control WHERE body ==> 'beer'",
        );
        assert_eq!(control.len(), 2);
        for sql in [
            "SELECT array_agg(tin.full_score(ctid) + tin.max_score(ctid) ORDER BY id)
             FROM lite_implied WHERE active = true AND body ==> 'beer'",
            // An outer join's ON clause restricts its nullable side.
            "SELECT array_agg(tin.full_score(t.ctid) + tin.max_score(t.ctid) ORDER BY t.id)
             FROM generate_series(1, 3) AS g(id)
             LEFT JOIN lite_implied t ON t.id = g.id AND t.active AND t.body ==> 'beer'
             WHERE t.id IS NOT NULL",
            "SELECT array_agg(tin.full_score(t.ctid) + tin.max_score(t.ctid) ORDER BY t.id)
             FROM generate_series(1, 3) AS g(id)
             LEFT JOIN lite_implied t ON t.id = g.id
             WHERE t.active AND t.body ==> 'beer'",
        ] {
            assert_eq!(scores_by_id(sql), control, "{sql}");
        }
    }

    #[pg_test]
    fn scoring_skips_partial_indexes_the_quals_do_not_imply() {
        Spi::run(
            "CREATE TABLE lite_twin (id int, body text, active boolean);
             INSERT INTO lite_twin VALUES
               (1, 'beer wine', true), (2, 'beer', true), (3, 'wine', true);
             INSERT INTO lite_twin
               SELECT n, 'beer', false FROM generate_series(4, 40) AS n;
             CREATE INDEX lite_twin_partial_idx ON lite_twin USING tin (body) WHERE active;
             CREATE INDEX lite_twin_full_idx ON lite_twin USING tin (body);
             CREATE TABLE lite_twin_all AS SELECT id, body FROM lite_twin;
             CREATE INDEX lite_twin_all_idx ON lite_twin_all USING tin (body);
             CREATE TABLE lite_twin_active AS SELECT id, body FROM lite_twin WHERE active;
             CREATE INDEX lite_twin_active_idx ON lite_twin_active USING tin (body);",
        )
        .unwrap();
        let full = "array_agg(tin.full_score(ctid) ORDER BY id)";
        let all = scores_by_id(&format!(
            "SELECT {full} FROM lite_twin_all WHERE body ==> 'beer'"
        ));
        let active = scores_by_id(&format!(
            "SELECT {full} FROM lite_twin_active WHERE body ==> 'beer'"
        ));
        assert_ne!(all[..2], active[..]);
        assert_eq!(
            scores_by_id(&format!(
                "SELECT {full} FROM lite_twin WHERE body ==> 'beer'"
            )),
            all
        );
        assert_eq!(
            scores_by_id(&format!(
                "SELECT {full} FROM lite_twin WHERE active AND body ==> 'beer'"
            )),
            active
        );
    }

    #[pg_test]
    fn highlighting_supports_explicit_and_implicit_queries() {
        assert_eq!(
            Spi::get_one::<String>(
                "SELECT tin.highlight('Beer and wine', '[', ']', query => 'beer')"
            )
            .unwrap(),
            Some("[Beer] and wine".into())
        );
        Spi::run(
            "CREATE TABLE lite_highlight (id int, s1 text, s2 text);
             INSERT INTO lite_highlight VALUES
               (1, 'Beer', 'and wine'), (2, 'cider', 'only');
             CREATE INDEX lite_highlight_idx ON lite_highlight
               USING tin (((s1 || ' '::text) || s2));",
        )
        .unwrap();
        assert_eq!(
            Spi::get_one::<String>(
                "SELECT tin.highlight(s1 || ' ' || s2)
                 FROM lite_highlight
                 WHERE (s1 || ' ' || s2) ==> 'beer'"
            )
            .unwrap(),
            Some("<b>Beer</b> and wine".into())
        );
        let ansi = Spi::get_one::<String>(
            "SELECT tin.highlight_ansi(s1 || ' ' || s2)
             FROM lite_highlight
             WHERE (s1 || ' ' || s2) ==> 'beer'",
        )
        .unwrap()
        .unwrap();
        assert!(ansi.contains("\x1b["));
        assert!(ansi.contains("Beer"));
    }
}

/// Scoring checks that span several transactions or sessions, which a
/// `#[pg_test]` cannot express because it runs inside one transaction.
#[cfg(all(test, feature = "pg_test"))]
mod session_tests {
    fn session() -> postgres::Client {
        pgrx_tests::run_test(
            "test_server_is_ready",
            None,
            crate::pg_test::postgresql_conf_options(),
        )
        .unwrap();
        pgrx_tests::client().unwrap().0
    }

    fn scores(client: &mut postgres::Client, sql: &str) -> Vec<(String, f32, f32)> {
        client
            .query(sql, &[])
            .unwrap()
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2)))
            .collect()
    }

    const SOFA: &str = "SELECT body, tin.full_score(ctid), tin.max_score(ctid)
                        FROM {table} WHERE body ==> 'sofa' ORDER BY id";

    fn sofa(client: &mut postgres::Client, table: &str) -> Vec<(String, f32, f32)> {
        scores(client, &SOFA.replace("{table}", table))
    }

    #[test]
    fn successive_autocommit_statements_score_their_own_snapshot() {
        // Each statement makes exactly one scoring call, as in the plain psql
        // session that first showed scores leaking across snapshots.
        fn full_score(client: &mut postgres::Client) -> (String, f32) {
            let row = client
                .query_one(
                    "SELECT body, tin.full_score(ctid) FROM lead_cache_probe
                     WHERE body ==> 'sofa'",
                    &[],
                )
                .unwrap();
            (row.get(0), row.get(1))
        }
        let mut client = session();
        client
            .batch_execute(
                "CREATE TEMP TABLE lead_cache_probe (id integer PRIMARY KEY, body text);
                 CREATE INDEX ON lead_cache_probe USING tin (body);",
            )
            .unwrap();
        client
            .execute("INSERT INTO lead_cache_probe VALUES (1, 'Blue sofa')", &[])
            .unwrap();
        let blue = full_score(&mut client);
        assert!((blue.1 - 0.2876821).abs() < 0.000001, "{blue:?}");

        client
            .execute("UPDATE lead_cache_probe SET body = 'Red sofa'", &[])
            .unwrap();
        assert_eq!(full_score(&mut client), ("Red sofa".into(), blue.1));

        client
            .execute("UPDATE lead_cache_probe SET body = 'sofa sofa sofa'", &[])
            .unwrap();
        let repeated = sofa(&mut client, "lead_cache_probe");
        assert_eq!(repeated.len(), 1);
        assert!(repeated[0].1 > blue.1, "{repeated:?}");
        assert_eq!(repeated[0].2, repeated[0].1);
        assert_eq!(
            full_score(&mut client),
            ("sofa sofa sofa".into(), repeated[0].1)
        );
    }

    #[test]
    fn read_committed_statements_see_concurrent_commits_in_scores() {
        let mut writer = session();
        let mut reader = session();
        writer
            .batch_execute(
                "DROP TABLE IF EXISTS lead_rc_probe;
                 CREATE TABLE lead_rc_probe (id integer PRIMARY KEY, body text);
                 CREATE INDEX ON lead_rc_probe USING tin (body);
                 INSERT INTO lead_rc_probe VALUES (1, 'Blue sofa'), (2, 'green chair');",
            )
            .unwrap();
        reader
            .batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED; SELECT pg_current_xact_id();")
            .unwrap();
        let blue = sofa(&mut reader, "lead_rc_probe");
        assert_eq!(blue.len(), 1);
        assert!(blue[0].1 > 0.0, "{blue:?}");

        writer
            .execute(
                "UPDATE lead_rc_probe SET body = 'Red sofa' WHERE id = 1",
                &[],
            )
            .unwrap();
        assert_eq!(
            sofa(&mut reader, "lead_rc_probe"),
            vec![("Red sofa".into(), blue[0].1, blue[0].1)]
        );
        reader.batch_execute("COMMIT").unwrap();
        writer.batch_execute("DROP TABLE lead_rc_probe").unwrap();
    }

    #[test]
    fn corpus_statistics_use_the_statement_snapshot() {
        let mut writer = session();
        let mut reader = session();
        writer
            .batch_execute(
                "DROP TABLE IF EXISTS lead_cursor_probe;
                 CREATE TABLE lead_cursor_probe (id integer PRIMARY KEY, body text);
                 CREATE INDEX ON lead_cursor_probe USING tin (body);
                 INSERT INTO lead_cursor_probe VALUES (1, 'Blue sofa'), (2, 'green chair');",
            )
            .unwrap();
        let before = sofa(&mut writer, "lead_cursor_probe");
        assert_eq!(before.len(), 1);

        // The cursor's snapshot predates the insert below, but the corpus is
        // only read once the first row is fetched.
        reader
            .batch_execute(&format!(
                "BEGIN ISOLATION LEVEL READ COMMITTED;
                 DECLARE probe CURSOR FOR {};",
                SOFA.replace("{table}", "lead_cursor_probe")
            ))
            .unwrap();
        writer
            .execute(
                "INSERT INTO lead_cursor_probe VALUES (3, 'sofa sofa'), (4, 'leather sofa')",
                &[],
            )
            .unwrap();
        assert_eq!(scores(&mut reader, "FETCH ALL FROM probe"), before);
        reader.batch_execute("COMMIT").unwrap();
        assert_ne!(sofa(&mut writer, "lead_cursor_probe")[0], before[0]);
        writer
            .batch_execute("DROP TABLE lead_cursor_probe")
            .unwrap();
    }

    #[test]
    fn unproved_partial_indexes_refuse_scoring_like_tin() {
        let mut client = session();
        client
            .batch_execute(
                "CREATE TEMP TABLE lead_unproved (id integer, body text, active boolean);
                 INSERT INTO lead_unproved VALUES (1, 'beer', true), (2, 'beer', false);
                 CREATE INDEX ON lead_unproved USING tin (body) WHERE active;
                 CREATE INDEX ON lead_unproved USING tin (lower(body)) WHERE active;",
            )
            .unwrap();
        for (sql, expression) in [
            (
                "SELECT tin.score(ctid) FROM lead_unproved WHERE body ==> 'beer'",
                "lead_unproved.body",
            ),
            (
                "SELECT tin.full_score(t.ctid) FROM lead_unproved t
                 WHERE lower(t.body) ==> 'beer' AND lower(t.body) ==> 'wine'",
                "lower(lead_unproved.body)",
            ),
            (
                "SELECT tin.max_score(ctid) FROM lead_unproved
                 WHERE body ==> 'beer' AND active IS NOT NULL",
                "lead_unproved.body",
            ),
            // An outer join's ON clause does not restrict its preserved side.
            (
                "SELECT tin.score(t.ctid) FROM lead_unproved t
                 LEFT JOIN generate_series(1, 2) AS g(id) ON g.id = t.id AND t.active
                 WHERE t.body ==> 'beer'",
                "lead_unproved.body",
            ),
        ] {
            let error = client.query(sql, &[]).unwrap_err();
            let error = error
                .as_db_error()
                .unwrap_or_else(|| panic!("{sql}: {error}"));
            assert_eq!(
                (
                    error.code().code(),
                    error.message(),
                    error.detail(),
                    error.hint()
                ),
                (
                    "0A000",
                    "cannot compute scores for this query",
                    Some(format!("No matching tin index for: {expression}.").as_str()),
                    Some(
                        "Add a matching USING tin index, or check the definition of an \
                         existing index."
                    ),
                ),
                "{sql}"
            );
        }
    }
}
