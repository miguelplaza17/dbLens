//! A small pool per open database, so browsing, counting and running SQL don't
//! wait for each other.
//!
//! - The **pinned** connection is the SQL panel's: every `execute`,
//!   `execute_stream`, `read_only`, `rehearse` and `execute_atomically` runs
//!   there, so a `BEGIN` in one run and a `COMMIT` in a later one share a
//!   transaction.
//! - **Browse** connections (opened lazily, one warmed up at connect) serve
//!   `relations`, `page`, `page_block`, `count` and `schema_graph`, and the
//!   grid's edits.
//! - While the user's transaction is open on the pinned connection
//!   (`DatabaseSession::transaction_open`), browsing and edits go to the pinned
//!   connection too: the grid then shows the transaction's own rows, and an
//!   edit joins the transaction behind a savepoint (the drivers' existing logic).
//! - Engines whose browse connections can't write (SQLite readers) keep their
//!   edits on the pinned connection.
//!
//! `PooledSession` also remembers block boundaries (the key of the last row of
//! each block) so `page_block` can hand drivers a `KeyHint` for keyset pagination.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::engine::Engine;
use crate::session::{
    keyset_direction, CellValue, ColumnSort, DatabaseSession, DbResult, EditOutcome, Execution,
    KeyHint, QueryResult, Relation, RowKey, SchemaGraph,
};
use crate::wire::StreamSink;

/// Opens one more connection to the same database (same URL, same tunnel).
pub type Opener =
    Arc<dyn Fn() -> BoxFuture<'static, DbResult<Box<dyn DatabaseSession>>> + Send + Sync>;

/// How many browse connections an engine gets, and where its edits go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub browse: usize,
    /// Edits always on the pinned connection (SQLite: its readers are read-only).
    pub edits_on_pinned: bool,
}

impl Policy {
    pub fn for_engine(engine: Engine) -> Policy {
        match engine {
            Engine::Postgres | Engine::Mysql => Policy {
                browse: 3,
                edits_on_pinned: false,
            },
            Engine::Sqlite => Policy {
                browse: 2,
                edits_on_pinned: true,
            },
            // Commands on one connection, browsing (SCAN, pages) on another.
            Engine::Redis => Policy {
                browse: 1,
                edits_on_pinned: false,
            },
            // The driver's client pools connections itself.
            Engine::Mongodb => Policy {
                browse: 0,
                edits_on_pinned: true,
            },
        }
    }
}

struct Browse {
    open: Opener,
    /// One permit per connection that may exist.
    permits: Semaphore,
    idle: StdMutex<Vec<Arc<dyn DatabaseSession>>>,
    all: StdMutex<Vec<Arc<dyn DatabaseSession>>>,
    /// Opening one failed (too many connections, say): use the pinned one.
    broken: AtomicBool,
    closed: AtomicBool,
}

impl Browse {
    fn put_back(&self, conn: Arc<dyn DatabaseSession>) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(mut idle) = self.idle.lock() {
            idle.push(conn);
        }
    }

    async fn open_one(&self) -> Option<Arc<dyn DatabaseSession>> {
        match (self.open)().await {
            Ok(conn) => {
                let conn: Arc<dyn DatabaseSession> = Arc::from(conn);
                if let Ok(mut all) = self.all.lock() {
                    all.push(conn.clone());
                }
                Some(conn)
            }
            Err(_) => {
                self.broken.store(true, Ordering::SeqCst);
                None
            }
        }
    }
}

/// A connection borrowed for one operation; a browse one goes back when dropped.
enum Lease<'a> {
    Pinned(&'a Arc<dyn DatabaseSession>),
    Browse {
        conn: Option<Arc<dyn DatabaseSession>>,
        pool: &'a Browse,
        _permit: SemaphorePermit<'a>,
    },
}

impl std::ops::Deref for Lease<'_> {
    type Target = dyn DatabaseSession;
    fn deref(&self) -> &Self::Target {
        match self {
            Lease::Pinned(conn) => conn.as_ref(),
            Lease::Browse { conn, .. } => conn.as_deref().expect("leased connection"),
        }
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Lease::Browse { conn, pool, .. } = self {
            if let Some(conn) = conn.take() {
                pool.put_back(conn);
            }
        }
    }
}

/// Block boundaries per (relation, search, sort): offset → key of the row there.
#[derive(Default)]
struct Hints {
    by_view: HashMap<String, BTreeMap<u64, Vec<Option<String>>>>,
}

