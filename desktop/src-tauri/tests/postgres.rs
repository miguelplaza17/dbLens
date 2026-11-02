//! Postgres driver against a throwaway server. Skips (passes) when
//! DBLENS_TEST_URL is unset. Every test makes and drops its own tables.

use std::sync::Arc;
use std::time::Duration;

use dblens_lib::drivers::postgres;
use dblens_lib::session::{
    CellValue, ColumnSort, DatabaseSession, DbError, QueryResult, Relation, RelationKind,
    RowKeyKind,
};
use dblens_lib::url::ConnectionUrl;

fn test_url() -> Option<String> {
    std::env::var("DBLENS_TEST_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

async fn open(url: &str) -> Box<dyn DatabaseSession> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    postgres::connect(url, None).await.expect("connect")
}

/// A session, or None (skip) without a test server.
async fn session() -> Option<Box<dyn DatabaseSession>> {
    Some(open(&test_url()?).await)
}

fn table(name: &str) -> Relation {
    Relation::new("public", name, RelationKind::Table, -1.0)
}

fn cell(result: &QueryResult, row: usize, column: usize) -> Option<&str> {
    result.rows[row][column].as_deref()
}

fn change(column: &str, value: Option<&str>) -> CellValue {
    CellValue {
        column: column.into(),
        value: value.map(String::from),
    }
}

async fn drop_tables(s: &dyn DatabaseSession, names: &[&str]) {
    for name in names {
        s.execute(&format!("drop table if exists public.\"{name}\" cascade; drop view if exists public.\"{name}\" cascade"))
            .await
            .ok();
    }
}

#[tokio::test]
async fn connects_and_prints_values_as_libpq_does() {
    let Some(s) = session().await else { return };
    assert!(
        s.server_version().contains('.'),
        "version {}",
        s.server_version()
    );
    let run = s
        .execute(
            "select 1 as n, null::text as nothing, 'héllo' as t, true as yes, false as no, 1.50::numeric as price, \
             now()::date = current_date as today, array[1,2,3] as list, '{\"a\": [1, 2]}'::jsonb as doc, \
             '\\xdeadbeef'::bytea as raw, interval '1 day 2 hours' as span, \
             '3f2504e0-4f89-11d3-9a0c-0305e82c3301'::uuid as id, timestamp '2024-03-05 07:08:09.5' as ts, 0.1::float8 as f",
        )
        .await
        .expect("select");
    let r = &run.results[0];
    assert_eq!(r.command, "SELECT 1");
    let names: Vec<_> = r.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names[..3], ["n", "nothing", "t"]);
    let row: Vec<_> = r.rows[0].iter().map(|v| v.as_deref()).collect();
    assert_eq!(
        row,
        vec![
            Some("1"),
            None,
            Some("héllo"),
            Some("t"),
            Some("f"),
            Some("1.50"),
            Some("t"),
            Some("{1,2,3}"),
            Some("{\"a\": [1, 2]}"),
            Some("\\xdeadbeef"),
            Some("1 day 02:00:00"),
            Some("3f2504e0-4f89-11d3-9a0c-0305e82c3301"),
            Some("2024-03-05 07:08:09.5"),
            Some("0.1"),
        ]
    );
    assert!(run.duration_ms >= 0.0);
}

#[tokio::test]
async fn executes_several_statements_with_their_tags() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_multi"]).await;
    let run = s
        .execute(
            "create table public.dbl_pg_multi(a int); insert into public.dbl_pg_multi values (1),(2),(3); \
             update public.dbl_pg_multi set a = a + 1 where a > 1; select a from public.dbl_pg_multi order by a; \
             delete from public.dbl_pg_multi where a = 1; drop table public.dbl_pg_multi",
        )
        .await
        .expect("script");
    let tags: Vec<_> = run.results.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(
        tags,
        [
            "CREATE TABLE",
            "INSERT 0 3",
            "UPDATE 2",
            "SELECT 3",
            "DELETE 1",
            "DROP TABLE"
        ]
    );
    assert_eq!(run.results[2].total_rows, 2);
    assert_eq!(run.results[3].rows.len(), 3);
    assert_eq!(cell(&run.results[3], 2, 0), Some("4"));
    assert_eq!(
        s.execute("select 1 where false").await.unwrap().results[0]
            .columns
            .len(),
        1,
        "empty results keep columns"
    );
}

#[tokio::test]
async fn reports_server_errors_and_stays_usable() {
    let Some(s) = session().await else { return };
    let error = s
        .execute("select * from dbl_pg_does_not_exist")
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(text.starts_with("ERROR:"), "{text}");
    assert!(text.contains("dbl_pg_does_not_exist"), "{text}");
    let error = s.execute("select 1/0").await.unwrap_err();
    assert!(error.to_string().contains("division by zero"));
    assert_eq!(
        cell(&s.execute("select 7").await.unwrap().results[0], 0, 0),
        Some("7")
    );
}

