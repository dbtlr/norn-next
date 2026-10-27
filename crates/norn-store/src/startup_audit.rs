//! Read-only persisted-state comparison for the startup measurement harness.
//!
//! Surrogate IDs become document paths, link ordinals, rule-set content, or
//! finding content including its children. Generations and derivation timestamps
//! are excluded; authored dates remain. FTS segment layout is physical, so the
//! audit compares vocabulary and per-document token sizes. Those do not prove
//! every term-to-document posting; authored search checks complement this audit.
//! Rows stream through SHA-256; Rust retains one projected row while SQLite
//! orders the rows and groups each finding's children. The substrate owns
//! opening and sealing the read-only snapshot.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use norn_db::rusqlite::{self, Connection, types::ValueRef};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{StoreError, ddl, error};

// Schema 3 is explicit so a new fact cannot silently escape comparison. Hidden
// FTS columns are absent from SELECT *, while findings.position is generated.
const COLUMNS: &[(&str, &str)] = &[
    ("meta", "key value"),
    (
        "migrations",
        "version ddl_fingerprint applied_generation applied_at",
    ),
    (
        "documents",
        "id path suffix_key path_key admitting_segments content_hash byte_length body body_hash body_offset frontmatter frontmatter_projection_hash frontmatter_diagnostic_count generation derived_at",
    ),
    (
        "vault_files",
        "id path suffix_key path_key admitting_segments generation",
    ),
    (
        "links",
        "id document ordinal family embed protocol target title anchor anchor_text anchor_marked block_ref address span_line span_column span_offset field",
    ),
    ("link_keys", "id link document key segments"),
    (
        "blocks",
        "id document ordinal block_id span_line span_column span_offset",
    ),
    (
        "headings",
        "id document ordinal text reading slug level span_line span_column span_offset body_offset inside_container",
    ),
    (
        "document_tags",
        "id document ordinal path name folded_name source span_line span_column span_offset",
    ),
    (
        "document_fields",
        "document key ordinal path container raw value_type typed least_raw least_typed offset_stated folded",
    ),
    ("document_trigrams", "trigram document"),
    ("rule_sets", "id vault_schema_fingerprint rules"),
    (
        "findings",
        "id vault_schema_fingerprint generation kind severity path target span_line span_column span_offset candidates_total message detail ordinal position rule_set value_head value_bytes value_hash",
    ),
    ("finding_classes", "finding class_key"),
    ("finding_paths", "finding path_key"),
    (
        "finding_rules",
        "finding rule vault_schema_fingerprint kind severity path position",
    ),
    ("finding_candidates", "finding rank path suffix"),
    (
        "tombstones",
        "id path suffix_key path_key last_content_hash provenance generation recorded_at",
    ),
    ("content_quarantines", "path content_hash"),
    ("documents_fts", "body"),
    ("documents_fts_config", "k v"),
    ("documents_fts_data", "id block"),
    ("documents_fts_docsize", "id sz"),
    ("documents_fts_idx", "segid term pgno"),
    ("documents_fts_vocab", "term doc cnt"),
];
const FINDING_CHILDREN: &[&str] = &[
    "finding_candidates",
    "finding_classes",
    "finding_paths",
    "finding_rules",
];
const META_FILTER: &str =
    " WHERE key NOT IN ('store_epoch','write_generation','vault_schema_generation')";

