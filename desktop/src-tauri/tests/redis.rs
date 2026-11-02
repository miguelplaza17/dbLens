//! Redis driver against a throwaway server. Skips when DBLENS_TEST_REDIS_URL is
//! unset. Every test uses its own key prefix and deletes its keys at the end.

use dblens_lib::drivers::redis;
use dblens_lib::session::{
    CellValue, ColumnSort, DatabaseSession, DbError, Relation, RelationKind, RowKey, RowKeyKind,
};

async fn open() -> Option<Box<dyn DatabaseSession>> {
    let url = std::env::var("DBLENS_TEST_REDIS_URL").ok()?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    Some(redis::connect(&url, None).await.expect("connect"))
}

fn cell(column: &str, value: Option<&str>) -> CellValue {
    CellValue {
        column: column.into(),
        value: value.map(String::from),
    }
}

fn key(columns: &[&str], values: &[&str]) -> RowKey {
    RowKey {
        columns: columns.iter().map(|c| c.to_string()).collect(),
        kind: RowKeyKind::PrimaryKey,
        values: values.iter().map(|v| Some(v.to_string())).collect(),
    }
}

fn rel(name: &str, subtype: &str) -> Relation {
    let schema = name
        .split(':')
        .next()
        .filter(|_| name.contains(':'))
        .unwrap_or("")
        .to_string();
    Relation {
        schema,
        name: name.into(),
        kind: RelationKind::Key,
        estimate: -1.0,
        subtype: subtype.into(),
    }
}

async fn run(s: &dyn DatabaseSession, text: &str) {
    s.execute(text)
        .await
        .unwrap_or_else(|e| panic!("{text}: {e}"));
}

async fn clean(s: &dyn DatabaseSession, prefix: &str) {
    let found = s.relations().await.unwrap();
    for r in found.iter().filter(|r| r.name.starts_with(prefix)) {
        run(s, &format!("DEL {}", r.name)).await;
    }
}