#[tokio::test]
async fn cuts_big_results() {
    let Some(s) = session().await else { return };
    let run = s
        .execute("select g from generate_series(1, 6000) g")
        .await
        .unwrap();
    let r = &run.results[0];
    assert_eq!(r.rows.len(), 5000);
    assert!(r.truncated);
    assert_eq!(r.total_rows, 6000);
    let small = s
        .execute("select g from generate_series(1, 5000) g")
        .await
        .unwrap();
    assert!(!small.results[0].truncated);
}

#[tokio::test]
async fn lists_relations_of_every_kind() {
    let Some(s) = session().await else { return };
    drop_tables(
        &*s,
        &[
            "dbl_pg_rel_t",
            "dbl_pg_rel_v",
            "dbl_pg_rel_m",
            "dbl_pg_rel_p",
        ],
    )
    .await;
    s.execute(
        "create table public.dbl_pg_rel_t(a int primary key); create view public.dbl_pg_rel_v as select a from public.dbl_pg_rel_t; \
         create materialized view public.dbl_pg_rel_m as select a from public.dbl_pg_rel_t; \
         create table public.dbl_pg_rel_p(a int) partition by range (a)",
    )
    .await
    .unwrap();
    let all = s.relations().await.unwrap();
    let kind = |name: &str| {
        all.iter()
            .find(|r| r.schema == "public" && r.name == name)
            .map(|r| r.kind)
    };
    assert_eq!(kind("dbl_pg_rel_t"), Some(RelationKind::Table));
    assert_eq!(kind("dbl_pg_rel_v"), Some(RelationKind::View));
    assert_eq!(kind("dbl_pg_rel_m"), Some(RelationKind::Materialized));
    assert_eq!(kind("dbl_pg_rel_p"), Some(RelationKind::Partitioned));
    assert!(all
        .iter()
        .all(|r| r.schema != "pg_catalog" && r.schema != "information_schema"));
    // public comes first.
    assert_eq!(all[0].schema, "public");
    assert_eq!(s.qualified(&table("we\"ird")), "\"public\".\"we\"\"ird\"");
    drop_tables(
        &*s,
        &[
            "dbl_pg_rel_t",
            "dbl_pg_rel_v",
            "dbl_pg_rel_m",
            "dbl_pg_rel_p",
        ],
    )
    .await;
}