/// Audit a cleanly closed worker's store outside measurement clocks.
///
/// Documents are emitted in path order so the controller can check each hash
/// and length against source bytes without retaining document bodies. The
/// callback writes harness evidence only; it must not mutate the audited store.
pub fn audit(
    path: &Path,
    mut emit_document: impl FnMut(&str, &str, u64) -> std::io::Result<()>,
) -> Result<Value, StoreError> {
    let mut database = norn_db::open_read_only(path).adopted?;
    database.open_snapshot()?;
    let connection = database.connection();
    validate_schema(connection)?;
    let mut statement = read(
        connection.prepare("SELECT path, content_hash, byte_length FROM documents WHERE body IS NOT NULL ORDER BY path"),
    )?;
    let mut rows = read(statement.query([]))?;
    while let Some(row) = read(rows.next())? {
        let document: String = read(row.get(0))?;
        let hash: String = read(row.get(1))?;
        let length: u64 = read(row.get(2))?;
        emit_document(&document, &hash, length).map_err(|error| StoreError::Lifecycle {
            operation: "emitting startup audit document evidence",
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    drop(rows);
    drop(statement);

    let mut tables = BTreeMap::new();
    let mut counts = BTreeMap::new();
    for &(table, _) in COLUMNS {
        if matches!(
            table,
            "documents_fts" | "documents_fts_data" | "documents_fts_idx"
        ) {
            continue;
        }
        let (fields, source) = projection(table);
        let order = (1..=fields.len())
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let query = format!("SELECT {} {source} ORDER BY {order}", fields.join(","));
        let (digest, count) = digest_query(connection, &query)?;
        let filter = if table == "meta" { META_FILTER } else { "" };
        let expected: u64 = read(connection.query_row(
            &format!("SELECT count(*) FROM {table}{filter}"),
            [],
            |row| row.get(0),
        ))?;
        if count != expected {
            return Err(damaged(format!(
                "startup audit lost a relationship in {table}"
            )));
        }
        counts.insert(table, count);
        tables.insert(table, json!({"sha256": digest, "rows": count}));
    }
    let mut overall = Sha256::new();
    for (table, value) in &tables {
        hash_row(
            &mut overall,
            &json!([table, value["rows"], value["sha256"]]),
        );
    }
    let epoch: String = read(connection.query_row(
        "SELECT value FROM meta WHERE key='store_epoch'",
        [],
        |row| row.get(0),
    ))?;
    let generation: u64 = read(connection.query_row(
        "SELECT value FROM meta WHERE key='write_generation'",
        [],
        |row| row.get(0),
    ))?;
    // Additional construction diagnostics retain identities and generations;
    // normalized equality alone deliberately cannot establish these properties.
    // Runtime caches and warm latency are not represented by either digest.
    let mut identity = Sha256::new();
    for &(table, _) in COLUMNS {
        if table == "documents_fts" {
            continue;
        }
        let fields: Vec<_> = columns(table)
            .filter(|column| !matches!(*column, "derived_at" | "recorded_at" | "applied_at"))
            .collect();
        let filter = if table == "meta" {
            " WHERE key != 'store_epoch'"
        } else {
            ""
        };
        let order = (1..=fields.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let (digest, rows) = digest_query(
            connection,
            &format!(
                "SELECT {} FROM {table}{filter} ORDER BY {order}",
                fields.join(",")
            ),
        )?;
        hash_row(&mut identity, &json!([table, digest, rows]));
    }
    let (page_map, _) = digest_query(
        connection,
        "SELECT name,path,pageno,pagetype,ncell,payload,unused,mx_payload,pgsize FROM dbstat ORDER BY name,path,pageno",
    )?;
    let construction = document_construction_digest(connection)?;
    let result = json!({"document_construction_sha256": construction, "canonical_identity_sha256": hex(&identity.finalize()), "page_map_sha256": page_map, "semantic_sha256": hex(&overall.finalize()), "tables": tables,
        "counts": counts, "store_epoch": epoch, "write_generation": generation, "schema_version": 3});
    database.close_snapshot()?;
    Ok(result)
}

// Match the host equivalence seam: IDs establish insertion order; consecutive
// equal document generations establish changeset membership. Finding writes
// consume global generations, so their absolute gaps are deliberately absent.
fn document_construction_digest(connection: &Connection) -> Result<String, StoreError> {
    let mut statement =
        read(connection.prepare("SELECT id,path,generation FROM documents ORDER BY id"))?;
    let mut rows = read(statement.query([]))?;
    let mut digest = Sha256::new();
    let mut previous = None;
    let mut group = 0_u64;
    while let Some(row) = read(rows.next())? {
        let id: i64 = read(row.get(0))?;
        let path: String = read(row.get(1))?;
        let generation: u64 = read(row.get(2))?;
        if previous != Some(generation) {
            group += 1;
            previous = Some(generation);
        }
        hash_row(&mut digest, &json!([id, path, group]));
    }
    Ok(hex(&digest.finalize()))
}

fn read<T>(result: rusqlite::Result<T>) -> Result<T, StoreError> {
    result.map_err(|error| error::sql("auditing persisted startup state", error))
}

fn damaged(what: String) -> StoreError {
    StoreError::Damaged { what }
}

fn columns(table: &str) -> impl Iterator<Item = &'static str> {
    COLUMNS
        .iter()
        .find(|(name, _)| *name == table)
        .expect("a declared audit table")
        .1
        .split_whitespace()
}

fn omitted(table: &str, column: &str) -> bool {
    (column == "id" && table != "documents_fts_docsize")
        || matches!(
            column,
            "generation" | "derived_at" | "recorded_at" | "applied_generation" | "applied_at"
        )
}

fn validate_schema(connection: &Connection) -> Result<(), StoreError> {
    let version = norn_db::meta::get_meta::<i64>(connection, "store_schema_version")?;
    let fingerprint = norn_db::meta::get_meta::<String>(connection, "ddl_fingerprint")?;
    let held_digest = norn_db::meta::get_meta::<String>(connection, "schema_digest")?;
    let current_digest = read(norn_db::schema_digest(connection))?;
    if version != Some(3)
        || fingerprint != Some(ddl::fingerprint())
        || held_digest.as_ref() != Some(&current_digest)
    {
        return Err(damaged(
            "startup audit requires the unchanged current store schema 3".into(),
        ));
    }
    let table_count: usize = read(connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table'",
        [],
        |row| row.get(0),
    ))?;
    if table_count != COLUMNS.len() {
        return Err(damaged("startup audit found unexpected tables".into()));
    }
    for &(table, _) in COLUMNS {
        let statement = read(connection.prepare(&format!("SELECT * FROM {table} LIMIT 0")))?;
        if statement.column_names() != columns(table).collect::<Vec<_>>() {
            return Err(damaged(format!(
                "startup audit found unexpected columns in {table}"
            )));
        }
    }
    let orphan: bool = read(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM findings f LEFT JOIN rule_sets r ON r.id=f.rule_set
         WHERE f.rule_set IS NOT NULL AND r.id IS NULL)",
        [],
        |row| row.get(0),
    ))?;
    if orphan {
        return Err(damaged(
            "startup audit found a missing rule-set relationship".into(),
        ));
    }
    Ok(())
}

