//! The open ceremony's contract, over a client this suite owns.
//!
//! The clients that run it in the product carry a domain — a store's mode, a
//! sidecar's model — and their suites prove what those mean. What is proven
//! here is the ceremony itself: which verdict each state of the file reaches,
//! which of them the client is asked about, and what a rebuild leaves behind.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use norn_db::rusqlite::Connection;
use norn_db::{Adoption, Client, DbError, OpenOutcome, RebuildReason, Schema, meta};
use norn_testkit::scratch::Scratch as TestkitScratch;

/// A database under a directory that lasts one test.
///
/// The naming and the removal are [`norn_testkit::scratch::Scratch`]'s; what
/// this adds is where the database sits inside the tree.
struct Scratch {
    root: TestkitScratch,
}

impl Scratch {
    fn new(label: &str) -> Self {
        Scratch {
            root: TestkitScratch::new(&format!("norn-db-ceremony-{label}")),
        }
    }

    /// The database a case opens. Its parent does not exist until an open
    /// prepares it, which is the state a first open meets.
    fn database(&self) -> PathBuf {
        self.root.join("derived").join("trial.sqlite3")
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

    let (connection, outcome) = norn_db::open(
        &scratch.database(),
        &schema,
        &TrialClient::new(Answer::Keep),
    )
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

/// A read-only handle answers from the database the writer wrote, and refuses
/// every write. **The open flag is the refusal**: SQLite reports the database
/// read-only on this connection, which is a property of the open that no
/// statement run on it can withdraw.
#[test]
fn a_read_only_connection_answers_reads_and_refuses_writes() {
    let scratch = Scratch::new("read-only");
    let database = scratch.database();
    let (writer, _) = open(&database, Answer::Keep).expect("a first open");
    meta::put_meta(&writer, meta::WRITE_GENERATION, 7_i64).expect("a writer writes");

    let reader = norn_db::connect_read_only(&database).expect("a read-only handle");
    assert!(
        reader
            .is_readonly(norn_db::rusqlite::MAIN_DB)
            .expect("SQLite reports the mode the database is open in"),
        "the read-only open left the database writable"
    );
    assert_eq!(
        meta::get_meta::<i64>(&reader, meta::WRITE_GENERATION).expect("a pinned scalar"),
        Some(7),
        "the read-only handle answers from another database"
    );
    let refused = meta::put_meta(&reader, meta::WRITE_GENERATION, 8_i64)
        .expect_err("a read-only handle refuses a write");
    assert!(
        matches!(refused, DbError::Sql { .. }),
        "a refused write is reported as the statement it was: {refused:?}"
    );
    assert_eq!(
        meta::get_meta::<i64>(&writer, meta::WRITE_GENERATION).expect("a pinned scalar"),
        Some(7),
        "the refused write reached the database"
    );
}

/// A read-only open creates nothing. Without it a reader over a path whose
/// database is gone would answer every read with no rows instead of saying the
/// file is not there.
#[test]
fn a_read_only_open_of_a_path_with_no_database_refuses() {
    let scratch = Scratch::new("read-only-absent");
    let error = norn_db::connect_read_only(&scratch.database())
        .expect_err("a read-only open of a path with no database refuses");
    assert!(
        matches!(error, DbError::Lifecycle { .. }),
        "an absent database is reported as the open it refused: {error:?}"
    );
    assert!(
        norn_db::connect_read_only(&scratch.database()).is_err(),
        "the refused open left a database behind for the next one to adopt"
    );
}

/// **A read-only open reports what it ran against the database**, so a caller
/// that holds a lock across the open can account for what that lock paid for.
///
/// Three opens, three counts, because a count that stood at the same number
/// for all of them would attest nothing: an open that adopted ran the
/// journal-mode read and the store-epoch read; an open refused on the journal
/// mode ran the journal-mode read alone and never reached the epoch; and an
/// open of a path with no database reached neither.
#[test]
fn a_read_only_open_reports_the_statements_it_ran_against_the_database() {
    let scratch = Scratch::new("read-only-statements");
    let database = scratch.database();
    let (writer, _) = open(&database, Answer::Keep).expect("a first open");
    meta::put_meta(&writer, meta::WRITE_GENERATION, 1_i64).expect("a writer writes");

    let adopted = norn_db::open_read_only(&database);
    adopted
        .adopted
        .expect("a read-only open of a live database binds to its file");
    assert_eq!(
        adopted.statements, 2,
        "an open that adopted ran something other than the journal-mode read and the epoch read"
    );

    let absent = Scratch::new("read-only-statements-absent");
    let nothing = norn_db::open_read_only(&absent.database());
    assert!(
        nothing.adopted.is_err(),
        "a read-only open of a path with no database adopted one"
    );
    assert_eq!(
        nothing.statements, 0,
        "an open that never opened a connection ran statements against a database"
    );

    writer
        .pragma_update(None, "journal_mode", "delete")
        .expect("a writer sets its own journal mode");
    drop(writer);
    let refused = norn_db::open_read_only(&database);
    assert!(
        refused.adopted.is_err(),
        "a read-only open adopted a database that is not in write-ahead logging"
    );
    assert_eq!(
        refused.statements, 1,
        "an open refused on the journal mode reported a count other than the read that refused it"
    );
}

/// The snapshot spelling holds one transaction open past the call that opened
/// it, and refuses a second over the same handle: a handle in a snapshot is a
/// handle nothing else begins a transaction on.
#[test]
fn a_snapshot_is_one_transaction_and_ends_where_it_is_closed() {
    let scratch = Scratch::new("snapshot");
    let database = scratch.database();
    let (writer, _) = open(&database, Answer::Keep).expect("a first open");
    meta::put_meta(&writer, meta::WRITE_GENERATION, 1_i64).expect("a writer writes");

    let mut reader = norn_db::Database::adopt(
        norn_db::connect_read_only(&database).expect("a read-only handle"),
        &database,
    )
    .expect("a read-only handle binds to its file");
    reader.open_snapshot().expect("a snapshot opens");
    // The statement that makes the snapshot real, and the reading it takes.
    assert_eq!(
        meta::get_meta::<i64>(reader.connection(), meta::WRITE_GENERATION).expect("a reading"),
        Some(1)
    );
    let refused = reader
        .open_snapshot()
        .expect_err("a second snapshot over one handle refuses");
    assert!(matches!(refused, DbError::Lifecycle { .. }), "{refused:?}");

    // The writer moves on, and the snapshot still answers at the reading it
    // was established at.
    meta::put_meta(&writer, meta::WRITE_GENERATION, 2_i64).expect("a writer writes");
    assert_eq!(
        meta::get_meta::<i64>(reader.connection(), meta::WRITE_GENERATION).expect("a reading"),
        Some(1),
        "the snapshot read a write that landed after it was established"
    );

    reader.close_snapshot().expect("a snapshot ends");
    reader
        .close_snapshot()
        .expect("ending an ended snapshot is no act");
    assert_eq!(
        meta::get_meta::<i64>(reader.connection(), meta::WRITE_GENERATION).expect("a reading"),
        Some(2),
        "the handle is still inside the snapshot it closed"
    );
}

/// **A snapshot is ended by the handle that opened it and by nothing else.**
/// A request is answered from one snapshot, and transaction control composed
/// over the snapshot's connection is what would break that: a `ROLLBACK` or a
/// `COMMIT` run there ends the snapshot and leaves every later statement
/// reading whatever is committed by then, while the reading the request
/// carries still names the generation its snapshot was established at. The
/// authorizer denies both, and admits the handle's own `BEGIN` and `ROLLBACK`
/// for the length of the one statement each runs.
#[test]
fn a_snapshot_refuses_the_transaction_control_that_would_end_it() {
    let scratch = Scratch::new("snapshot-control");
    let database = scratch.database();
    let (writer, _) = open(&database, Answer::Keep).expect("a first open");
    meta::put_meta(&writer, meta::WRITE_GENERATION, 1_i64).expect("a writer writes");

    let mut reader = norn_db::Database::adopt(
        norn_db::connect_read_only(&database).expect("a read-only handle"),
        &database,
    )
    .expect("a read-only handle binds to its file");
    reader.open_snapshot().expect("a snapshot opens");
    assert_eq!(
        meta::get_meta::<i64>(reader.connection(), meta::WRITE_GENERATION).expect("a reading"),
        Some(1),
        "the statement that establishes the snapshot read another database"
    );

    // The writer moves on, so a snapshot that ended here would start reading
    // the new generation and say nothing about it.
    meta::put_meta(&writer, meta::WRITE_GENERATION, 2_i64).expect("a writer writes");
    for control in ["ROLLBACK", "COMMIT", "BEGIN"] {
        let refused = reader
            .connection()
            .execute_batch(control)
            .expect_err("a statement composed over the snapshot controlled its transaction");
        assert!(
            refused.to_string().contains("not authorized"),
            "`{control}` was refused as something other than an authorization: {refused}"
        );
    }
    assert_eq!(
        meta::get_meta::<i64>(reader.connection(), meta::WRITE_GENERATION).expect("a reading"),
        Some(1),
        "the snapshot ended under the refused transaction control"
    );

    // The handle's own control still runs: the snapshot ends where it is
    // closed, and the connection reads the writer's new generation after it.
    reader.close_snapshot().expect("a snapshot ends");
    assert_eq!(
        meta::get_meta::<i64>(reader.connection(), meta::WRITE_GENERATION).expect("a reading"),
        Some(2),
        "the handle is still inside the snapshot it closed"
    );
    reader.open_snapshot().expect("a second snapshot opens");
    reader.close_snapshot().expect("a second snapshot ends");
}

/// **A read-only connection cannot disarm itself.** The open flag is what
/// refuses a write, and the authorizer is what keeps the connection from
/// reaching around it: the pragma that would relax `query_only`, the attach
/// that would reach a writable database, the temporary table that would be
/// writable inside the connection, and the table-valued pragma form that
/// spells a pragma as a table are each refused at statement preparation.
///
/// One pragma stands apart and is asserted here too: `data_version` is read
/// with no value, sets nothing, and is what FTS5 runs on the connection for
/// itself, so it passes while every setting pragma beside it refuses.
#[test]
fn a_read_only_connection_refuses_to_relax_its_own_settings() {
    let scratch = Scratch::new("read-only-sealed");
    let database = scratch.database();
    let (writer, _) = open(&database, Answer::Keep).expect("a first open");
    meta::put_meta(&writer, meta::WRITE_GENERATION, 3_i64).expect("a writer writes");

    let elsewhere = scratch.root.join("derived").join("elsewhere.sqlite3");
    let (second, _) = open(&elsewhere, Answer::Keep).expect("a second database");
    drop(second);

    let reader = norn_db::connect_read_only(&database).expect("a read-only handle");
    for statement in [
        "PRAGMA query_only = 0",
        "PRAGMA foreign_keys = OFF",
        "CREATE TEMP TABLE probe (value INTEGER)",
        "SELECT name FROM pragma_table_info('meta')",
    ] {
        assert!(
            reader.execute_batch(statement).is_err(),
            "a read-only connection ran `{statement}`"
        );
    }

    // The one pragma the connection answers, and it reports rather than sets.
    reader
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .expect("a read-only connection refused the pragma its full-text reads run");
    reader
        .execute_batch(&format!(
            "ATTACH DATABASE '{}' AS writable",
            elsewhere.display()
        ))
        .expect_err("a read-only connection attached a writable database");

    // The refusals above are refusals of those statements alone: the
    // connection still reads.
    assert_eq!(
        meta::get_meta::<i64>(&reader, meta::WRITE_GENERATION).expect("a pinned scalar"),
        Some(3),
        "a sealed connection stopped answering reads"
    );
}

/// **A plan carries every column its statement reads, and taking it leaves the
/// connection as it was.** The reads are SQLite's own report at preparation,
/// so they name what the statement reads wherever in it the read is — a
/// subquery's included. On a sealed connection the seal judges the statement
/// while its plan is taken, so a statement the seal refuses has no plan, and
/// afterwards the seal and the handle's own transaction control stand as they
/// were; a writable connection writes afterwards as it did before.
#[test]
fn an_emitted_plan_carries_the_columns_its_statement_reads() {
    let scratch = Scratch::new("emitted-reads");
    let database = scratch.database();
    let (writer, _) = open(&database, Answer::Keep).expect("a first open");
    meta::put_meta(&writer, meta::WRITE_GENERATION, 1_i64).expect("a writer writes");
    let read = |table: &str, column: &str| norn_db::ColumnRead {
        table: table.to_string(),
        column: column.to_string(),
    };

    let mut reader = norn_db::Database::adopt(
        norn_db::connect_read_only(&database).expect("a read-only handle"),
        &database,
    )
    .expect("a read-only handle binds to its file");
    let plan = reader
        .emitted_plan(meta::META_READ_SQL, [meta::WRITE_GENERATION])
        .expect("the plan of a pinned-scalar read");
    assert_eq!(
        plan.reads,
        [read("meta", "key"), read("meta", "value")]
            .into_iter()
            .collect(),
        "{plan:?}"
    );
    let nested = reader
        .emitted_plan(
            "SELECT 1 WHERE EXISTS (SELECT 1 FROM meta AS m WHERE length(m.value) >= 0)",
            [],
        )
        .expect("the plan of a probe");
    assert_eq!(
        nested.reads,
        [read("meta", "value")].into_iter().collect(),
        "a read inside a subquery went unreported: {nested:?}"
    );
    reader
        .emitted_plan("CREATE TEMP TABLE probe (value INTEGER)", [])
        .expect_err("a statement the seal refuses was explained");

    for statement in ["PRAGMA query_only = 0", "ROLLBACK"] {
        assert!(
            reader.connection().execute_batch(statement).is_err(),
            "taking a plan unsealed the connection: it ran `{statement}`"
        );
    }
    reader
        .open_snapshot()
        .expect("taking a plan disarmed the handle's own transaction control");
    reader.close_snapshot().expect("a snapshot ends");

    let writing = norn_db::Database::adopt(writer, &database).expect("a writer binds to its file");
    let plan = writing
        .emitted_plan(meta::META_READ_SQL, [meta::WRITE_GENERATION])
        .expect("the plan of a pinned-scalar read on a writer");
    assert_eq!(
        plan.reads,
        [read("meta", "key"), read("meta", "value")]
            .into_iter()
            .collect()
    );
    meta::put_meta(writing.connection(), meta::WRITE_GENERATION, 2_i64)
        .expect("taking a plan left the writer unable to write");
}

/// A database that is not in write-ahead logging is refused as an environment
/// fact rather than as damage. The journal mode says how the file is being
/// used, not what its pages hold, and discarding a sound database over it
/// would destroy work to fix nothing.
#[test]
fn a_read_only_open_of_a_database_that_is_not_in_wal_refuses_without_authorizing_a_rebuild() {
    let scratch = Scratch::new("read-only-journal");
    let database = scratch.database();
    let (writer, _) = open(&database, Answer::Keep).expect("a first open");
    writer
        .pragma_update(None, "journal_mode", "delete")
        .expect("a writer sets its own journal mode");
    drop(writer);

    let error = norn_db::connect_read_only(&database)
        .expect_err("a read-only handle opened a database that is not in write-ahead logging");
    let DbError::Lifecycle { message, .. } = &error else {
        panic!("the journal mode was typed as something a rebuild answers: {error:?}");
    };
    assert!(
        message.contains("write-ahead logging"),
        "the refusal names something other than the journal mode: {message}"
    );
}

/// **A connection counts every page its statements ask for, a full-text
/// match's included.** A walk of every row asks for more pages than a seek of
/// one, and a second walk asks for as many as the first although the cache
/// now holds them. A match walks a full-text index's segments inside one
/// virtual-machine step, and a word a thousand rows carry ten times each asks
/// for more pages than a word one row carries.
#[test]
fn a_connection_counts_the_pages_its_statements_ask_for() {
    let scratch = Scratch::new("pages-touched");
    let database = scratch.database();
    let (writer, _) = open(&database, Answer::Keep).expect("a first open");
    writer
        .execute_batch(
            "CREATE VIRTUAL TABLE words USING fts5(body);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000)
             INSERT INTO notes (path) SELECT printf('note-%04d-%s', i, hex(zeroblob(40))) FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000)
             INSERT INTO words (body)
               SELECT printf('w%04d %s', i, replace(hex(zeroblob(10)), '00', 'common ')) FROM n;",
        )
        .expect("a writer fills the trial tables");
    let reader = norn_db::Database::adopt(
        norn_db::connect_read_only(&database).expect("a read-only handle"),
        &database,
    )
    .expect("a read-only handle binds to its file");
    let touched = |sql: &str| {
        let before = reader.pages_touched();
        reader
            .connection()
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .expect("a trial read");
        reader.pages_touched() - before
    };

    let seek = touched("SELECT count(*) FROM notes WHERE path = 'note-0500'");
    let walk = touched("SELECT count(*) FROM notes WHERE path LIKE '%x%'");
    assert!(seek > 0, "a seek asked for no page");
    assert!(
        walk > seek * 4,
        "a walk asked for {walk} pages and a seek for {seek}"
    );
    assert_eq!(
        touched("SELECT count(*) FROM notes WHERE path LIKE '%x%'"),
        walk,
        "a walk over a warm cache asked for another number of pages"
    );

    // The first match on a connection reads the index's own configuration.
    touched("SELECT count(*) FROM words WHERE words MATCH 'w0001'");
    let rare = touched("SELECT count(*) FROM words WHERE words MATCH 'w0500'");
    let common = touched("SELECT count(*) FROM words WHERE words MATCH 'common'");
    assert!(
        common > rare,
        "a word a thousand rows carry asked for {common} pages and a word one row carries for \
         {rare}"
    );
}