#[tokio::test]
async fn pages_sort_search_and_count() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_page"]).await;
    s.execute(
        "create table public.dbl_pg_page(id int primary key, name text, qty int); \
         insert into public.dbl_pg_page values (3, 'Cherry', 5), (1, 'apple', null), (2, 'Banana', 5), (4, '50% off_sale', 1), (5, 'APPLE pie', 9)",
    )
    .await
    .unwrap();
    let t = table("dbl_pg_page");

    let first = s.page(&t, 100, 0, "", None).await.unwrap();
    assert_eq!(
        first
            .rows
            .iter()
            .map(|r| r[0].clone().unwrap())
            .collect::<Vec<_>>(),
        ["1", "2", "3", "4", "5"]
    );
    let types: Vec<_> = first.columns.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(types, ["integer", "text", "integer"]);
    let keys = first.keys.as_ref().expect("keys");
    assert_eq!(keys.kind, RowKeyKind::PrimaryKey);
    assert_eq!(keys.columns, ["id"]);
    assert_eq!(keys.values[2], vec![Some("3".to_string())]);

    // Paging stays in primary-key order.
    let second = s.page(&t, 2, 2, "", None).await.unwrap();
    assert_eq!(
        second
            .rows
            .iter()
            .map(|r| r[0].clone().unwrap())
            .collect::<Vec<_>>(),
        ["3", "4"]
    );

    // Sort by a column, then the key; NULL last in both directions.
    let by_qty = s
        .page(
            &t,
            100,
            0,
            "",
            Some(&ColumnSort {
                column: "qty".into(),
                ascending: true,
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        by_qty
            .rows
            .iter()
            .map(|r| r[0].clone().unwrap())
            .collect::<Vec<_>>(),
        ["4", "2", "3", "5", "1"]
    );
    let by_qty_desc = s
        .page(
            &t,
            100,
            0,
            "",
            Some(&ColumnSort {
                column: "qty".into(),
                ascending: false,
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        by_qty_desc
            .rows
            .iter()
            .map(|r| r[0].clone().unwrap())
            .collect::<Vec<_>>(),
        ["5", "2", "3", "4", "1"]
    );
    // The keys follow the sorted rows.
    assert_eq!(by_qty.keys.unwrap().values[0], vec![Some("4".to_string())]);

    // Search: any column, case-insensitive, wildcards are literal.
    let apple = s.page(&t, 100, 0, "APPLE", None).await.unwrap();
    assert_eq!(apple.rows.len(), 2);
    assert_eq!(s.page(&t, 100, 0, "50%", None).await.unwrap().rows.len(), 1);
    assert_eq!(
        s.page(&t, 100, 0, "off_s", None).await.unwrap().rows.len(),
        1
    );
    assert_eq!(
        s.page(&t, 100, 0, "%", None).await.unwrap().rows.len(),
        1,
        "a lone % is not a wildcard"
    );
    assert_eq!(
        s.page(&t, 100, 0, "o'brien", None)
            .await
            .unwrap()
            .rows
            .len(),
        0
    );
    assert_eq!(
        s.page(&t, 100, 0, "5", None).await.unwrap().rows.len(),
        4,
        "searches numbers too"
    );

    // Count.
    assert_eq!(s.count(&t, "").await.unwrap(), Some(5));
    assert_eq!(s.count(&t, "apple").await.unwrap(), Some(2));
    let huge = Relation::new("public", "dbl_pg_page", RelationKind::Table, 600_000.0);
    assert_eq!(
        s.count(&huge, "").await.unwrap(),
        None,
        "big tables are not counted"
    );
    drop_tables(&*s, &["dbl_pg_page"]).await;
}

#[tokio::test]
async fn composite_keys_and_quoted_names() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_Odd Name"]).await;
    s.execute("create table public.\"dbl_pg_Odd Name\"(\"a b\" int, \"c\"\"d\" text, primary key (\"c\"\"d\", \"a b\"))").await.unwrap();
    s.execute("insert into public.\"dbl_pg_Odd Name\" values (2, 'x'), (1, 'x'), (1, 'a')")
        .await
        .unwrap();
    let t = table("dbl_pg_Odd Name");
    let page = s.page(&t, 10, 0, "", None).await.unwrap();
    let keys = page.keys.expect("keys");
    assert_eq!(keys.columns, ["c\"d", "a b"], "key order, not column order");
    assert_eq!(
        keys.values[0],
        vec![Some("a".to_string()), Some("1".to_string())]
    );
    let key = keys.row(2).unwrap();
    let out = s
        .update(&t, &key, &[change("a b", Some("20"))])
        .await
        .unwrap();
    assert_eq!(out.affected, 1);
    assert_eq!(out.command, "UPDATE 1");
    assert!(out.sql.contains("\"a b\" = $1"), "{}", out.sql);
    assert_eq!(s.count(&t, "20").await.unwrap(), Some(1));
    drop_tables(&*s, &["dbl_pg_Odd Name"]).await;
}

#[tokio::test]
async fn edits_one_row_with_casts_and_nulls() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_edit"]).await;
    s.execute(
        "create table public.dbl_pg_edit(id serial primary key, n int, at timestamp, doc jsonb, note text default 'dflt', flag boolean); \
         insert into public.dbl_pg_edit(n) values (1), (2)",
    )
    .await
    .unwrap();
    let t = table("dbl_pg_edit");
    let page = s.page(&t, 10, 0, "", None).await.unwrap();
    let keys = page.keys.unwrap();

    let out = s
        .update(
            &t,
            &keys.row(0).unwrap(),
            &[
                change("n", Some("42")),
                change("at", Some("2024-03-05 07:08:09")),
                change("doc", Some("{\"k\": 1}")),
                change("flag", Some("true")),
                change("note", None),
            ],
        )
        .await
        .unwrap();
    assert_eq!((out.affected, out.command.as_str()), (1, "UPDATE 1"));
    let row = s.page(&t, 10, 0, "", None).await.unwrap();
    assert_eq!(
        row.rows[0].iter().map(|v| v.as_deref()).collect::<Vec<_>>(),
        [
            Some("1"),
            Some("42"),
            Some("2024-03-05 07:08:09"),
            Some("{\"k\": 1}"),
            None,
            Some("t")
        ]
    );
    assert_eq!(cell(&row, 1, 1), Some("2"), "the other row is untouched");

    // Setting NULL.
    s.update(&t, &keys.row(0).unwrap(), &[change("n", None)])
        .await
        .unwrap();
    assert_eq!(
        cell(&s.page(&t, 10, 0, "", None).await.unwrap(), 0, 1),
        None
    );

    // A value the column can't hold is the server's error, and nothing changes.
    let error = s
        .update(
            &t,
            &keys.row(1).unwrap(),
            &[change("n", Some("not a number"))],
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("integer"), "{error}");
    assert_eq!(
        cell(&s.page(&t, 10, 0, "", None).await.unwrap(), 1, 1),
        Some("2")
    );
    assert!(s
        .update(&t, &keys.row(1).unwrap(), &[])
        .await
        .unwrap()
        .sql
        .is_empty());

    // Insert: with values, defaults, and nothing at all.
    let out = s
        .insert(
            &t,
            &[change("n", Some("7")), change("note", Some("it's \\ fine"))],
        )
        .await
        .unwrap();
    assert_eq!((out.affected, out.command.as_str()), (1, "INSERT 0 1"));
    s.insert(&t, &[]).await.unwrap();
    let all = s.page(&t, 10, 0, "", None).await.unwrap();
    assert_eq!(all.rows.len(), 4);
    assert_eq!(cell(&all, 2, 4), Some("it's \\ fine"));
    assert_eq!(cell(&all, 3, 4), Some("dflt"));

    // Delete exactly one.
    let out = s
        .delete(&t, &all.keys.unwrap().row(3).unwrap())
        .await
        .unwrap();
    assert_eq!((out.affected, out.command.as_str()), (1, "DELETE 1"));
    assert_eq!(s.count(&t, "").await.unwrap(), Some(3));
    drop_tables(&*s, &["dbl_pg_edit"]).await;
}

#[tokio::test]
async fn refuses_edits_that_touch_the_wrong_number_of_rows() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_wrong"]).await;
    s.execute("create table public.dbl_pg_wrong(grp int, v int primary key); insert into public.dbl_pg_wrong values (1, 1), (1, 2), (2, 3)").await.unwrap();
    let t = table("dbl_pg_wrong");
    let key = |column: &str, value: &str| dblens_lib::session::RowKey {
        columns: vec![column.into()],
        kind: RowKeyKind::PrimaryKey,
        values: vec![Some(value.into())],
    };

    // Two rows would change: rolled back.
    let error = s
        .update(&t, &key("grp", "1"), &[change("grp", Some("9"))])
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("affected 2 rows and was rolled back"),
        "{error}"
    );
    let error = s.delete(&t, &key("grp", "1")).await.unwrap_err();
    assert!(error.to_string().contains("affected 2 rows"), "{error}");
    assert_eq!(s.count(&t, "").await.unwrap(), Some(3));
    assert_eq!(s.count(&t, "9").await.unwrap(), Some(0));

    // No row: the row is gone.
    let error = s.delete(&t, &key("v", "99")).await.unwrap_err();
    assert!(
        error.to_string().contains("changed or was deleted"),
        "{error}"
    );

    // A failed edit leaves no open transaction behind.
    let error = s
        .update(&t, &key("v", "1"), &[change("v", Some("2"))])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("duplicate key"), "{error}");
    assert_eq!(s.count(&t, "").await.unwrap(), Some(3));
    s.delete(&t, &key("v", "3")).await.unwrap();
    assert_eq!(
        s.execute("select txid_current_if_assigned() is null")
            .await
            .unwrap()
            .results[0]
            .rows[0][0]
            .as_deref(),
        Some("t")
    );
    drop_tables(&*s, &["dbl_pg_wrong"]).await;
}