fn finding_identity() -> String {
    let mut fields = Vec::new();
    for column in columns("findings").filter(|column| !omitted("findings", column)) {
        if column == "rule_set" {
            fields.extend([
                "r.vault_schema_fingerprint".to_string(),
                "r.rules".to_string(),
            ]);
        } else {
            fields.push(format!("f.{column}"));
        }
    }
    // Scalar content need not uniquely identify a document-scoped finding.
    // Include its child groups so redistributing children between otherwise
    // identical findings changes the normalized relationship, too.
    for &table in FINDING_CHILDREN {
        let child = columns(table)
            .filter(|column| *column != "finding")
            .collect::<Vec<_>>()
            .join(",");
        fields.push(format!(
            "(SELECT json_group_array(json_array({child})) FROM
             (SELECT {child} FROM {table} WHERE finding=f.id ORDER BY {child}))"
        ));
    }
    format!("json_array({})", fields.join(","))
}

fn projection(table: &str) -> (Vec<String>, String) {
    let mut fields = Vec::new();
    let mut source = format!("FROM {table} t");
    for column in columns(table).filter(|column| !omitted(table, column)) {
        match column {
            "document" => {
                fields.push("d.path".into());
                source.push_str(" JOIN documents d ON d.id=t.document");
            }
            "id" if table == "documents_fts_docsize" => {
                fields.push("d.path".into());
                source.push_str(" JOIN documents d ON d.id=t.id");
            }
            "link" if table == "link_keys" => {
                fields.extend(["ld.path".into(), "l.ordinal".into()]);
                source.push_str(" JOIN links l ON l.id=t.link AND l.document=t.document JOIN documents ld ON ld.id=l.document");
            }
            "finding" => {
                fields.push(finding_identity());
                source.push_str(
                    " JOIN findings f ON f.id=t.finding LEFT JOIN rule_sets r ON r.id=f.rule_set",
                );
            }
            "rule_set" if table == "findings" => {
                fields.extend(["r.vault_schema_fingerprint".into(), "r.rules".into()]);
                source.push_str(" LEFT JOIN rule_sets r ON r.id=t.rule_set");
            }
            _ => fields.push(format!("t.{column}")),
        }
    }
    if table == "meta" {
        source.push_str(META_FILTER);
    }
    (fields, source)
}

fn digest_query(connection: &Connection, query: &str) -> Result<(String, u64), StoreError> {
    let mut statement = read(connection.prepare(query))?;
    let width = statement.column_count();
    let mut rows = read(statement.query([]))?;
    let mut digest = Sha256::new();
    let mut count = 0;
    while let Some(row) = read(rows.next())? {
        let mut values = Vec::with_capacity(width);
        for column in 0..width {
            values.push(match read(row.get_ref(column))? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(value) => json!(value),
                ValueRef::Real(value) => json!(value),
                ValueRef::Text(value) => json!(
                    std::str::from_utf8(value)
                        .map_err(|_| damaged("startup audit found non-UTF-8 stored text".into()))?
                ),
                ValueRef::Blob(value) => json!({"bytes_hex": hex(value)}),
            });
        }
        hash_row(&mut digest, &Value::Array(values));
        count += 1;
    }
    Ok((hex(&digest.finalize()), count))
}