const MAX_VIEWS: usize = 32;
const MAX_HINTS_PER_VIEW: usize = 4_096;

fn view_key(relation: &Relation, search: &str, sort: Option<&ColumnSort>) -> String {
    match sort {
        Some(s) => format!(
            "{}\u{1f}{search}\u{1f}{}\u{1f}{}",
            relation.id(),
            s.column,
            s.ascending
        ),
        None => format!("{}\u{1f}{search}", relation.id()),
    }
}

/// The open database: a pinned connection for the SQL panel and a few for browsing.
pub struct PooledSession {
    pinned: Arc<dyn DatabaseSession>,
    browse: Option<Arc<Browse>>,
    edits_on_pinned: bool,
    hints: StdMutex<Hints>,
}

impl PooledSession {
    /// A pool around `pinned`; `open` makes the browse connections (none when
    /// the policy has none or `open` is None).
    pub fn new(pinned: Arc<dyn DatabaseSession>, policy: Policy, open: Option<Opener>) -> Self {
        let browse = open.filter(|_| policy.browse > 0).map(|open| {
            Arc::new(Browse {
                open,
                permits: Semaphore::new(policy.browse),
                idle: StdMutex::new(vec![]),
                all: StdMutex::new(vec![]),
                broken: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            })
        });
        Self {
            pinned,
            browse,
            edits_on_pinned: policy.edits_on_pinned,
            hints: StdMutex::new(Hints::default()),
        }
    }

    /// Opens the first browse connection in the background, so the first table
    /// click finds it ready.
    pub fn warm_up(&self) {
        let Some(pool) = self.browse.clone() else {
            return;
        };
        tokio::spawn(async move {
            let Ok(_permit) = pool.permits.try_acquire() else {
                return;
            };
            if let Some(conn) = pool.open_one().await {
                pool.put_back(conn);
            }
        });
    }

    /// How many browse connections are open now (for tests and the bench).
    pub fn browse_connections(&self) -> usize {
        self.browse
            .as_ref()
            .and_then(|p| p.all.lock().ok().map(|a| a.len()))
            .unwrap_or(0)
    }

    /// The pinned connection (the SQL panel's).
    pub fn pinned(&self) -> &dyn DatabaseSession {
        self.pinned.as_ref()
    }

    /// Stops the SQL panel's run (and the stream), leaving browsing alone.
    pub fn cancel_sql(&self) {
        self.pinned.cancel();
    }

    /// Stops every running operation on every connection.
    pub fn cancel_all(&self) {
        self.pinned.cancel();
        if let Some(pool) = &self.browse {
            if let Ok(all) = pool.all.lock() {
                for conn in all.iter() {
                    conn.cancel();
                }
            }
        }
    }

    /// A browse connection, or the pinned one while the user's transaction is
    /// open there (or when there is no pool).
    async fn reader(&self) -> Lease<'_> {
        if self.pinned.transaction_open() {
            return Lease::Pinned(&self.pinned);
        }
        self.browse_lease().await
    }

    async fn writer(&self) -> Lease<'_> {
        if self.edits_on_pinned || self.pinned.transaction_open() {
            return Lease::Pinned(&self.pinned);
        }
        self.browse_lease().await
    }

    async fn browse_lease(&self) -> Lease<'_> {
        let Some(pool) = self.browse.as_deref() else {
            return Lease::Pinned(&self.pinned);
        };
        if pool.broken.load(Ordering::SeqCst) || pool.closed.load(Ordering::SeqCst) {
            return Lease::Pinned(&self.pinned);
        }
        let Ok(permit) = pool.permits.acquire().await else {
            return Lease::Pinned(&self.pinned);
        };
        let idle = pool.idle.lock().ok().and_then(|mut idle| idle.pop());
        let conn = match idle {
            Some(conn) => Some(conn),
            None => pool.open_one().await,
        };
        match conn {
            Some(conn) => Lease::Browse {
                conn: Some(conn),
                pool,
                _permit: permit,
            },
            None => Lease::Pinned(&self.pinned),
        }
    }

    fn forget_hints(&self) {
        if let Ok(mut hints) = self.hints.lock() {
            hints.by_view.clear();
        }
    }

    fn hint(&self, view: &str, offset: u64) -> Option<KeyHint> {
        let mut hints = self.hints.lock().ok()?;
        if offset == 0 {
            // A first block starts the view over: rows may have moved since.
            hints.by_view.remove(view);
            return None;
        }
        let (at, values) = hints.by_view.get(view)?.range(..offset).next_back()?;
        Some(KeyHint {
            offset: *at,
            values: values.clone(),
        })
    }

    fn remember(&self, view: String, offset: u64, result: &QueryResult, sort: Option<&ColumnSort>) {
        let Some(keys) = &result.keys else {
            return;
        };
        if keyset_direction(&keys.columns, sort).is_none() {
            return;
        }
        let Some(last) = keys.values.last() else {
            return;
        };
        let Ok(mut hints) = self.hints.lock() else {
            return;
        };
        if !hints.by_view.contains_key(&view) && hints.by_view.len() >= MAX_VIEWS {
            hints.by_view.clear();
        }
        let map = hints.by_view.entry(view).or_default();
        if map.len() >= MAX_HINTS_PER_VIEW {
            map.clear();
        }
        map.insert(offset + keys.values.len() as u64 - 1, last.clone());
    }
}