#[tokio::test]
async fn tables_without_a_primary_key_use_ctid_checked_by_xmin() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_nokey"]).await;
    s.execute("create table public.dbl_pg_nokey(a int, b text); insert into public.dbl_pg_nokey values (1, 'x'), (1, 'x'), (2, 'y')").await.unwrap();
    let t = table("dbl_pg_nokey");
    let page = s.page(&t, 10, 0, "", None).await.unwrap();
    assert_eq!(
        page.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"],
        "ctid and xmin are not columns"
    );
    assert_eq!(page.rows[0].len(), 2);
    let keys = page.keys.clone().expect("keys");
    assert_eq!(keys.kind, RowKeyKind::Ctid);
    assert_eq!(keys.columns, ["ctid", "xmin"]);
    assert!(keys.values[0][0].as_deref().unwrap().starts_with("(0,"));

    // Identical rows are told apart.
    let out = s
        .update(&t, &keys.row(1).unwrap(), &[change("b", Some("changed"))])
        .await
        .unwrap();
    assert_eq!(out.affected, 1);
    let after = s.page(&t, 10, 0, "", None).await.unwrap();
    assert_eq!(
        after
            .rows
            .iter()
            .filter(|r| r[1].as_deref() == Some("changed"))
            .count(),
        1
    );

    // The old key is stale now (the update gave the row a new ctid and xmin).
    let error = s
        .update(&t, &keys.row(1).unwrap(), &[change("b", Some("again"))])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("changed or was deleted"),
        "{error}"
    );
    // A row nobody touched still works.
    s.delete(&t, &keys.row(2).unwrap()).await.unwrap();
    assert_eq!(s.count(&t, "").await.unwrap(), Some(2));
    drop_tables(&*s, &["dbl_pg_nokey"]).await;
}

