//! MongoDB driver against a throwaway server. Skips (passes) when
//! DBLENS_TEST_MONGO_URL is unset; the URL's database (e.g.
//! mongodb://root:pw@127.0.0.1:57017/shop?authSource=admin) is where the tests
//! work. Every test makes and drops its own collections. Port of the server
//! tests in Tests/DBLensKitTests/MongoTests.swift.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dblens_lib::drivers::mongo::{self, MongoSession};
use dblens_lib::session::{
    CellValue, ColumnSort, DatabaseSession, DbError, Relation, RelationKind, RowKeyKind, ROW_LIMIT,
};

fn test_url() -> Option<String> {
    std::env::var("DBLENS_TEST_MONGO_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

async fn open(url: &str) -> MongoSession {
    let _ = rustls::crypto::ring::default_provider().install_default();
    mongo::open(url, None).await.expect("connect")
}

/// A session, or None (skip) without a test server.
async fn session() -> Option<MongoSession> {
    Some(open(&test_url()?).await)
}

/// The URL's database, where the tests' collections go.
fn database() -> String {
    mongo::summary(&test_url().unwrap())
        .map(|s| s.database)
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "test".into())
}

/// A collection name only this test uses, so tests can run side by side.
fn collection_name(name: &str) -> String {
    format!(
        "dblens_{name}_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    )
}

async fn collection(session: &MongoSession, name: &str) -> Relation {
    session
        .relations()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no {name}"))
}

async fn run(session: &MongoSession, text: &str) -> dblens_lib::session::Execution {
    session
        .execute(text)
        .await
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn rows(execution: &dblens_lib::session::Execution) -> Vec<Vec<Option<String>>> {
    execution.main().map(|r| r.rows.clone()).unwrap_or_default()
}

fn s(text: &str) -> Option<String> {
    Some(text.to_string())
}

fn cell(column: &str, value: Option<&str>) -> CellValue {
    CellValue {
        column: column.into(),
        value: value.map(String::from),
    }
}

#[tokio::test]
async fn connects() {
    let Some(session) = session().await else {
        return;
    };
    assert_eq!(session.engine(), dblens_lib::engine::Engine::Mongodb);
    assert_eq!(session.quote("my things"), "my things");
    assert!(
        session
            .server_version()
            .starts_with(|c: char| c.is_ascii_digit()),
        "{}",
        session.server_version()
    );
    println!("server: {}", session.server_version());
    assert_eq!(session.current_database().await, database());
    let ping = run(&session, "db.runCommand({ping: 1})").await;
    let main = ping.main().unwrap();
    assert_eq!(
        main.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["ok"]
    );
    assert!(
        main.rows == [[s("1.0")]] || main.rows == [[s("1")]],
        "{:?}",
        main.rows
    );

    // A wrong password: the server's words.
    let url = test_url().unwrap();
    let at = url
        .rfind('@')
        .expect("the test URL has a user and password");
    let colon = url[..at].rfind(':').unwrap();
    let wrong = format!("{}:wrong{}", &url[..colon], &url[at..]);
    let error = mongo::open(&wrong, None)
        .await
        .err()
        .expect("connected with a wrong password")
        .message();
    assert!(error.contains("Authentication failed"), "{error}");
    let closed = mongo::open("mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=500", None).await;
    assert!(!closed
        .err()
        .expect("connected to a closed port")
        .message()
        .is_empty());
}

#[tokio::test]
async fn lists_collections_and_views() {
    let Some(session) = session().await else {
        return;
    };
    let name = collection_name("rel");
    run(
        &session,
        &format!(
            "db.{name}.insertMany([{{a: 1}}, {{a: 2}}, {{a: 3}}])\n\
             db.runCommand({{create: \"{name}_view\", viewOn: \"{name}\", pipeline: [{{$match: {{a: {{$gt: 1}}}}}}]}})"
        ),
    )
    .await;
    let relations = session.relations().await.unwrap();
    let table = relations.iter().find(|r| r.name == name).unwrap();
    assert_eq!(
        (
            table.schema.as_str(),
            table.kind,
            table.subtype.as_str(),
            table.estimate
        ),
        (database().as_str(), RelationKind::Table, "collection", 3.0)
    );
    let view = relations
        .iter()
        .find(|r| r.name == format!("{name}_view"))
        .unwrap();
    assert_eq!(
        (view.kind, view.subtype.as_str(), view.estimate),
        (RelationKind::View, "view", -1.0)
    );
    assert!(!relations.iter().any(|r| r.name.starts_with("system.")));
    assert_eq!(session.count(table, "").await.unwrap(), Some(3));
    assert_eq!(session.count(view, "").await.unwrap(), Some(2));
    let view_page = session.page(view, 10, 0, "", None).await.unwrap();
    assert_eq!(view_page.rows.len(), 2);
    assert!(view_page.keys.is_none());
    // Too big to count quickly.
    let huge = Relation {
        estimate: 600_000.0,
        ..table.clone()
    };
    assert_eq!(session.count(&huge, "").await.unwrap(), None);

    // Without a database in the URL: every database but admin, local and config.
    let url = test_url().unwrap();
    let without = url.replacen(&format!("/{}", database()), "/", 1);
    let all = open(&without).await;
    let everywhere = all.relations().await.unwrap();
    assert!(everywhere
        .iter()
        .any(|r| r.schema == database() && r.name == name));
    assert!(!everywhere
        .iter()
        .any(|r| ["admin", "local", "config"].contains(&r.schema.as_str())));
    run(
        &session,
        &format!("db.{name}_view.drop(); db.{name}.drop()"),
    )
    .await;
}

async fn seed(session: &MongoSession, name: &str) {
    run(
        session,
        &format!(
            r#"db.{name}.insertMany([
  {{_id: ObjectId("650000000000000000000002"), name: "Bia", score: 1.5, big: NumberLong("9007199254740993"),
   price: NumberDecimal("9.99"), code: "42", note: null}},
  {{_id: ObjectId("650000000000000000000001"), name: "Ana", age: 30, tags: ["a", "b"], address: {{city: "Lisbon", zip: "1000"}},
   joined: ISODate("2024-05-01T10:30:00.250Z"), active: true}},
  {{_id: ObjectId("650000000000000000000003"), age: NumberInt(7), bin: BinData(0, "AQID"), note: "here"}}
])"#
        ),
    )
    .await;
}

#[tokio::test]
async fn searches_fields_and_ids() {
    let Some(session) = session().await else {
        return;
    };
    let name = collection_name("search");
    seed(&session, &name).await;
    let relation = collection(&session, &name).await;
    let ana = session.page(&relation, 10, 0, "ana", None).await.unwrap();
    assert_eq!(ana.rows.len(), 1);
    assert_eq!(ana.rows[0][1], s("Ana"));
    assert_eq!(session.count(&relation, "ana").await.unwrap(), Some(1));
    assert_eq!(
        session
            .page(&relation, 10, 0, "000003", None)
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    // Regex characters are searched as text.
    assert_eq!(
        session
            .page(&relation, 10, 0, "a.a", None)
            .await
            .unwrap()
            .rows
            .len(),
        0
    );

    // Read-only queries: only reading methods run.
    let found = session
        .read_only(&format!("db.{name}.find({{name: \"Ana\"}})"))
        .await
        .unwrap();
    assert_eq!(rows(&found).len(), 1);
    assert!(session
        .read_only(&format!("db.{name}.countDocuments({{}})"))
        .await
        .is_ok());
    assert!(session.read_only("show collections").await.is_ok());
    let total = session.count(&relation, "").await.unwrap();
    for attempt in [
        format!("db.{name}.insertOne({{a: 1}})"),
        format!("db.{name}.deleteMany({{}})"),
        format!("db.{name}.drop()"),
        format!("db.{name}.aggregate([{{$out: \"stolen\"}}])"),
        format!("db.{name}.aggregate([{{$merge: {{into: \"x\"}}}}])"),
        format!("db.{name}.find({{}}); db.{name}.deleteMany({{}})"),
        "db.dropDatabase()".to_string(),
        "use admin".to_string(),
        format!("db.runCommand({{drop: \"{name}\"}})"),
    ] {
        assert!(session.read_only(&attempt).await.is_err(), "{attempt}");
    }
    assert_eq!(session.count(&relation, "").await.unwrap(), total);

    let by_age = session
        .page(
            &relation,
            10,
            0,
            "",
            Some(&ColumnSort {
                column: "age".into(),
                ascending: false,
            }),
        )
        .await
        .unwrap();
    let ages: Vec<f64> = by_age
        .rows
        .iter()
        .filter_map(|r| r[2].as_deref()?.parse().ok())
        .collect();
    assert!(!ages.is_empty());
    assert!(ages.windows(2).all(|w| w[0] >= w[1]), "{ages:?}");
    run(&session, &format!("db.{name}.drop()")).await;
}

#[tokio::test]
async fn pages_mixed_documents() {
    let Some(session) = session().await else {
        return;
    };
    let name = collection_name("page");
    seed(&session, &name).await;
    let relation = collection(&session, &name).await;
    let page = session.page(&relation, 10, 0, "", None).await.unwrap();
    let names: Vec<&str> = page.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "_id", "name", "age", "tags", "address", "joined", "active", "score", "big", "price",
            "code", "note", "bin"
        ]
    );
    let types: Vec<&str> = page.columns.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(
        types,
        [
            "objectId", "string", "int", "array", "object", "date", "bool", "double", "long",
            "decimal", "string", "string", "binData"
        ]
    );
    assert_eq!(
        page.rows[0],
        [
            s("650000000000000000000001"),
            s("Ana"),
            s("30"),
            s(r#"["a","b"]"#),
            s(r#"{"city":"Lisbon","zip":"1000"}"#),
            s("2024-05-01T10:30:00.250Z"),
            s("true"),
            None,
            None,
            None,
            None,
            None,
            None
        ]
    );
    assert_eq!(
        page.rows[1],
        [
            s("650000000000000000000002"),
            s("Bia"),
            None,
            None,
            None,
            None,
            None,
            s("1.5"),
            s("9007199254740993"),
            s("9.99"),
            s("42"),
            None,
            None
        ]
    );
    assert_eq!(
        page.rows[2],
        [
            s("650000000000000000000003"),
            None,
            s("7"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            s("here"),
            s(r#"{"$binary":{"base64":"AQID","subType":"00"}}"#)
        ]
    );
    let keys = page.keys.clone().unwrap();
    assert_eq!(
        (keys.columns.as_slice(), keys.kind),
        (["_id".to_string()].as_slice(), RowKeyKind::PrimaryKey)
    );
    assert_eq!(
        keys.values[0],
        [s(r#"{"$oid":"650000000000000000000001"}"#)]
    );
    assert_eq!(page.note.as_deref(), Some("3 documents"));
    assert_eq!(page.command, "FIND 3");

    let second = session.page(&relation, 2, 2, "", None).await.unwrap();
    assert_eq!(
        second.rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(),
        [s("650000000000000000000003")]
    );
    run(&session, &format!("db.{name}.drop()")).await;
}

#[tokio::test]
async fn edits_documents() {
    let Some(session) = session().await else {
        return;
    };
    let name = collection_name("edit");
    seed(&session, &name).await;
    let relation = collection(&session, &name).await;
    let page = session.page(&relation, 10, 0, "", None).await.unwrap();
    let keys = page.keys.clone().unwrap();
    let key = |i: usize| keys.row(i).unwrap();

    // A string field stays a string, even when the text looks like a number.
    let outcome = session
        .update(
            &relation,
            &key(1),
            &[cell("code", Some("43")), cell("name", Some("Bia Maria"))],
        )
        .await
        .unwrap();
    assert_eq!(
        (outcome.command.as_str(), outcome.affected),
        ("UPDATE 1", 1)
    );
    assert_eq!(
        outcome.sql,
        format!("db.{name}.updateOne({{_id: ?}}, {{$set: {{code: ?, name: ?}}}})")
    );
    let typed = run(
        &session,
        &format!(r#"db.{name}.countDocuments({{code: {{$type: "string"}}, name: "Bia Maria"}})"#),
    )
    .await;
    assert_eq!(rows(&typed), [[s("1")]]);

    // Numbers, dates and new fields.
    session
        .update(&relation, &key(2), &[cell("age", Some("8"))])
        .await
        .unwrap();
    session
        .update(
            &relation,
            &key(0),
            &[
                cell("joined", Some("2025-01-02T03:04:05Z")),
                cell("extra", Some("{n: 1}")),
                cell("note", None),
            ],
        )
        .await
        .unwrap();
    let page = session.page(&relation, 10, 0, "", None).await.unwrap();
    let column = |field: &str| page.columns.iter().position(|c| c.name == field).unwrap();
    assert_eq!(page.rows[2][column("age")], s("8"));
    assert_eq!(
        page.rows[0][column("joined")],
        s("2025-01-02T03:04:05.000Z")
    );
    assert_eq!(page.rows[0][column("extra")], s(r#"{"n":1}"#));
    let checks = run(
        &session,
        &format!(
            "db.{name}.countDocuments({{age: {{$type: \"int\"}}}})\n\
             db.{name}.countDocuments({{joined: {{$type: \"date\"}}}})\n\
             db.{name}.countDocuments({{note: null, _id: ObjectId(\"650000000000000000000001\")}})"
        ),
    )
    .await;
    let counts: Vec<_> = checks.results.iter().map(|r| r.rows.clone()).collect();
    assert_eq!(
        counts,
        [vec![vec![s("2")]], vec![vec![s("1")]], vec![vec![s("1")]]]
    );

    // Wrong types, the _id, and a document that's gone.
    let error = session
        .update(&relation, &key(2), &[cell("age", Some("eight"))])
        .await
        .unwrap_err();
    assert_eq!(error.message(), "This isn't a valid int.");
    assert!(session
        .update(&relation, &key(0), &[cell("_id", Some("x"))])
        .await
        .is_err());
    run(
        &session,
        &format!(r#"db.{name}.deleteOne({{_id: ObjectId("650000000000000000000003")}})"#),
    )
    .await;
    let changed =
        "The row changed or was deleted since it was loaded. Reload the table and try again.";
    assert_eq!(
        session
            .update(&relation, &key(2), &[cell("age", Some("9"))])
            .await
            .unwrap_err()
            .message(),
        changed
    );
    assert_eq!(
        session
            .delete(&relation, &key(2))
            .await
            .unwrap_err()
            .message(),
        changed
    );
    assert_eq!(
        session
            .update(&relation, &key(2), &[])
            .await
            .unwrap()
            .command,
        "UPDATE 0"
    );

    // Insert: an empty _id becomes an ObjectId; types come from the page.
    let inserted = session
        .insert(
            &relation,
            &[
                cell("_id", Some("")),
                cell("name", Some("Novo")),
                cell("age", Some("5")),
                cell("code", Some("7")),
            ],
        )
        .await
        .unwrap();
    assert_eq!(inserted.command, "INSERT 1");
    assert_eq!(
        inserted.sql,
        format!("db.{name}.insertOne({{name: ?, age: ?, code: ?}})")
    );
    let found = run(
        &session,
        &format!(r#"db.{name}.find({{name: "Novo", age: {{$type: "int"}}, code: "7"}})"#),
    )
    .await;
    assert_eq!(found.main().unwrap().rows.len(), 1);
    assert_eq!(found.main().unwrap().columns[0].type_name, "objectId");
    // Text that isn't the column's type is taken as it reads.
    session
        .insert(
            &relation,
            &[
                cell("_id", Some("own-id")),
                cell("name", Some("Own")),
                cell("age", Some("old")),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        rows(
            &run(
                &session,
                &format!(r#"db.{name}.countDocuments({{_id: "own-id", age: "old"}})"#)
            )
            .await
        ),
        [[s("1")]]
    );

    // Delete exactly the row.
    let page = session.page(&relation, 10, 0, "", None).await.unwrap();
    let keys = page.keys.clone().unwrap();
    let row = page.rows.iter().position(|r| r[0] == s("own-id")).unwrap();
    let deleted = session
        .delete(&relation, &keys.row(row).unwrap())
        .await
        .unwrap();
    assert_eq!(
        (deleted.command.as_str(), deleted.sql.clone()),
        ("DELETE 1", format!("db.{name}.deleteOne({{_id: ?}})"))
    );
    assert_eq!(session.count(&relation, "").await.unwrap(), Some(3));
    run(&session, &format!("db.{name}.drop()")).await;
}

#[tokio::test]
async fn runs_commands() {
    let Some(session) = session().await else {
        return;
    };
    let name = collection_name("exec");
    let other = collection_name("db");
    let home = database();
    let command = |e: &dblens_lib::session::Execution| e.main().unwrap().command.clone();

    assert_eq!(
        command(
            &run(
                &session,
                &format!("db.{name}.insertOne({{_id: 1, name: 'Ana', city: 'Lisbon', age: 30}})")
            )
            .await
        ),
        "INSERT 1"
    );
    let many = run(
        &session,
        &format!(
            "db.{name}.insertMany([\n  {{_id: 2, name: 'Bia', city: 'Porto', age: 25}},\n  {{_id: 3, name: 'Caio', city: 'Lisbon', age: 41, tags: ['x']}},\n  {{_id: 4, name: 'Duda', city: 'Faro'}},\n])"
        ),
    )
    .await;
    assert_eq!(command(&many), "INSERT 3");

    let found = run(
        &session,
        &format!("db.{name}.find({{city: 'Lisbon'}}, {{name: 1, age: 1}}).sort({{age: -1}})"),
    )
    .await;
    let main = found.main().unwrap();
    assert_eq!(
        main.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["_id", "name", "age"]
    );
    assert_eq!(
        main.rows,
        [[s("3"), s("Caio"), s("41")], [s("1"), s("Ana"), s("30")]]
    );
    assert_eq!(main.command, "FIND 2");
    let chained = run(
        &session,
        &format!("db.getCollection('{name}').find()\n  .sort({{_id: 1}})\n  .skip(1)\n  .limit(2)"),
    )
    .await;
    assert_eq!(
        rows(&chained)
            .iter()
            .map(|r| r[0].clone())
            .collect::<Vec<_>>(),
        [s("2"), s("3")]
    );
    assert_eq!(
        rows(
            &run(
                &session,
                &format!("db.{name}.find({{name: /^d/i}}).toArray()")
            )
            .await
        )[0][1],
        s("Duda")
    );
    assert_eq!(
        rows(&run(&session, &format!("db.{name}.findOne({{_id: {{$gt: 1}}}})")).await).len(),
        1
    );
    assert_eq!(
        rows(&run(&session, &format!("db.{name}.find({{}}).count()")).await),
        [[s("4")]]
    );
    assert_eq!(
        rows(
            &run(
                &session,
                &format!("db.{name}.countDocuments({{age: {{$exists: true}}}})")
            )
            .await
        ),
        [[s("3")]]
    );
    assert_eq!(
        rows(&run(&session, &format!("db.{name}.estimatedDocumentCount()")).await),
        [[s("4")]]
    );
    assert_eq!(
        rows(&run(&session, &format!("db.{name}.find().limit(-2)")).await).len(),
        2
    );
    let distinct = run(
        &session,
        &format!("db.{name}.distinct('city', {{age: {{$gte: 25}}}})"),
    )
    .await;
    let main = distinct.main().unwrap();
    assert_eq!(main.columns[0].name, "value");
    let mut cities: Vec<_> = main.rows.iter().filter_map(|r| r[0].clone()).collect();
    cities.sort();
    assert_eq!(cities, ["Lisbon", "Porto"]);

    let grouped = run(
        &session,
        &format!(
            "db.{name}.aggregate([\n  {{$group: {{_id: '$city', total: {{$sum: 1}}, ages: {{$sum: '$age'}}}}}},\n  {{$sort: {{total: -1, _id: 1}}}},\n])"
        ),
    )
    .await;
    let main = grouped.main().unwrap();
    assert_eq!(
        main.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["_id", "total", "ages"]
    );
    assert_eq!(
        main.rows,
        [
            [s("Lisbon"), s("2"), s("71")],
            [s("Faro"), s("1"), s("0")],
            [s("Porto"), s("1"), s("25")]
        ]
    );
    assert_eq!(main.command, "AGGREGATE 3");
    assert_eq!(
        rows(
            &run(
                &session,
                &format!(
                    "db.{name}.aggregate([{{$count: 'n'}}], {{batchSize: 1, allowDiskUse: true}})"
                )
            )
            .await
        ),
        [[s("4")]]
    );

    assert_eq!(
        command(
            &run(
                &session,
                &format!("db.{name}.updateOne({{_id: 1}}, {{$set: {{age: 31}}}})")
            )
            .await
        ),
        "UPDATE 1"
    );
    assert_eq!(
        command(
            &run(
                &session,
                &format!("db.{name}.updateMany({{city: 'Lisbon'}}, {{$inc: {{age: 1}}}})")
            )
            .await
        ),
        "UPDATE 2"
    );
    assert_eq!(
        command(
            &run(
                &session,
                &format!(
                    "db.{name}.updateMany({{}}, [{{$set: {{upper: {{$toUpper: '$name'}}}}}}])"
                )
            )
            .await
        ),
        "UPDATE 4"
    );
    assert_eq!(
        command(
            &run(
                &session,
                &format!("db.{name}.updateOne({{_id: 9}}, {{$set: {{a: 1}}}}, {{upsert: true}})")
            )
            .await
        ),
        "UPDATE 0"
    );
    assert_eq!(
        rows(&run(&session, &format!("db.{name}.countDocuments({{_id: 9}})")).await),
        [[s("1")]]
    );
    assert_eq!(
        command(
            &run(
                &session,
                &format!("db.{name}.replaceOne({{_id: 4}}, {{name: 'Duda', city: 'Braga'}})")
            )
            .await
        ),
        "UPDATE 1"
    );
    assert_eq!(
        rows(&run(&session, &format!("db.{name}.findOne({{_id: 4}})")).await),
        [[s("4"), s("Duda"), s("Braga")]]
    );
    assert_eq!(
        command(&run(&session, &format!("db.{name}.deleteOne({{_id: 4}})")).await),
        "DELETE 1"
    );
    assert_eq!(
        command(
            &run(
                &session,
                &format!("db.{name}.deleteMany({{age: {{$lt: 100}}}})")
            )
            .await
        ),
        "DELETE 3"
    );
    run(&session, &format!("db.{name}.deleteMany({{}})")).await;

    let index = run(
        &session,
        &format!("db.{name}.createIndex({{city: 1, age: -1}})"),
    )
    .await;
    assert_eq!(
        (rows(&index), command(&index)),
        (vec![vec![s("city_1_age_-1")]], "CREATE INDEX".to_string())
    );
    assert_eq!(
        rows(
            &run(
                &session,
                &format!("db.{name}.createIndex({{name: 1}}, {{unique: true, name: 'by_name'}})")
            )
            .await
        ),
        [[s("by_name")]]
    );
    let indexes = run(&session, &format!("db.{name}.getIndexes()")).await;
    let main = indexes.main().unwrap();
    let at = main.columns.iter().position(|c| c.name == "name").unwrap();
    let mut names: Vec<_> = main.rows.iter().filter_map(|r| r[at].clone()).collect();
    names.sort();
    assert_eq!(names, ["_id_", "by_name", "city_1_age_-1"]);

    assert!(rows(&run(&session, "db.getCollectionNames()").await).contains(&vec![s(&name)]));
    assert!(rows(&run(&session, "show collections").await).contains(&vec![s(&name)]));
    let dbs = run(&session, "show dbs").await;
    assert_eq!(dbs.main().unwrap().columns[0].name, "name");
    assert!(rows(&dbs).iter().any(|r| r[0] == s(&home)));
    assert_eq!(rows(&run(&session, "db.getName()").await), [[s(&home)]]);
    assert!(rows(&run(&session, "db.runCommand('ping')").await)[0][0].is_some());
    assert_eq!(command(&run(&session, "db.stats()").await), "OK");

    // use switches the database for later statements, also in later runs.
    let used = run(
        &session,
        &format!("use {other}\ndb.things.insertOne({{a: 1}})\ndb.getName()"),
    )
    .await;
    let commands: Vec<_> = used.results.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(
        commands,
        [format!("USE {other}").as_str(), "INSERT 1", "FIND 1"]
    );
    assert_eq!(rows(&used), [[s(&other)]]);
    assert_eq!(session.current_database().await, other);
    assert_eq!(
        rows(&run(&session, "db.things.countDocuments()").await),
        [[s("1")]]
    );
    assert_eq!(
        command(
            &run(
                &session,
                "db.createCollection('made', {capped: true, size: 4096})"
            )
            .await
        ),
        "CREATE COLLECTION"
    );
    assert_eq!(
        command(&run(&session, "db.dropDatabase()").await),
        "DROP DATABASE"
    );
    run(&session, &format!("use {home}")).await;

    assert_eq!(
        command(&run(&session, &format!("db.{name}.drop()")).await),
        "DROP"
    );
    assert_eq!(
        command(&run(&session, &format!("db.{name}.drop()")).await),
        "DROP"
    );

    // Errors: the server's own words, and what dbLens doesn't run.
    let error = session
        .execute("db.runCommand({noSuchCommand: 1})")
        .await
        .unwrap_err()
        .message();
    assert!(error.contains("noSuchCommand"), "{error}");
    let error = session
        .execute(&format!("db.{name}.mapReduce(1, 2)"))
        .await
        .unwrap_err()
        .message();
    assert_eq!(
        error,
        format!("dbLens can't run this yet: db.{name}.mapReduce(1, 2)")
    );
    assert!(session
        .execute(&format!("db.{name}.insertOne({{a: }})"))
        .await
        .is_err());
    assert!(session
        .execute(&format!("db.{name}.find({{$bad: 1}})"))
        .await
        .is_err());
    let duplicate = session
        .execute(&format!("db.{name}.insertMany([{{_id: 1}}, {{_id: 1}}])"))
        .await
        .unwrap_err()
        .message();
    assert!(duplicate.contains("duplicate key"), "{duplicate}");
    assert!(session
        .execute("// only a comment")
        .await
        .unwrap()
        .results
        .is_empty());
    run(&session, &format!("db.{name}.drop()")).await;
    // Mongo has no transaction dbLens can roll back.
    assert!(matches!(
        session.execute_atomically("db.x.find()").await,
        Err(DbError::NotAtomic(_))
    ));
    assert!(session.rehearse("db.x.find()").await.is_err());
}

#[tokio::test]
async fn cuts_big_results() {
    let Some(session) = session().await else {
        return;
    };
    let name = collection_name("big");
    run(
        &session,
        &format!(
            "db.{name}_seed.insertOne({{}})\n\
             db.{name}_seed.aggregate([{{$project: {{_id: 0, n: {{$range: [0, 6000]}}}}}}, {{$unwind: '$n'}}, {{$out: '{name}'}}])"
        ),
    )
    .await;
    let result = run(&session, &format!("db.{name}.find()")).await;
    let main = result.main().unwrap();
    assert_eq!(main.rows.len(), ROW_LIMIT);
    assert!(main.truncated);
    let small = run(&session, &format!("db.{name}.find().limit(10)")).await;
    assert_eq!(small.main().unwrap().rows.len(), 10);
    assert!(!small.main().unwrap().truncated);
    let relation = collection(&session, &name).await;
    assert_eq!(session.count(&relation, "").await.unwrap(), Some(6000));
    let page = session
        .page(
            &relation,
            100,
            5950,
            "",
            Some(&ColumnSort {
                column: "n".into(),
                ascending: true,
            }),
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 50);
    assert_eq!(page.note.as_deref(), Some("6,000 documents"));
    run(
        &session,
        &format!("db.{name}.drop(); db.{name}_seed.drop()"),
    )
    .await;
}

#[tokio::test]
async fn guesses_links_on_the_board() {
    let Some(session) = session().await else {
        return;
    };
    let home = database();
    let users = collection_name("users");
    let owner = collection_name("owner");
    let orders = collection_name("orders");
    run(
        &session,
        &format!(
            "db.{users}s.insertOne({{_id: ObjectId(\"650000000000000000000001\"), name: 'Ana'}})\n\
             db.{owner}.insertOne({{_id: ObjectId(\"650000000000000000000009\")}})\n\
             db.{orders}.insertMany([\n\
               {{{users}_id: ObjectId(\"650000000000000000000001\"), {owner}Id: ObjectId(\"650000000000000000000009\"), total: 10}},\n\
               {{{users}_id: ObjectId(\"650000000000000000000001\"), total: 5.5, coupon: null}},\n\
               {{{users}_id: ObjectId(\"650000000000000000000001\"), total: 7}},\n\
             ])"
        ),
    )
    .await;
    let graph = session.schema_graph().await.unwrap();
    let table = graph.tables.iter().find(|t| t.name == orders).unwrap();
    assert_eq!(table.schema, home);
    assert!(!table.is_view);
    let column = |name: &str| {
        table
            .columns
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert_eq!(table.columns[0].name, "_id");
    assert!(column("_id").is_primary_key && !column("_id").is_nullable);
    assert_eq!(column(&format!("{users}_id")).type_name, "objectId");
    assert!(!column(&format!("{users}_id")).is_nullable);
    assert_eq!(column("total").type_name, "int");
    assert!(!column("total").is_nullable);
    assert_eq!(column("coupon").type_name, "null");
    assert!(column("coupon").is_nullable);
    assert!(column(&format!("{owner}Id")).is_nullable);
    let from = format!("{home}.{orders}");
    let mut links: Vec<_> = graph.links.iter().filter(|l| l.from == from).collect();
    links.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(
        links.iter().map(|l| l.to.clone()).collect::<Vec<_>>(),
        [format!("{home}.{owner}"), format!("{home}.{users}s")]
    );
    assert_eq!(
        links.iter().map(|l| l.name.clone()).collect::<Vec<_>>(),
        [
            format!("guessed ({owner}Id)"),
            format!("guessed ({users}_id)")
        ]
    );
    assert!(links.iter().all(|l| l.to_columns == ["_id"]));
    run(
        &session,
        &format!("db.{users}s.drop(); db.{owner}.drop(); db.{orders}.drop()"),
    )
    .await;

    // One guessed link in a collection is named just "guessed".
    let solo = collection_name("solo");
    run(
        &session,
        &format!("db.{owner}.insertOne({{}}); db.{solo}.insertOne({{{owner}_id: ObjectId()}})"),
    )
    .await;
    let solo_from = format!("{home}.{solo}");
    let names: Vec<_> = session
        .schema_graph()
        .await
        .unwrap()
        .links
        .into_iter()
        .filter(|l| l.from == solo_from)
        .map(|l| l.name)
        .collect();
    assert_eq!(names, ["guessed"]);
    run(&session, &format!("db.{owner}.drop(); db.{solo}.drop()")).await;
}

#[tokio::test]
async fn cancels_slow_finds() {
    let Some(session) = session().await else {
        return;
    };
    let session = Arc::new(session);
    let name = collection_name("slow");
    run(
        &session,
        &format!(
            "db.{name}_seed.insertOne({{}})\n\
             db.{name}_seed.aggregate([{{$project: {{_id: 0, n: {{$range: [0, 200000]}}}}}}, {{$unwind: '$n'}}, {{$out: '{name}'}}])"
        ),
    )
    .await;
    // Matches nothing, and works hard on every document: one long server operation.
    let slow = format!("db.{name}.find({{$expr: {{$lt: [{{$size: {{$range: [{{$mod: ['$n', 7]}}, 3000]}}}}, 0]}}}})");
    let start = Instant::now();
    let running = tokio::spawn({
        let session = session.clone();
        async move { session.execute(&slow).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    session.cancel();
    let outcome = running.await.unwrap();
    assert_eq!(outcome.unwrap_err(), DbError::Cancelled);
    let elapsed = start.elapsed();
    println!("canceled after {elapsed:?}");
    assert!(elapsed < Duration::from_millis(2500), "{elapsed:?}");
    // The server stops the operation too (killOp by its comment).
    let mut stopped = false;
    for _ in 0..30 {
        let ops = run(
            &session,
            &format!(
                "db.adminCommand({{currentOp: 1, ns: '{}.{name}'}})",
                database()
            ),
        )
        .await;
        let main = ops.main().unwrap();
        let at = main
            .columns
            .iter()
            .position(|c| c.name == "inprog")
            .unwrap();
        if main.rows[0][at].as_deref() == Some("[]") {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(stopped, "the find still runs on the server");
    // Cancel with nothing running does nothing; the session goes on.
    session.cancel();
    assert_eq!(
        rows(&run(&session, &format!("db.{name}.countDocuments()")).await),
        [[s("200000")]]
    );
    run(
        &session,
        &format!("db.{name}.drop(); db.{name}_seed.drop()"),
    )
    .await;
}