fn hash_row(digest: &mut Sha256, row: &Value) {
    digest.update(serde_json::to_vec(row).expect("JSON values serialize"));
    digest.update(b"\n");
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("a string");
    }
    output
}

#[cfg(test)]
mod tests {
    use norn_testkit::scratch::Scratch;

    use crate::{DerivationVersion, Store, StoredPathOrder, induced_failure};

    fn fixture(scratch: &Scratch, offset: i64, generation: i64) -> std::path::PathBuf {
        let path = scratch.join("store.sqlite3");
        let mut store = Store::open(
            &path,
            StoredPathOrder::Sensitive,
            DerivationVersion::new(17),
        )
        .expect("a disposable store");
        induced_failure::execute_out_of_band(&mut store, &format!(
            "INSERT INTO documents (id,path,suffix_key,path_key,admitting_segments,content_hash,
                byte_length,body,body_hash,body_offset,frontmatter_diagnostic_count,generation,derived_at)
             VALUES ({a},'a.md','a/','a.md',1,'source-a',6,'alpha\n','body-a',0,0,{generation},{time}),
                    ({b},'b.md','b/','b.md',1,'source-b',5,'beta\n','body-b',0,0,{generation},{time});
             INSERT INTO links(id,document,ordinal,family,embed,target,address)
             VALUES ({a},{a},0,'wikilink',0,'beta','vault'),({b},{b},0,'wikilink',0,'alpha','vault');
             INSERT INTO link_keys(id,link,document,key,segments) VALUES ({a},{a},{a},'beta/',1);
             INSERT INTO rule_sets(id,vault_schema_fingerprint,rules)
             VALUES ({a},'fingerprint','[\"rule-a\"]'),({b},'fingerprint','[\"rule-b\"]');
             INSERT INTO findings(id,vault_schema_fingerprint,generation,kind,severity,path,
                candidates_total,message,ordinal,rule_set)
             VALUES ({a},'fingerprint',{generation},'link/broken','warning','a.md',0,'missing',0,{a}),
                    ({b},'fingerprint',{generation},'link/broken','warning','b.md',0,'missing',0,{b});
             INSERT INTO finding_classes(finding,class_key) VALUES ({a},'missing/');
             INSERT INTO finding_paths(finding,path_key) VALUES ({a},'missing.md');
             INSERT INTO finding_rules(finding,rule,vault_schema_fingerprint,kind,severity,path,position)
             VALUES ({a},'rule-a','fingerprint','link/broken','warning','a.md',0);
             INSERT INTO finding_candidates(finding,rank,path,suffix) VALUES ({a},0,'b.md','b');
             INSERT INTO vault_files(id,path,suffix_key,path_key,admitting_segments,generation)
             VALUES (-{a},'image.png','image.png/','image.png',1,{generation});
             INSERT INTO blocks(id,document,ordinal,block_id) VALUES ({a},{a},0,'block');
             INSERT INTO headings(id,document,ordinal,text,reading,slug,level,
                span_line,span_column,span_offset,body_offset,inside_container)
             VALUES ({a},{a},0,'Title','title','title',1,1,1,0,0,0);
             INSERT INTO document_tags(id,document,ordinal,path,name,folded_name,source)
             VALUES ({a},{a},0,'tag','Tag','tag','body');
             INSERT INTO document_fields(document,key,ordinal,path,raw,value_type,typed,least_raw,least_typed,folded)
             VALUES ({a},'status',1,'status','open','string','open',1,1,'open');
             INSERT INTO document_trigrams(trigram,document) VALUES ('alp',{a});
             INSERT INTO tombstones(id,path,suffix_key,path_key,last_content_hash,provenance,generation,recorded_at)
             VALUES ({a},'gone.md','gone/','gone.md','old','plan-delete',{generation},{time});
             INSERT INTO migrations(version,ddl_fingerprint,applied_generation,applied_at)
             VALUES (2,'older',{generation},{time});
             UPDATE meta SET value={generation} WHERE key='write_generation';",
            a = offset + 1, b = offset + 2, time = generation * 100,
        )).expect("arrange identities and generations through the store seam");
        drop(store);
        path
    }

    #[test]
    fn equivalent_stores_ignore_surrogate_ids_epochs_generations_and_derived_times() {
        let first = Scratch::new("startup-audit-first");
        let second = Scratch::new("startup-audit-second");
        let left_path = fixture(&first, 0, 1);
        let right_path = fixture(&second, 100, 30);
        let mut documents = Vec::new();
        let left = super::audit(&left_path, |path, hash, length| {
            documents.push((path.to_string(), hash.to_string(), length));
            Ok(())
        })
        .expect("audit the closed store");
        let right =
            super::audit(&right_path, |_, _, _| Ok(())).expect("audit another closed store");
        assert_eq!(left["semantic_sha256"], right["semantic_sha256"]);
        assert_eq!(left["tables"], right["tables"]);
        assert_ne!(left["store_epoch"], right["store_epoch"]);
        assert_eq!(left["write_generation"], 1);
        assert_eq!(right["write_generation"], 30);
        assert_eq!(
            documents,
            [
                ("a.md".into(), "source-a".into(), 6),
                ("b.md".into(), "source-b".into(), 5)
            ]
        );
    }

    fn change(path: &std::path::Path, sql: &str) {
        let mut store = Store::open(path, StoredPathOrder::Sensitive, DerivationVersion::new(17))
            .expect("open the disposable store for an arrangement");
        induced_failure::execute_out_of_band(&mut store, sql)
            .expect("arrange through the store seam");
    }

    #[test]
    fn construction_digest_preserves_ids_order_and_document_generation_groups() {
        let first = Scratch::new("startup-construction-first");
        let second = Scratch::new("startup-construction-second");
        let left_path = fixture(&first, 0, 1);
        let right_path = fixture(&second, 0, 30);
        let digest = |path: &std::path::Path| {
            let receipt = super::audit(path, |_, _, _| Ok(())).unwrap();
            receipt["document_construction_sha256"]
                .as_str()
                .expect("a construction digest")
                .to_string()
        };
        let original = digest(&left_path);
        assert_eq!(
            original,
            digest(&right_path),
            "absolute generation gaps are health dependent"
        );
        change(
            &right_path,
            "UPDATE documents SET generation=50 WHERE path='b.md'",
        );
        let split = digest(&right_path);
        assert_ne!(original, split, "changeset membership must participate");
        change(
            &right_path,
            "UPDATE documents SET generation=generation+100",
        );
        change(
            &right_path,
            "UPDATE documents SET generation=generation+400 WHERE path='b.md'",
        );
        assert_eq!(
            split,
            digest(&right_path),
            "shifted gaps preserve membership"
        );
        change(
            &right_path,
            "UPDATE documents SET path='temporary.md' WHERE path='a.md';
            UPDATE documents SET path='a.md' WHERE path='b.md';
            UPDATE documents SET path='b.md' WHERE path='temporary.md';",
        );
        assert_ne!(
            split,
            digest(&right_path),
            "path insertion order must participate"
        );
        let third = Scratch::new("startup-construction-other-ids");
        let other_ids_path = fixture(&third, 100, 30);
        assert_ne!(
            original,
            digest(&other_ids_path),
            "IDs matter even when path order stays the same"
        );
        let before_rename = digest(&right_path);
        change(
            &right_path,
            "UPDATE documents SET path='renamed.md' WHERE path='a.md'",
        );
        assert_ne!(before_rename, digest(&right_path), "paths must participate");
    }

    #[test]
    fn derived_facts_and_their_relationships_participate_in_equivalence() {
        let changes = [
            (
                "documents",
                "UPDATE documents SET body='different' WHERE id=1",
            ),
            ("vault_files", "UPDATE vault_files SET path='different.png'"),
            ("links", "UPDATE links SET target='different' WHERE id=1"),
            ("link_keys", "UPDATE link_keys SET link=2, document=2"),
            ("blocks", "UPDATE blocks SET document=2"),
            ("headings", "UPDATE headings SET slug='different'"),
            (
                "document_tags",
                "UPDATE document_tags SET folded_name='different'",
            ),
            (
                "document_fields",
                "UPDATE document_fields SET typed='different'",
            ),
            (
                "document_trigrams",
                "UPDATE document_trigrams SET document=2",
            ),
            ("rule_sets", "UPDATE rule_sets SET rules='[]' WHERE id=1"),
            ("findings", "UPDATE findings SET rule_set=2 WHERE id=1"),
            ("finding_classes", "UPDATE finding_classes SET finding=2"),
            (
                "finding_paths",
                "UPDATE finding_paths SET path_key='different.md'",
            ),
            ("finding_rules", "UPDATE finding_rules SET severity='error'"),
            (
                "finding_candidates",
                "UPDATE finding_candidates SET path='different.md'",
            ),
            ("tombstones", "UPDATE tombstones SET provenance='observed'"),
            (
                "migrations",
                "UPDATE migrations SET ddl_fingerprint='different'",
            ),
            (
                "meta",
                "UPDATE meta SET value='case-insensitive' WHERE key='path_order'",
            ),
            (
                "content_quarantines",
                "INSERT INTO content_quarantines VALUES ('bad.md','hash')",
            ),
            (
                "documents_fts_vocab",
                "INSERT INTO documents_fts(documents_fts) VALUES ('delete-all')",
            ),
        ];
        for (table, sql) in changes {
            let scratch = Scratch::new("startup-audit-delta");
            let path = fixture(&scratch, 0, 1);
            let before = super::audit(&path, |_, _, _| Ok(())).expect("the original state");
            change(&path, sql);
            let after = super::audit(&path, |_, _, _| Ok(())).expect("the changed state");
            assert_ne!(
                before["tables"][table]["sha256"], after["tables"][table]["sha256"],
                "{table}"
            );
            assert_ne!(
                before["semantic_sha256"], after["semantic_sha256"],
                "{table}"
            );
        }
    }

    #[test]
    fn identical_findings_keep_their_own_child_relationships() {
        let scratch = Scratch::new("startup-audit-children");
        let path = fixture(&scratch, 0, 1);
        change(
            &path,
            "UPDATE findings SET path='a.md',rule_set=1,ordinal=NULL;
            DELETE FROM finding_classes; DELETE FROM finding_paths;
            DELETE FROM finding_rules; DELETE FROM finding_candidates;
            INSERT INTO finding_classes(finding,class_key) VALUES (1,'red/'),(2,'blue/');",
        );
        let before = super::audit(&path, |_, _, _| Ok(())).expect("distinct child groups");
        change(&path, "UPDATE finding_classes SET finding=1");
        let after = super::audit(&path, |_, _, _| Ok(())).expect("one combined group");
        assert_ne!(before["semantic_sha256"], after["semantic_sha256"]);
    }

    #[test]
    fn schema_drift_and_broken_references_are_refused() {
        for sql in [
            "CREATE TABLE future_facts(fact TEXT)",
            "ALTER TABLE links ADD COLUMN future_fact TEXT",
            "UPDATE meta SET value=4 WHERE key='store_schema_version'",
            "PRAGMA foreign_keys=OFF; UPDATE blocks SET document=999",
            "PRAGMA foreign_keys=OFF; UPDATE link_keys SET document=2",
            "PRAGMA foreign_keys=OFF; UPDATE finding_classes SET finding=999",
            "PRAGMA foreign_keys=OFF; UPDATE findings SET rule_set=999",
        ] {
            let scratch = Scratch::new("startup-audit-refusal");
            let path = fixture(&scratch, 0, 1);
            change(&path, sql);
            assert!(super::audit(&path, |_, _, _| Ok(())).is_err(), "{sql}");
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness evidence: a refused read must create no file.
    fn a_missing_store_is_refused_without_creating_a_database() {
        let scratch = Scratch::new("startup-audit-missing");
        let path = scratch.join("missing.sqlite3");
        assert!(super::audit(&path, |_, _, _| Ok(())).is_err());
        assert!(!path.exists());
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness evidence: compare bytes around the public read-only audit.
    fn auditing_a_closed_store_preserves_its_bytes_epoch_and_generation() {
        let scratch = Scratch::new("startup-audit-read-only");
        let path = fixture(&scratch, 0, 1);
        let before = std::fs::read(&path).expect("the closed store's bytes");
        let first = super::audit(&path, |_, _, _| Ok(())).expect("a first read-only audit");
        assert_eq!(
            std::fs::read(&path).expect("the audited store's bytes"),
            before
        );
        let second = super::audit(&path, |_, _, _| Ok(())).expect("a second read-only audit");
        assert_eq!(
            std::fs::read(&path).expect("the audited store's bytes"),
            before
        );
        assert_eq!(first["store_epoch"], second["store_epoch"]);
        assert_eq!(first["write_generation"], 1);
        assert_eq!(second["write_generation"], 1);
        assert_eq!(first["semantic_sha256"], second["semantic_sha256"]);
    }
}