#[tokio::test]
async fn views_page_without_keys() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_vt", "dbl_pg_vv"]).await;
    s.execute("create table public.dbl_pg_vt(a int primary key); insert into public.dbl_pg_vt values (1); create view public.dbl_pg_vv as select a, a * 2 as double from public.dbl_pg_vt").await.unwrap();
    let v = Relation::new("public", "dbl_pg_vv", RelationKind::View, -1.0);
    let page = s.page(&v, 10, 0, "", None).await.unwrap();
    assert!(page.keys.is_none());
    assert_eq!(
        page.rows,
        vec![vec![Some("1".to_string()), Some("2".to_string())]]
    );
    assert_eq!(page.columns[1].type_name, "integer");
    assert_eq!(s.count(&v, "").await.unwrap(), Some(1));
    drop_tables(&*s, &["dbl_pg_vv", "dbl_pg_vt"]).await;
}

#[tokio::test]
async fn read_only_lets_reads_through_and_nothing_else() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_ro"]).await;
    s.execute("create table public.dbl_pg_ro(a int); insert into public.dbl_pg_ro values (1)")
        .await
        .unwrap();
    let run = s
        .read_only("select a from public.dbl_pg_ro;")
        .await
        .unwrap();
    assert_eq!(cell(&run.results[0], 0, 0), Some("1"));

    for sql in [
        "insert into public.dbl_pg_ro values (2)",
        "delete from public.dbl_pg_ro",
        "drop table public.dbl_pg_ro",
        "create table public.dbl_pg_ro2(a int)",
        "select nextval('pg_class_oid_index'::regclass)",
    ] {
        assert!(s.read_only(sql).await.is_err(), "{sql} should be refused");
    }
    // Several statements are refused, so a COMMIT can't open the door.
    let error = s
        .read_only("commit; delete from public.dbl_pg_ro")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("multiple commands"), "{error}");
    assert!(s
        .read_only("set transaction read write; delete from public.dbl_pg_ro")
        .await
        .is_err());
    assert_eq!(s.count(&table("dbl_pg_ro"), "").await.unwrap(), Some(1));
    // And the session is back to normal afterwards.
    s.execute("insert into public.dbl_pg_ro values (3)")
        .await
        .unwrap();
    assert_eq!(s.count(&table("dbl_pg_ro"), "").await.unwrap(), Some(2));
    drop_tables(&*s, &["dbl_pg_ro"]).await;
}

#[tokio::test]
async fn rehearsals_are_always_undone() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_reh"]).await;
    s.execute(
        "create table public.dbl_pg_reh(a int); insert into public.dbl_pg_reh values (1), (2)",
    )
    .await
    .unwrap();

    let run = s
        .rehearse("delete from public.dbl_pg_reh where a = 1; create table public.dbl_pg_reh_new(x int); select count(*) from public.dbl_pg_reh")
        .await
        .unwrap();
    let tags: Vec<_> = run.results.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(tags, ["DELETE 1", "CREATE TABLE", "SELECT 1"]);
    assert_eq!(
        cell(&run.results[2], 0, 0),
        Some("1"),
        "the script saw its own delete"
    );
    assert_eq!(
        s.count(&table("dbl_pg_reh"), "").await.unwrap(),
        Some(2),
        "nothing was kept"
    );
    assert!(s
        .relations()
        .await
        .unwrap()
        .iter()
        .all(|r| r.name != "dbl_pg_reh_new"));

    // A failing statement is named, and the transaction is still undone.
    let error = s
        .rehearse("delete from public.dbl_pg_reh; select * from nowhere_at_all")
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(
        text.starts_with("Statement 2 of 2 failed (select * from nowhere_at_all)"),
        "{text}"
    );
    assert_eq!(s.count(&table("dbl_pg_reh"), "").await.unwrap(), Some(2));

    // Transaction control is refused: a COMMIT would make the test permanent.
    assert!(s
        .rehearse("delete from public.dbl_pg_reh; commit")
        .await
        .is_err());
    assert_eq!(s.count(&table("dbl_pg_reh"), "").await.unwrap(), Some(2));
    drop_tables(&*s, &["dbl_pg_reh"]).await;
}