fn texts(rows: &[Vec<Option<String>>], column: usize) -> Vec<String> {
    rows.iter()
        .map(|r| r[column].clone().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn connects_and_reports_version() {
    let Some(s) = open().await else { return };
    assert_eq!(s.engine().name(), "Redis");
    assert!(!s.server_version().is_empty());
    let r = s.execute("PING").await.unwrap();
    assert_eq!(r.results[0].rows[0][0].as_deref(), Some("PONG"));
}

#[tokio::test]
async fn bad_password_and_bad_host_say_why() {
    let Some(url) = std::env::var("DBLENS_TEST_REDIS_URL").ok() else {
        return;
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let err = redis::connect("redis://127.0.0.1:1", None)
        .await
        .err()
        .expect("refused");
    assert!(
        err.message().contains("Couldn't connect to 127.0.0.1:1"),
        "{err}"
    );
    let err = redis::connect("redis+sentinel://h", None)
        .await
        .err()
        .expect("sentinel");
    assert!(err.message().contains("Sentinel"));
    // Through a "tunnel" port that is closed.
    let err = redis::connect(&url, Some(1))
        .await
        .err()
        .expect("tunnel port");
    assert!(err.message().contains("127.0.0.1:1"), "{err}");
}

#[tokio::test]
async fn lists_keys_by_prefix_and_type() {
    let Some(s) = open().await else { return };
    let p = "dbl_rel";
    clean(&*s, p).await;
    run(&*s, &format!("SET {p}:a:1 x\nHSET {p}:h f v\nRPUSH {p}:l a b\nSADD {p}:s m\nZADD {p}:z 1 m\nSET {p}plain 1")).await;
    let all = s.relations().await.unwrap();
    let mine: Vec<&Relation> = all.iter().filter(|r| r.name.starts_with(p)).collect();
    assert_eq!(mine.len(), 6);
    assert!(mine
        .iter()
        .all(|r| r.kind == RelationKind::Key && r.estimate == -1.0));
    let find = |n: &str| mine.iter().find(|r| r.name == n).unwrap();
    assert_eq!(find(&format!("{p}:a:1")).schema, p);
    assert_eq!(find(&format!("{p}:a:1")).subtype, "string");
    assert_eq!(find(&format!("{p}:h")).subtype, "hash");
    assert_eq!(find(&format!("{p}:l")).subtype, "list");
    assert_eq!(find(&format!("{p}:s")).subtype, "set");
    assert_eq!(find(&format!("{p}:z")).subtype, "zset");
    assert_eq!(find(&format!("{p}plain")).schema, "");
    // sorted by group, then name
    let order: Vec<(&str, &str)> = all
        .iter()
        .map(|r| (r.schema.as_str(), r.name.as_str()))
        .collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);
    clean(&*s, p).await;
    assert!(s
        .relations()
        .await
        .unwrap()
        .iter()
        .all(|r| !r.name.starts_with(p)));
}

#[tokio::test]
async fn pages_each_type() {
    let Some(s) = open().await else { return };
    let p = "dbl_page";
    clean(&*s, p).await;
    run(
        &*s,
        &format!("SET {p}:str \"hello world\"\nEXPIRE {p}:str 3600\nSET {p}:nottl x"),
    )
    .await;
    run(&*s, &format!("HSET {p}:h b 2 a 1 c 3\nRPUSH {p}:l x y z w\nSADD {p}:s q p r\nZADD {p}:z 2 two 1 one 3 three")).await;
    run(&*s, &format!("XADD {p}:x 1-1 f v g w\nXADD {p}:x 2-1 f v2")).await;

    let str_page = s
        .page(&rel(&format!("{p}:str"), "string"), 100, 0, "", None)
        .await
        .unwrap();
    assert_eq!(str_page.rows, vec![vec![Some("hello world".to_string())]]);
    assert_eq!(str_page.keys.as_ref().unwrap().columns, ["key"]);
    assert!(
        str_page
            .note
            .as_deref()
            .unwrap()
            .starts_with("string · TTL "),
        "{:?}",
        str_page.note
    );
    let nottl = s
        .page(&rel(&format!("{p}:nottl"), "string"), 100, 0, "", None)
        .await
        .unwrap();
    assert_eq!(nottl.note.as_deref(), Some("string · no expiry"));

    let hash = s
        .page(&rel(&format!("{p}:h"), "hash"), 100, 0, "", None)
        .await
        .unwrap();
    assert_eq!(
        hash.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["field", "value"]
    );
    assert_eq!(texts(&hash.rows, 0), ["a", "b", "c"]);
    assert_eq!(
        hash.keys.as_ref().unwrap().values[1],
        vec![Some("b".to_string())]
    );
    let hash2 = s
        .page(&rel(&format!("{p}:h"), "hash"), 2, 1, "", None)
        .await
        .unwrap();
    assert_eq!(texts(&hash2.rows, 0), ["b", "c"]);

    let list = s
        .page(&rel(&format!("{p}:l"), "list"), 2, 1, "", None)
        .await
        .unwrap();
    assert_eq!(list.columns[0].type_name, "integer");
    assert_eq!(texts(&list.rows, 0), ["1", "2"]);
    assert_eq!(texts(&list.rows, 1), ["y", "z"]);
    assert_eq!(
        list.keys.as_ref().unwrap().values[0],
        vec![Some("1".to_string())]
    );

    let set = s
        .page(&rel(&format!("{p}:s"), "set"), 100, 0, "", None)
        .await
        .unwrap();
    assert_eq!(texts(&set.rows, 0), ["p", "q", "r"]);

    let zset = s
        .page(&rel(&format!("{p}:z"), "zset"), 100, 0, "", None)
        .await
        .unwrap();
    assert_eq!(zset.columns[1].type_name, "double");
    assert_eq!(texts(&zset.rows, 0), ["one", "two", "three"]);
    assert_eq!(texts(&zset.rows, 1), ["1", "2", "3"]);

    let stream = s
        .page(&rel(&format!("{p}:x"), "stream"), 100, 0, "", None)
        .await
        .unwrap();
    assert_eq!(
        stream.rows[0],
        vec![Some("1-1".to_string()), Some("f=v, g=w".to_string())]
    );
    assert!(stream.keys.is_none());
    let stream2 = s
        .page(&rel(&format!("{p}:x"), "stream"), 1, 1, "", None)
        .await
        .unwrap();
    assert_eq!(texts(&stream2.rows, 0), ["2-1"]);

    let gone = s
        .page(&rel(&format!("{p}:missing"), "string"), 10, 0, "", None)
        .await
        .unwrap_err();
    assert!(gone.message().contains("no longer exists"));
    clean(&*s, p).await;
}

#[tokio::test]
async fn searches_sorts_and_counts() {
    let Some(s) = open().await else { return };
    let p = "dbl_search";
    clean(&*s, p).await;
    run(
        &*s,
        &format!("HSET {p}:h apple 10 Banana 9 cherry Red2 date 100"),
    )
    .await;
    run(&*s, &format!("RPUSH {p}:l one Two three")).await;
    let h = rel(&format!("{p}:h"), "hash");
    let l = rel(&format!("{p}:l"), "list");

    let hit = s.page(&h, 100, 0, "BAN", None).await.unwrap();
    assert_eq!(texts(&hit.rows, 0), ["Banana"]);
    let in_value = s.page(&h, 100, 0, "red", None).await.unwrap();
    assert_eq!(texts(&in_value.rows, 0), ["cherry"]);
    assert!(s
        .page(&h, 100, 0, "zzz", None)
        .await
        .unwrap()
        .rows
        .is_empty());

    let by_value = ColumnSort {
        column: "value".into(),
        ascending: true,
    };
    let sorted = s.page(&h, 100, 0, "", Some(&by_value)).await.unwrap();
    assert_eq!(texts(&sorted.rows, 1), ["9", "10", "100", "Red2"]);
    let desc = ColumnSort {
        column: "value".into(),
        ascending: false,
    };
    let sorted = s.page(&h, 2, 0, "", Some(&desc)).await.unwrap();
    assert_eq!(texts(&sorted.rows, 1), ["Red2", "100"]);
    // keys follow the filtered rows
    assert_eq!(
        sorted.keys.unwrap().values[0],
        vec![Some("cherry".to_string())]
    );
    let paged = s.page(&h, 2, 2, "", Some(&by_value)).await.unwrap();
    assert_eq!(texts(&paged.rows, 1), ["100", "Red2"]);

    assert_eq!(s.count(&h, "").await.unwrap(), Some(4));
    assert_eq!(s.count(&h, "an").await.unwrap(), Some(1));
    assert_eq!(s.count(&l, "").await.unwrap(), Some(3));
    assert_eq!(s.count(&l, "T").await.unwrap(), Some(2));
    run(
        &*s,
        &format!("SET {p}:str v\nSADD {p}:s a b\nZADD {p}:z 1 a\nXADD {p}:x * f v"),
    )
    .await;
    assert_eq!(
        s.count(&rel(&format!("{p}:str"), "string"), "")
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        s.count(&rel(&format!("{p}:s"), "set"), "").await.unwrap(),
        Some(2)
    );
    assert_eq!(
        s.count(&rel(&format!("{p}:z"), "zset"), "").await.unwrap(),
        Some(1)
    );
    assert_eq!(
        s.count(&rel(&format!("{p}:x"), "stream"), "")
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        s.count(&rel(&format!("{p}:nokey"), "?"), "").await.unwrap(),
        None
    );
    clean(&*s, p).await;
}

#[tokio::test]
async fn edits_one_row() {
    let Some(s) = open().await else { return };
    let p = "dbl_edit";
    clean(&*s, p).await;
    run(&*s, &format!("SET {p}:str old\nEXPIRE {p}:str 1000\nHSET {p}:h f1 v1 f2 v2\nRPUSH {p}:l a b c\nSADD {p}:s m1 m2\nZADD {p}:z 1 a 2 b")).await;

    // string: value changes, TTL stays; NULL is refused
    let st = rel(&format!("{p}:str"), "string");
    s.page(&st, 10, 0, "", None).await.unwrap();
    s.update(
        &st,
        &key(&["key"], &[&st.name]),
        &[cell("value", Some("new"))],
    )
    .await
    .unwrap();
    let after = s.page(&st, 10, 0, "", None).await.unwrap();
    assert_eq!(after.rows[0][0].as_deref(), Some("new"));
    assert!(after.note.unwrap().contains("TTL"));
    let e = s
        .update(&st, &key(&["key"], &[&st.name]), &[cell("value", None)])
        .await
        .unwrap_err();
    assert!(e.message().contains("no NULL"));
    let e = s
        .update(
            &st,
            &key(&["key"], &[&st.name]),
            &[cell("other", Some("x"))],
        )
        .await
        .unwrap_err();
    assert!(e.message().contains("can't be changed"));
    assert!(
        s.update(&st, &key(&["key"], &[&st.name]), &[])
            .await
            .unwrap()
            .affected
            == 0
    );

    // hash: value, rename, clash
    let h = rel(&format!("{p}:h"), "hash");
    s.page(&h, 10, 0, "", None).await.unwrap();
    let o = s
        .update(&h, &key(&["field"], &["f1"]), &[cell("value", Some("V1"))])
        .await
        .unwrap();
    assert_eq!((o.command.as_str(), o.affected), ("HSET", 1));
    s.update(&h, &key(&["field"], &["f1"]), &[cell("field", Some("g1"))])
        .await
        .unwrap();
    let page = s.page(&h, 10, 0, "", None).await.unwrap();
    assert_eq!(texts(&page.rows, 0), ["f2", "g1"]);
    assert_eq!(texts(&page.rows, 1), ["v2", "V1"]);
    let e = s
        .update(&h, &key(&["field"], &["f2"]), &[cell("field", Some("g1"))])
        .await
        .unwrap_err();
    assert!(e.message().contains("already there"));
    let e = s
        .update(&h, &key(&["field"], &["gone"]), &[cell("value", Some("x"))])
        .await
        .unwrap_err();
    assert!(e.message().contains("changed or was deleted"));

    // list: by index, checked against what was shown
    let l = rel(&format!("{p}:l"), "list");
    s.page(&l, 10, 0, "", None).await.unwrap();
    s.update(&l, &key(&["index"], &["1"]), &[cell("value", Some("B"))])
        .await
        .unwrap();
    assert_eq!(
        texts(&s.page(&l, 10, 0, "", None).await.unwrap().rows, 1),
        ["a", "B", "c"]
    );
    run(&*s, &format!("LSET {p}:l 2 changed")).await;
    let e = s
        .update(&l, &key(&["index"], &["2"]), &[cell("value", Some("x"))])
        .await
        .unwrap_err();
    assert!(e.message().contains("changed or was deleted"), "{e}");

    // set: rename member
    let set = rel(&format!("{p}:s"), "set");
    s.page(&set, 10, 0, "", None).await.unwrap();
    s.update(
        &set,
        &key(&["member"], &["m1"]),
        &[cell("member", Some("m3"))],
    )
    .await
    .unwrap();
    assert_eq!(
        texts(&s.page(&set, 10, 0, "", None).await.unwrap().rows, 0),
        ["m2", "m3"]
    );
    let e = s
        .update(
            &set,
            &key(&["member"], &["m2"]),
            &[cell("member", Some("m3"))],
        )
        .await
        .unwrap_err();
    assert!(e.message().contains("already there"));

    // zset: score, rename keeping the score
    let z = rel(&format!("{p}:z"), "zset");
    s.page(&z, 10, 0, "", None).await.unwrap();
    s.update(&z, &key(&["member"], &["a"]), &[cell("score", Some("5"))])
        .await
        .unwrap();
    s.update(&z, &key(&["member"], &["b"]), &[cell("member", Some("bb"))])
        .await
        .unwrap();
    let page = s.page(&z, 10, 0, "", None).await.unwrap();
    assert_eq!(texts(&page.rows, 0), ["bb", "a"]);
    assert_eq!(texts(&page.rows, 1), ["2", "5"]);
    clean(&*s, p).await;
}

#[tokio::test]
async fn inserts_and_deletes_rows() {
    let Some(s) = open().await else { return };
    let p = "dbl_ins";
    clean(&*s, p).await;
    run(&*s, &format!("HSET {p}:h f v\nRPUSH {p}:l a b c\nSADD {p}:s m\nZADD {p}:z 1 a\nSET {p}:str v\nXADD {p}:x * f v")).await;
    let h = rel(&format!("{p}:h"), "hash");
    let l = rel(&format!("{p}:l"), "list");
    let set = rel(&format!("{p}:s"), "set");
    let z = rel(&format!("{p}:z"), "zset");

    s.insert(&h, &[cell("field", Some("g")), cell("value", Some("w"))])
        .await
        .unwrap();
    let e = s
        .insert(&h, &[cell("field", Some("g")), cell("value", Some("w"))])
        .await
        .unwrap_err();
    assert!(e.message().contains("already there"));
    assert!(s
        .insert(&h, &[cell("value", Some("w"))])
        .await
        .unwrap_err()
        .message()
        .contains("Enter a field"));
    s.insert(&l, &[cell("value", Some("d"))]).await.unwrap();
    s.insert(&set, &[cell("member", Some("n"))]).await.unwrap();
    assert!(s.insert(&set, &[cell("member", Some("n"))]).await.is_err());
    s.insert(&z, &[cell("member", Some("b"))]).await.unwrap();
    s.insert(&z, &[cell("member", Some("c")), cell("score", Some("7"))])
        .await
        .unwrap();
    assert!(s
        .insert(
            &rel(&format!("{p}:str"), "string"),
            &[cell("value", Some("x"))]
        )
        .await
        .unwrap_err()
        .message()
        .contains("holds one value"));
    assert!(s
        .insert(&rel(&format!("{p}:x"), "stream"), &[])
        .await
        .unwrap_err()
        .message()
        .contains("command panel"));
    assert!(s
        .insert(&rel(&format!("{p}:none"), "hash"), &[])
        .await
        .is_err());

    assert_eq!(
        texts(&s.page(&h, 10, 0, "", None).await.unwrap().rows, 0),
        ["f", "g"]
    );
    assert_eq!(
        texts(&s.page(&l, 10, 0, "", None).await.unwrap().rows, 1),
        ["a", "b", "c", "d"]
    );
    let zp = s.page(&z, 10, 0, "", None).await.unwrap();
    assert_eq!(texts(&zp.rows, 0), ["b", "a", "c"]);
    assert_eq!(texts(&zp.rows, 1), ["0", "1", "7"]);

    // deletes need the page that showed the row
    s.delete(&h, &key(&["field"], &["g"])).await.unwrap();
    assert_eq!(
        texts(&s.page(&h, 10, 0, "", None).await.unwrap().rows, 0),
        ["f"]
    );
    let e = s.delete(&h, &key(&["field"], &["g"])).await.unwrap_err();
    assert!(e.message().contains("changed or was deleted"));
    s.page(&set, 10, 0, "", None).await.unwrap();
    s.delete(&set, &key(&["member"], &["n"])).await.unwrap();
    s.delete(&z, &key(&["member"], &["a"])).await.unwrap();
    s.page(&l, 10, 0, "", None).await.unwrap();
    s.delete(&l, &key(&["index"], &["1"])).await.unwrap();
    assert_eq!(
        texts(&s.page(&l, 10, 0, "", None).await.unwrap().rows, 1),
        ["a", "c", "d"]
    );
    // the list moved under us: the shown value no longer matches
    run(&*s, &format!("LSET {p}:l 0 other")).await;
    let e = s.delete(&l, &key(&["index"], &["0"])).await.unwrap_err();
    assert!(e.message().contains("changed or was deleted"));
    assert_eq!(
        texts(&s.page(&l, 10, 0, "", None).await.unwrap().rows, 1),
        ["other", "c", "d"]
    );
    let e = s
        .delete(&rel(&format!("{p}:str"), "string"), &key(&["key"], &["x"]))
        .await
        .unwrap_err();
    assert!(e.message().contains("DEL"));
    clean(&*s, p).await;
}

#[tokio::test]
async fn refuses_to_edit_bytes_that_are_not_text() {
    let Some(s) = open().await else { return };
    let p = "dbl_bin";
    clean(&*s, p).await;
    run(
        &*s,
        &format!("HSET {p}:h f \"\\xff\\xfe\"\nSET {p}:s \"\\xff\""),
    )
    .await;
    let h = rel(&format!("{p}:h"), "hash");
    let page = s.page(&h, 10, 0, "", None).await.unwrap();
    assert_eq!(page.rows[0][1].as_deref(), Some("0xFFFE"));
    let e = s
        .update(&h, &key(&["field"], &["f"]), &[cell("value", Some("x"))])
        .await
        .unwrap_err();
    assert!(e.message().contains("aren't text"), "{e}");
    assert!(s.delete(&h, &key(&["field"], &["f"])).await.is_err());
    clean(&*s, p).await;
}

#[tokio::test]
async fn runs_commands_as_typed() {
    let Some(s) = open().await else { return };
    let p = "dbl_exec";
    clean(&*s, p).await;
    let r = s
        .execute(&format!("# comment\nSET {p}:k \"a b\\n\"\n\nGET {p}:k\nHSET {p}:h a 1 b 2\nHGETALL {p}:h\nRPUSH {p}:l x y\nLRANGE {p}:l 0 -1"))
        .await
        .unwrap();
    assert_eq!(r.results.len(), 6);
    assert_eq!(r.results[1].rows[0][0].as_deref(), Some("a b\n"));
    assert_eq!(
        r.results[3]
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["field", "value"]
    );
    assert_eq!(r.results[3].command, "HGETALL 2");
    assert_eq!(texts(&r.results[5].rows, 0), ["x", "y"]);
    assert!(r.duration_ms >= 0.0);
    // errors stop the run with the server's text
    let e = s.execute("GET a b c").await.unwrap_err();
    assert!(
        e.message()
            .to_lowercase()
            .contains("wrong number of arguments"),
        "{e}"
    );
    let e = s.execute("NOSUCHCOMMAND").await.unwrap_err();
    assert!(
        e.message().to_lowercase().contains("unknown command"),
        "{e}"
    );
    assert!(s
        .execute("SET 'oops")
        .await
        .unwrap_err()
        .message()
        .contains("Unbalanced"));
    assert!(s
        .execute("MONITOR")
        .await
        .unwrap_err()
        .message()
        .contains("never returns"));
    // the connection still works afterwards
    assert_eq!(
        s.execute("PING").await.unwrap().results[0].rows[0][0].as_deref(),
        Some("PONG")
    );
    clean(&*s, p).await;
}

#[tokio::test]
async fn select_changes_the_database_for_later_calls() {
    let Some(s) = open().await else { return };
    let p = "dbl_sel";
    clean(&*s, p).await;
    run(&*s, &format!("SET {p}:a 1")).await;
    assert!(s
        .relations()
        .await
        .unwrap()
        .iter()
        .any(|r| r.name == format!("{p}:a")));
    run(&*s, "SELECT 14").await;
    assert!(s
        .relations()
        .await
        .unwrap()
        .iter()
        .all(|r| r.name != format!("{p}:a")));
    run(&*s, "SELECT 15").await;
    assert!(s
        .relations()
        .await
        .unwrap()
        .iter()
        .any(|r| r.name == format!("{p}:a")));
    clean(&*s, p).await;
}

#[tokio::test]
async fn read_only_allows_only_reads() {
    let Some(s) = open().await else { return };
    let p = "dbl_ro";
    clean(&*s, p).await;
    run(&*s, &format!("SET {p}:k v")).await;
    let ok = s.read_only(&format!("GET {p}:k")).await.unwrap();
    assert_eq!(ok.results[0].rows[0][0].as_deref(), Some("v"));
    let e = s
        .read_only(&format!("SET {p}:k changed"))
        .await
        .unwrap_err();
    assert!(e.message().contains("SET can change data"));
    assert!(s.read_only("FLUSHDB").await.is_err());
    assert!(s.read_only("EVAL \"return 1\" 0").await.is_err());
    assert!(s
        .read_only("PING\nPING")
        .await
        .unwrap_err()
        .message()
        .contains("one command"));
    assert_eq!(
        s.execute(&format!("GET {p}:k")).await.unwrap().results[0].rows[0][0].as_deref(),
        Some("v")
    );
    clean(&*s, p).await;
}

#[tokio::test]
async fn rehearse_refuses_and_atomic_is_not_atomic() {
    let Some(s) = open().await else { return };
    let p = "dbl_atom";
    clean(&*s, p).await;
    let e = s.rehearse(&format!("SET {p}:k 1")).await.unwrap_err();
    assert!(e.message().contains("can't try a statement and undo it"));
    let e = s
        .execute_atomically(&format!("SET {p}:k 1"))
        .await
        .unwrap_err();
    assert!(matches!(e, DbError::NotAtomic(_)), "{e:?}");
    // nothing ran
    assert!(s
        .relations()
        .await
        .unwrap()
        .iter()
        .all(|r| r.name != format!("{p}:k")));
    assert!(s.schema_graph().await.unwrap().tables.is_empty());
    assert_eq!(s.quote("a b"), "a b");
    assert_eq!(s.qualified(&rel("u:1", "string")), "u:1");
}

#[tokio::test]
async fn cancel_stops_a_running_command_and_the_session_recovers() {
    let Some(s) = open().await else { return };
    let s: std::sync::Arc<dyn DatabaseSession> = s.into();
    let waiter = s.clone();
    // BLPOP on a missing list blocks for 30 s on the server.
    let task = tokio::spawn(async move { waiter.execute("BLPOP dbl_cancel:never 30").await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    s.cancel();
    let e = task.await.unwrap().unwrap_err();
    assert!(matches!(e, DbError::Cancelled), "{e:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    // cancel with nothing running does nothing; the next call reconnects
    s.cancel();
    assert_eq!(
        s.execute("PING").await.unwrap().results[0].rows[0][0].as_deref(),
        Some("PONG")
    );
}

#[tokio::test]
async fn closes_and_reconnects_lazily() {
    let Some(s) = open().await else { return };
    s.close().await;
    assert_eq!(
        s.execute("PING").await.unwrap().results[0].rows[0][0].as_deref(),
        Some("PONG")
    );
}
