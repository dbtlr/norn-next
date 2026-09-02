//! The open ceremony's contract, over a client this suite owns.
//!
//! The clients that run it in the product carry a domain — a store's mode, a
//! sidecar's model — and their suites prove what those mean. What is proven
//! here is the ceremony itself: which verdict each state of the file reaches,
//! which of them the client is asked about, and what a rebuild leaves behind.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use norn_db::rusqlite::Connection;
use norn_db::{Adoption, Client, DbError, OpenOutcome, RebuildReason, Schema, meta};

/// Distinguishes two scratch directories taken in the same process.
static SERIAL: AtomicU64 = AtomicU64::new(0);

/// A directory that exists for one test and is removed with it.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "norn-db-ceremony-{label}-{}-{serial}",
            std::process::id()
        ));
        Scratch { root }
    }

    /// The database a case opens. Its parent does not exist until an open
    /// prepares it, which is the state a first open meets.
    fn database(&self) -> PathBuf {
        self.root.join("derived").join("trial.sqlite3")
    }
}

impl Drop for Scratch {
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: removing the directory this test made.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The pinned scalar the trial client writes inside the create transaction.
const TRIAL_LABEL: &str = "trial_label";

/// What the trial client answers when the mechanics call the database usable.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Answer {
    Keep,
    Rebuild,
    Refuse,
}

/// A client of the ceremony: one table of its own, one pinned scalar, and an
/// adoption answer a case sets.
struct TrialClient {
    label: &'static str,
    answer: Answer,
    /// How many times the ceremony asked this client to adopt a database.
    adoptions: Cell<usize>,
}

impl TrialClient {
    fn new(answer: Answer) -> Self {
        TrialClient {
            label: "first",
            answer,
            adoptions: Cell::new(0),
        }
    }
}

/// What the trial client refuses with, and the substrate refusal it carries.
#[derive(Debug)]
enum TrialError {
    Db(DbError),
    Refused,
}

impl From<DbError> for TrialError {
    fn from(error: DbError) -> Self {
        TrialError::Db(error)
    }
}

impl Client for TrialClient {
    type Error = TrialError;

    fn record(&self, transaction: &Connection) -> Result<(), TrialError> {
        meta::put_meta(transaction, TRIAL_LABEL, self.label)?;
        Ok(())
    }

    fn adopt(&self, _connection: &Connection, _path: &Path) -> Result<Adoption, TrialError> {
        self.adoptions.set(self.adoptions.get() + 1);
        match self.answer {
            Answer::Keep => Ok(Adoption::Keep),
            Answer::Rebuild => Ok(Adoption::Rebuild {
                detail: "the trial client wants a fresh database".to_string(),
            }),
            Answer::Refuse => Err(TrialError::Refused),
        }
    }
}

/// The trial schema: the `meta` table every database carries, and one table of
/// the client's own.
fn schema() -> Schema {
    schema_at(1, "CREATE TABLE notes (path TEXT PRIMARY KEY)")
}

/// The trial schema at another version, or over another statement list. The
/// ceremony takes the DDL fingerprint over the list, so an edited statement is
/// the only way to move it.
fn schema_at(version: i64, notes: &str) -> Schema {
    let mut statements = meta::statements();
    statements.push(notes.to_string());
    Schema {
        operations: norn_db::schema_operations!("trial schema"),
        version,
        statements,
    }
}

/// The fingerprint a `schema` records, taken the way the ceremony takes it.
fn fingerprint_of(schema: &Schema) -> String {
    norn_db::digest(schema.statements.iter().map(String::as_str))
}

/// Open the trial schema at `path`, answering `answer` where the mechanics ask
/// the client to adopt.
fn open(path: &Path, answer: Answer) -> Result<(Connection, OpenOutcome), TrialError> {
    norn_db::open(path, &schema(), &TrialClient::new(answer))
}

fn label(connection: &Connection) -> Option<String> {
    meta::get_meta(connection, TRIAL_LABEL).expect("reading the client's own key")
}

fn epoch(connection: &Connection) -> String {
    meta::get_meta::<String>(connection, meta::STORE_EPOCH)
        .expect("reading the epoch")
        .expect("a created database mints an epoch")
}

/// A path with nothing at it is created: the statement list runs, the
/// mechanics keys and the client's own key are recorded, and an epoch is
/// minted. The parent directory is prepared on the way.
#[test]
fn a_fresh_path_is_created_with_the_client_s_own_keys() {
    let scratch = Scratch::new("fresh");
    let (connection, outcome) = open(&scratch.database(), Answer::Keep).expect("a first open");

    assert_eq!(outcome, OpenOutcome::Created);
    assert_eq!(label(&connection).as_deref(), Some("first"));
    assert!(!epoch(&connection).is_empty());
    assert_eq!(
        meta::get_meta::<i64>(&connection, meta::STORE_SCHEMA_VERSION).expect("the version"),
        Some(1)
    );
    assert_eq!(
        meta::get_meta::<String>(&connection, meta::DDL_FINGERPRINT).expect("the fingerprint"),
        Some(fingerprint_of(&schema())),
        "a create recorded a fingerprint the statements it ran do not produce"
    );
}