#[tokio::test]
async fn atomic_runs_are_all_or_nothing() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_atom"]).await;
    s.execute("create table public.dbl_pg_atom(a int primary key)")
        .await
        .unwrap();

    let error = s
        .execute_atomically("insert into public.dbl_pg_atom values (1); insert into public.dbl_pg_atom values (2); insert into public.dbl_pg_atom values (1)")
        .await
        .unwrap_err();
    assert!(
        error.to_string().starts_with("Statement 3 of 3 failed"),
        "{error}"
    );
    assert!(error.to_string().contains("duplicate key"), "{error}");
    assert_eq!(
        s.count(&table("dbl_pg_atom"), "").await.unwrap(),
        Some(0),
        "the first two were rolled back"
    );

    let run = s.execute_atomically("insert into public.dbl_pg_atom values (1), (2); update public.dbl_pg_atom set a = a + 10").await.unwrap();
    assert_eq!(run.results.len(), 2);
    assert_eq!(run.results[1].command, "UPDATE 2");
    assert_eq!(s.count(&table("dbl_pg_atom"), "").await.unwrap(), Some(2));

    // DDL is transactional here too.
    assert!(s
        .execute_atomically("create table public.dbl_pg_atom_b(x int); select 1/0")
        .await
        .is_err());
    assert!(s
        .relations()
        .await
        .unwrap()
        .iter()
        .all(|r| r.name != "dbl_pg_atom_b"));
    assert!(matches!(
        s.execute_atomically("begin; select 1").await,
        Err(DbError::Message(_))
    ));
    drop_tables(&*s, &["dbl_pg_atom"]).await;
}

#[tokio::test]
async fn draws_the_schema_graph_with_foreign_keys() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_orders", "dbl_pg_customers", "dbl_pg_gv"]).await;
    s.execute(
        "create table public.dbl_pg_customers(id int primary key, name text not null); \
         create table public.dbl_pg_orders(id int primary key, customer_id int, region text, \
           constraint dbl_pg_orders_customer_fk foreign key (customer_id) references public.dbl_pg_customers(id)); \
         create view public.dbl_pg_gv as select id from public.dbl_pg_orders",
    )
    .await
    .unwrap();
    let graph = s.schema_graph().await.unwrap();
    let orders = graph
        .tables
        .iter()
        .find(|t| t.name == "dbl_pg_orders")
        .expect("orders");
    assert!(!orders.is_view);
    assert_eq!(orders.schema, "public");
    let names: Vec<_> = orders.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "customer_id", "region"]);
    assert!(orders.columns[0].is_primary_key && !orders.columns[1].is_primary_key);
    assert!(orders.columns[1].is_nullable && !orders.columns[0].is_nullable);
    assert_eq!(orders.columns[1].type_name, "integer");
    let customers = graph
        .tables
        .iter()
        .find(|t| t.name == "dbl_pg_customers")
        .unwrap();
    assert!(!customers.columns[1].is_nullable);
    assert!(
        graph
            .tables
            .iter()
            .find(|t| t.name == "dbl_pg_gv")
            .unwrap()
            .is_view
    );
    let link = graph
        .links
        .iter()
        .find(|l| l.name == "dbl_pg_orders_customer_fk")
        .expect("link");
    assert_eq!(link.from, "public.dbl_pg_orders");
    assert_eq!(link.to, "public.dbl_pg_customers");
    assert_eq!(link.from_columns, ["customer_id"]);
    assert_eq!(link.to_columns, ["id"]);
    drop_tables(&*s, &["dbl_pg_gv", "dbl_pg_orders", "dbl_pg_customers"]).await;
}