#[async_trait]
impl DatabaseSession for PooledSession {
    fn engine(&self) -> Engine {
        self.pinned.engine()
    }

    fn server_version(&self) -> String {
        self.pinned.server_version()
    }

    fn cancel(&self) {
        self.cancel_all();
    }

    fn quote(&self, identifier: &str) -> String {
        self.pinned.quote(identifier)
    }

    fn qualified(&self, relation: &Relation) -> String {
        self.pinned.qualified(relation)
    }

    async fn execute(&self, sql: &str) -> DbResult<Execution> {
        self.forget_hints();
        self.pinned.execute(sql).await
    }

    async fn execute_stream(&self, sql: &str, sink: &mut StreamSink) -> DbResult<()> {
        self.forget_hints();
        self.pinned.execute_stream(sql, sink).await
    }

    async fn read_only(&self, text: &str) -> DbResult<Execution> {
        self.pinned.read_only(text).await
    }

    async fn rehearse(&self, text: &str) -> DbResult<Execution> {
        self.pinned.rehearse(text).await
    }

    async fn execute_atomically(&self, text: &str) -> DbResult<Execution> {
        self.forget_hints();
        self.pinned.execute_atomically(text).await
    }

    async fn relations(&self) -> DbResult<Vec<Relation>> {
        self.reader().await.relations().await
    }

    async fn page(
        &self,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
    ) -> DbResult<QueryResult> {
        self.page_block(relation, offset, limit, search, sort, None)
            .await
    }

    async fn page_block(
        &self,
        relation: &Relation,
        offset: u64,
        limit: u32,
        search: &str,
        sort: Option<&ColumnSort>,
        hint: Option<&KeyHint>,
    ) -> DbResult<QueryResult> {
        let view = view_key(relation, search, sort);
        let remembered = match hint {
            Some(hint) => Some(hint.clone()),
            None => self.hint(&view, offset),
        };
        let result = self
            .reader()
            .await
            .page_block(relation, offset, limit, search, sort, remembered.as_ref())
            .await?;
        self.remember(view, offset, &result, sort);
        Ok(result)
    }

    async fn count(&self, relation: &Relation, search: &str) -> DbResult<Option<u64>> {
        self.reader().await.count(relation, search).await
    }

    async fn update(
        &self,
        relation: &Relation,
        key: &RowKey,
        changes: &[CellValue],
    ) -> DbResult<EditOutcome> {
        self.forget_hints();
        self.writer().await.update(relation, key, changes).await
    }

    async fn insert(&self, relation: &Relation, values: &[CellValue]) -> DbResult<EditOutcome> {
        self.forget_hints();
        self.writer().await.insert(relation, values).await
    }

    async fn delete(&self, relation: &Relation, key: &RowKey) -> DbResult<EditOutcome> {
        self.forget_hints();
        self.writer().await.delete(relation, key).await
    }

    async fn schema_graph(&self) -> DbResult<SchemaGraph> {
        self.reader().await.schema_graph().await
    }

    fn transaction_open(&self) -> bool {
        self.pinned.transaction_open()
    }

    async fn close(&self) {
        let others = match &self.browse {
            Some(pool) => {
                pool.closed.store(true, Ordering::SeqCst);
                if let Ok(mut idle) = pool.idle.lock() {
                    idle.clear();
                }
                pool.all
                    .lock()
                    .map(|mut all| std::mem::take(&mut *all))
                    .unwrap_or_default()
            }
            None => vec![],
        };
        let closing = others.iter().map(|c| c.close());
        futures_util::future::join(self.pinned.close(), futures_util::future::join_all(closing))
            .await;
    }
}