/// A database this build wrote, whose client keeps it, is adopted as it
/// stands — same epoch, same rows.
#[test]
fn a_database_the_client_keeps_is_reused() {
    let scratch = Scratch::new("reuse");
    let database = scratch.database();
    let (connection, _) = open(&database, Answer::Keep).expect("a first open");
    let minted = epoch(&connection);
    connection
        .execute("INSERT INTO notes (path) VALUES ('a.md')", [])
        .expect("a row of the client's own");
    drop(connection);

    let (connection, outcome) = open(&database, Answer::Keep).expect("a second open");
    assert_eq!(outcome, OpenOutcome::Reused);
    assert_eq!(epoch(&connection), minted);
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
        .expect("counting the rows");
    assert_eq!(rows, 1, "the reuse discarded rows it was handed");
}

/// The client's verdict over its own keys is the one the mechanics cannot
/// take: a client that asks for a rebuild gets one, and the reason carries the
/// detail it named.
#[test]
fn a_rebuild_the_client_asks_for_carries_the_client_s_detail() {
    let scratch = Scratch::new("client-rebuild");
    let database = scratch.database();
    let (connection, _) = open(&database, Answer::Keep).expect("a first open");
    let minted = epoch(&connection);
    connection
        .execute("INSERT INTO notes (path) VALUES ('a.md')", [])
        .expect("a row of the client's own");
    drop(connection);

    let (connection, outcome) = open(&database, Answer::Rebuild).expect("a second open");
    match &outcome {
        OpenOutcome::RebuiltFromZero(reason @ RebuildReason::Client { detail }) => {
            assert_eq!(detail, "the trial client wants a fresh database");
            assert_eq!(
                reason.to_string(),
                *detail,
                "the reason displays its detail"
            );
        }
        other => panic!("the open ended as {other:?} rather than rebuilding"),
    }
    assert_ne!(epoch(&connection), minted, "a rebuild mints a new epoch");
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
        .expect("counting the rows");
    assert_eq!(rows, 0, "the rebuilt database holds what it was handed");
}

/// A client that refuses the database it is offered is reported, and nothing
/// is discarded: a refusal is not a verdict that the stored state is wrong.
#[test]
fn a_client_that_refuses_the_database_is_reported() {
    let scratch = Scratch::new("client-refusal");
    let database = scratch.database();
    let (connection, _) = open(&database, Answer::Keep).expect("a first open");
    let minted = epoch(&connection);
    drop(connection);

    let error = open(&database, Answer::Refuse).expect_err("a client that refuses");
    assert!(matches!(error, TrialError::Refused), "{error:?}");

    let (connection, outcome) = open(&database, Answer::Keep).expect("the database is still there");
    assert_eq!(outcome, OpenOutcome::Reused);
    assert_eq!(
        epoch(&connection),
        minted,
        "the refusal discarded a database"
    );
}

/// The client is asked only where the mechanics reach a usable database: a
/// create has no recorded keys to judge.
#[test]
fn the_client_is_asked_to_adopt_only_what_the_mechanics_call_usable() {
    let scratch = Scratch::new("asked-once");
    let database = scratch.database();
    let client = TrialClient::new(Answer::Keep);

    let (connection, _) = norn_db::open(&database, &schema(), &client).expect("a first open");
    assert_eq!(
        client.adoptions.get(),
        0,
        "a create asked the client to adopt"
    );
    drop(connection);

    norn_db::open(&database, &schema(), &client).expect("a second open");
    assert_eq!(client.adoptions.get(), 1);
}

/// A file that is not a database at all is removed and created from zero.
#[test]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging a foreign file out of band.
fn a_file_that_is_not_a_database_is_rebuilt_from_zero() {
    let scratch = Scratch::new("foreign");
    let database = scratch.database();
    norn_db::prepare_parent(&database).expect("the parent directory");
    std::fs::write(&database, b"not a database at all").expect("a foreign file");

    let (connection, outcome) = open(&database, Answer::Keep).expect("an open over a foreign file");
    match &outcome {
        OpenOutcome::RebuiltFromZero(RebuildReason::Damaged { detail }) => {
            assert!(!detail.is_empty(), "the damage was not named");
        }
        other => panic!("the open ended as {other:?} rather than rebuilding"),
    }
    assert_eq!(label(&connection).as_deref(), Some("first"));
}