#[tokio::test]
async fn cancel_stops_a_running_query() {
    let Some(url) = test_url() else { return };
    let s: Arc<dyn DatabaseSession> = Arc::from(open(&url).await);
    let runner = {
        let s = s.clone();
        tokio::spawn(async move { s.execute("select pg_sleep(30)").await })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    let started = std::time::Instant::now();
    s.cancel();
    let result = tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .expect("cancel took too long")
        .unwrap();
    assert_eq!(result.unwrap_err(), DbError::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
    // The session works after a cancel, and a later statement_timeout is not mistaken for one.
    assert_eq!(
        cell(&s.execute("select 1").await.unwrap().results[0], 0, 0),
        Some("1")
    );
    s.execute("set statement_timeout = 200").await.unwrap();
    let error = s.execute("select pg_sleep(5)").await.unwrap_err();
    assert!(
        matches!(&error, DbError::Message(m) if m.contains("statement timeout")),
        "{error:?}"
    );
    s.execute("set statement_timeout = 0").await.unwrap();
}

#[tokio::test]
async fn cancel_inside_a_rehearsal_leaves_nothing_behind() {
    let Some(url) = test_url() else { return };
    let s: Arc<dyn DatabaseSession> = Arc::from(open(&url).await);
    drop_tables(&*s, &["dbl_pg_cr"]).await;
    s.execute("create table public.dbl_pg_cr(a int); insert into public.dbl_pg_cr values (1)")
        .await
        .unwrap();
    let runner = {
        let s = s.clone();
        tokio::spawn(async move {
            s.rehearse("delete from public.dbl_pg_cr; select pg_sleep(30)")
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    s.cancel();
    let result = tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .expect("timeout")
        .unwrap();
    let error = result.unwrap_err();
    assert!(matches!(error, DbError::Cancelled), "{error:?}");
    assert_eq!(s.count(&table("dbl_pg_cr"), "").await.unwrap(), Some(1));
    drop_tables(&*s, &["dbl_pg_cr"]).await;
}

#[tokio::test]
async fn operations_queue_instead_of_interleaving() {
    let Some(url) = test_url() else { return };
    let s: Arc<dyn DatabaseSession> = Arc::from(open(&url).await);
    drop_tables(&*s, &["dbl_pg_q"]).await;
    s.execute("create table public.dbl_pg_q(a int)")
        .await
        .unwrap();
    // Many atomic scripts at once on one session: each is its own transaction, none sees a half-done one.
    let mut jobs = Vec::new();
    for i in 0..8 {
        let s = s.clone();
        jobs.push(tokio::spawn(async move {
            s.execute_atomically(&format!("insert into public.dbl_pg_q values ({i}); insert into public.dbl_pg_q values ({i})")).await
        }));
    }
    for job in jobs {
        job.await.unwrap().unwrap();
    }
    assert_eq!(s.count(&table("dbl_pg_q"), "").await.unwrap(), Some(16));
    drop_tables(&*s, &["dbl_pg_q"]).await;
}

#[tokio::test]
async fn connection_errors_carry_the_servers_text() {
    let Some(url) = test_url() else { return };
    let parts = ConnectionUrl::new(&url).unwrap();
    let user = parts.user().unwrap_or("postgres").to_string();
    let host = parts.host();
    let port = parts.port().unwrap_or(5432);
    let db = parts.database();

    let wrong_password = format!("postgres://{user}:definitely-wrong@{host}:{port}/{db}");
    let message = postgres::connect(&wrong_password, None)
        .await
        .err()
        .expect("must fail")
        .to_string();
    assert!(
        message.contains("authentication failed") || message.contains("password"),
        "{message}"
    );

    let wrong_db = format!(
        "postgres://{user}:{}@{host}:{port}/dbl_no_such_database",
        parts.password().unwrap_or("")
    );
    let message = postgres::connect(&wrong_db, None)
        .await
        .err()
        .expect("must fail")
        .to_string();
    assert!(message.contains("does not exist"), "{message}");

    let closed = format!("postgres://{user}@127.0.0.1:1/{db}?connect_timeout=3");
    let message = postgres::connect(&closed, None)
        .await
        .err()
        .expect("must fail")
        .to_string();
    assert!(message.to_lowercase().contains("refused"), "{message}");
}

#[tokio::test]
async fn tls_modes_follow_sslmode() {
    let Some(url) = test_url() else { return };
    let parts = ConnectionUrl::new(&url).unwrap();
    let base = format!(
        "postgres://{}:{}@{}:{}/{}",
        parts.user().unwrap_or("postgres"),
        parts.password().unwrap_or(""),
        parts.host(),
        parts.port().unwrap_or(5432),
        parts.database()
    );
    let _ = rustls::crypto::ring::default_provider().install_default();
    let ssl_on = open(&url).await.execute("show ssl").await.unwrap().results[0].rows[0][0].clone()
        == Some("on".into());

    // Plain always works; prefer works either way.
    let plain = open(&format!("{base}?sslmode=disable")).await;
    assert_eq!(
        cell(
            &plain
                .execute("select ssl from pg_stat_ssl where pid = pg_backend_pid()")
                .await
                .unwrap()
                .results[0],
            0,
            0
        ),
        Some("f")
    );
    open(&format!("{base}?sslmode=prefer")).await;

    if ssl_on {
        // require encrypts without checking the certificate.
        let secure = open(&format!("{base}?sslmode=require")).await;
        assert_eq!(
            cell(
                &secure
                    .execute("select ssl from pg_stat_ssl where pid = pg_backend_pid()")
                    .await
                    .unwrap()
                    .results[0],
                0,
                0
            ),
            Some("t")
        );
        // A throwaway server's self-signed certificate fails verification.
        let error = postgres::connect(&format!("{base}?sslmode=verify-full"), None)
            .await
            .err()
            .expect("must not verify");
        assert!(
            error.to_string().to_lowercase().contains("certificate")
                || error.to_string().to_lowercase().contains("tls"),
            "{error}"
        );
    } else {
        let error = postgres::connect(&format!("{base}?sslmode=require"), None)
            .await
            .err()
            .expect("no tls offered");
        assert!(error.to_string().to_lowercase().contains("tls"), "{error}");
    }
    assert!(
        postgres::connect(&format!("{base}?sslmode=sometimes"), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn connects_through_a_local_port_for_ssh_tunnels() {
    let Some(url) = test_url() else { return };
    let parts = ConnectionUrl::new(&url).unwrap();
    let target = format!("{}:{}", parts.host(), parts.port().unwrap_or(5432));
    // A stand-in for the tunnel: a local port that forwards to the real server.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                return;
            };
            let target = target.clone();
            tokio::spawn(async move {
                if let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    // The URL's host does not resolve: only the local port can be used.
    let through = format!(
        "postgres://{}:{}@db.unreachable.invalid:5432/{}",
        parts.user().unwrap_or("postgres"),
        parts.password().unwrap_or(""),
        parts.database()
    );
    let _ = rustls::crypto::ring::default_provider().install_default();
    let s = postgres::connect(&through, Some(local))
        .await
        .expect("connect through the local port");
    assert_eq!(
        cell(
            &s.execute("select 'tunnel'").await.unwrap().results[0],
            0,
            0
        ),
        Some("tunnel")
    );
    s.close().await;
}

#[tokio::test]
async fn grid_edits_inside_a_user_transaction_leave_the_commit_to_the_user() {
    let Some(s) = session().await else { return };
    drop_tables(&*s, &["dbl_pg_tx"]).await;
    s.execute("create table public.dbl_pg_tx(id int primary key, v text); insert into public.dbl_pg_tx values (1, 'a')")
        .await
        .unwrap();
    s.execute("begin; insert into public.dbl_pg_tx values (2, 'user-pending')")
        .await
        .unwrap();
    let page = s.page(&table("dbl_pg_tx"), 50, 0, "", None).await.unwrap();
    let keys = page.keys.clone().unwrap();
    let first = dblens_lib::session::RowKey {
        columns: keys.columns.clone(),
        kind: keys.kind,
        values: keys.values[0].clone(),
    };
    s.update(&table("dbl_pg_tx"), &first, &[change("v", Some("edited"))])
        .await
        .unwrap();
    // A refused edit is undone alone; the user's transaction stays usable.
    let gone = dblens_lib::session::RowKey {
        values: vec![Some("99".into())],
        ..first.clone()
    };
    assert!(s.delete(&table("dbl_pg_tx"), &gone).await.is_err());
    // Scripts that need their own transaction are refused instead of undoing the user's work.
    assert!(s.rehearse("select 1").await.is_err());
    assert!(s.read_only("select 1").await.is_err());
    s.execute("rollback").await.unwrap();
    let after = s.page(&table("dbl_pg_tx"), 50, 0, "", None).await.unwrap();
    assert_eq!(
        after.rows.len(),
        1,
        "the user's pending row was rolled back"
    );
    assert_eq!(
        cell(&after, 0, 1),
        Some("a"),
        "the edit was part of the user's transaction"
    );
    drop_tables(&*s, &["dbl_pg_tx"]).await;
}

#[tokio::test]
async fn a_failed_transaction_is_rolled_back_so_the_session_keeps_working() {
    let Some(s) = session().await else { return };
    let error = s.execute("begin; select 1/0").await.unwrap_err();
    assert!(error.to_string().contains("rolled back"), "{error}");
    assert_eq!(
        cell(&s.execute("select 1").await.unwrap().results[0], 0, 0),
        Some("1")
    );
    assert!(s.relations().await.is_ok());
}

#[tokio::test]
async fn huge_results_stream_and_keep_only_the_limit() {
    let Some(s) = session().await else { return };
    let run = s
        .execute("select g from generate_series(1, 200000) g")
        .await
        .unwrap();
    let r = &run.results[0];
    assert_eq!(r.rows.len(), dblens_lib::session::ROW_LIMIT);
    assert_eq!(r.total_rows, 200_000);
    assert!(r.truncated);
}

#[tokio::test]
async fn a_late_cancel_does_not_reach_the_next_statement() {
    let Some(s) = session().await else { return };
    // Nothing is running: the cancel is dropped, and the next statement runs.
    s.cancel();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let run = s.execute("select pg_sleep(0.5), 1").await.unwrap();
    assert_eq!(cell(&run.results[0], 0, 1), Some("1"));
}