/// A database holding tables and no `meta` table carries nothing to ask, which
/// is damage rather than a disagreement.
#[test]
fn a_database_with_no_meta_table_is_rebuilt_from_zero() {
    let scratch = Scratch::new("no-meta");
    let database = scratch.database();
    norn_db::prepare_parent(&database).expect("the parent directory");
    match norn_db::connect(&database).expect("connecting") {
        norn_db::Attempt::Connected(connection) => connection
            .execute_batch("CREATE TABLE notes (path TEXT PRIMARY KEY)")
            .expect("a table this build did not write"),
        norn_db::Attempt::Unreadable { detail } => panic!("the file is unreadable: {detail}"),
    }

    let (_, outcome) = open(&database, Answer::Keep).expect("an open over a foreign schema");
    match &outcome {
        OpenOutcome::RebuiltFromZero(RebuildReason::Damaged { detail }) => {
            assert!(detail.contains("`meta` table"), "{detail}");
        }
        other => panic!("the open ended as {other:?} rather than rebuilding"),
    }
}

/// A statement list that moved is a shape this build does not write, and the
/// reason names both fingerprints.
#[test]
fn a_moved_fingerprint_is_rebuilt_from_zero() {
    let scratch = Scratch::new("fingerprint");
    let database = scratch.database();
    let first = fingerprint_of(&schema());
    drop(open(&database, Answer::Keep).expect("a first open"));

    let moved = schema_at(1, "CREATE TABLE notes (path TEXT PRIMARY KEY, body TEXT)");
    let (_, outcome) = norn_db::open(&database, &moved, &TrialClient::new(Answer::Keep))
        .expect("an open under an edited statement list");
    match &outcome {
        OpenOutcome::RebuiltFromZero(RebuildReason::DdlFingerprint { expected, found }) => {
            assert_eq!(*expected, fingerprint_of(&moved));
            assert_eq!(found.as_deref(), Some(first.as_str()));
        }
        other => panic!("the open ended as {other:?} rather than rebuilding"),
    }
}

/// The fingerprint a database records is the digest of the statement list that
/// created it. The ceremony takes it, so a client has no way to record one the
/// statements it ran do not produce.
#[test]
fn the_recorded_fingerprint_is_the_digest_of_the_statements_that_ran() {
    let scratch = Scratch::new("derived-fingerprint");
    let schema = schema_at(1, "CREATE TABLE notes (path TEXT PRIMARY KEY, body TEXT)");

    let (connection, outcome) =
        norn_db::open(&scratch.database(), &schema, &TrialClient::new(Answer::Keep))
            .expect("a first open");
    assert_eq!(outcome, OpenOutcome::Created);
    assert_eq!(
        meta::get_meta::<String>(&connection, meta::DDL_FINGERPRINT).expect("the fingerprint"),
        Some(norn_db::digest(
            schema.statements.iter().map(String::as_str)
        ))
    );
}

/// A pinned version that is not this build's is the typed reason it is, and
/// the mechanics never ask the client about it.
#[test]
fn a_moved_version_is_rebuilt_from_zero() {
    let scratch = Scratch::new("version");
    let database = scratch.database();
    drop(open(&database, Answer::Keep).expect("a first open"));

    let moved = schema_at(7, "CREATE TABLE notes (path TEXT PRIMARY KEY)");
    let client = TrialClient::new(Answer::Keep);
    let (_, outcome) =
        norn_db::open(&database, &moved, &client).expect("an open under a moved version");
    match &outcome {
        OpenOutcome::RebuiltFromZero(RebuildReason::StoreSchemaVersion { expected, found }) => {
            assert_eq!(*expected, 7);
            assert_eq!(*found, Some(1));
        }
        other => panic!("the open ended as {other:?} rather than rebuilding"),
    }
    assert_eq!(client.adoptions.get(), 0);
}

/// The rebuild entry point, reached deliberately rather than through a
/// verdict: the file goes, and what comes back is what a create produces.
#[test]
fn a_deliberate_rebuild_starts_the_database_over() {
    let scratch = Scratch::new("deliberate");
    let database = scratch.database();
    let (connection, _) = open(&database, Answer::Keep).expect("a first open");
    let minted = epoch(&connection);
    connection
        .execute("INSERT INTO notes (path) VALUES ('a.md')", [])
        .expect("a row of the client's own");
    drop(connection);

    let connection = norn_db::rebuild(&database, &schema(), &TrialClient::new(Answer::Keep))
        .expect("a deliberate rebuild");
    assert_ne!(epoch(&connection), minted, "a rebuild mints a new epoch");
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
        .expect("counting the rows");
    assert_eq!(rows, 0);
}

/// A hostile environment is refused rather than resolved, and the refusal
/// crosses into the client's own error: a name SQLite reads as something other
/// than a file is nothing to rebuild.
#[test]
fn an_environment_that_holds_no_database_is_refused_through_the_client_s_error() {
    let error = open(Path::new(":memory:"), Answer::Keep).expect_err("a name that is not a file");
    let TrialError::Db(DbError::Lifecycle { operation, .. }) = &error else {
        panic!("the name was refused as {error:?} rather than as a lifecycle refusal");
    };
    assert!(operation.contains("opening"), "{operation}");
}
