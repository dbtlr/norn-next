//! A key declared `link` is compared by the link its values are, not by their
//! text: an equality, an inequality and a membership part read the key the
//! host's reading hands the store, so two spellings of one link are one value.
//!
//! The reading here is a stand-in for the host's: the store reads no link
//! syntax, so the cases hold only that it compares by whatever key it is
//! handed, which drops a `|alias` and keeps everything else.

use std::sync::Arc;

use crate::common::{Scratch, document, write_documents};
use crate::find::{map, request, string};
use norn_store::{
    ContentModel, FieldDeclaration, Found, FrontmatterValue, LinkKey, PageRefusal, SnapshotReader,
    Store,
};
use norn_wire::{FieldShape, Predicate};

const SCHEMA: &str = "link-schema";

/// A reading that keeps a `[[…]]` link with its alias dropped, and reads
/// anything else as no link.
fn link_key() -> LinkKey {
    LinkKey::new(|raw| {
        let inner = raw.strip_prefix("[[")?.strip_suffix("]]")?;
        Some(format!("[[{}]]", inner.split('|').next()?))
    })
}

fn declared() -> ContentModel {
    ContentModel::under(SCHEMA)
        .declare_field(
            "project",
            FieldDeclaration::link(link_key()).with_shape(Some(FieldShape::Single)),
        )
        .declare_field("related", FieldDeclaration::link(link_key()))
}

/// Four documents holding a `project` link, and one holding a `related` list.
struct Vault {
    _scratch: Scratch,
    _store: Store,
    reader: Arc<SnapshotReader>,
}

impl Vault {
    fn new(label: &str) -> Self {
        let scratch = Scratch::new(label);
        let mut store = scratch.open();
        store
            .begin_request()
            .pin_vault_schema(SCHEMA.as_bytes(), SCHEMA)
            .expect("pinning the schema");
        let declared = declared();
        let held = |path: &str, project: &str| {
            document(path, path, "a body\n")
                .with_frontmatter(Some(map(vec![("project", string(project))])), &declared)
        };
        let related = document("notes/e.md", "hash-e", "a body\n").with_frontmatter(
            Some(map(vec![(
                "related",
                FrontmatterValue::Sequence(vec![string("[[alpha|A]]"), string("[[beta]]")]),
            )])),
            &declared,
        );
        write_documents(
            &mut store.begin_request(),
            &[
                held("notes/a.md", "[[alpha]]"),
                held("notes/b.md", "[[alpha|Alpha]]"),
                held("notes/c.md", "[[alpha#Plan]]"),
                held("notes/d.md", "[[Alpha]]"),
                related,
            ],
        );
        let reader = Arc::new(store.open_reader().reader.expect("a reader"));
        Vault {
            _scratch: scratch,
            _store: store,
            reader,
        }
    }

    fn find(&self, part: Predicate) -> Result<Found, PageRefusal> {
        self.reader
            .try_take()
            .expect("an idle reader")
            .establish()
            .expect("a snapshot")
            .find(&request().with_predicates([part]), &declared())
    }

    fn paths(&self, part: Predicate) -> Vec<String> {
        self.find(part)
            .expect("an answered find")
            .rows
            .into_iter()
            .map(|row| row.path.as_str().to_string())
            .collect()
    }
}

#[test]
fn an_equality_on_a_link_key_matches_every_alias_of_the_link() {
    let vault = Vault::new("find-link-eq");
    let both = ["notes/a.md", "notes/b.md"];
    assert_eq!(
        vault.paths(Predicate::equal_to("project", "[[alpha]]")),
        both
    );
    assert_eq!(
        vault.paths(Predicate::equal_to("project", "[[alpha|Other]]")),
        both
    );
}

#[test]
fn an_equality_on_a_link_key_tells_an_anchor_and_a_case_apart() {
    let vault = Vault::new("find-link-distinct");
    assert_eq!(
        vault.paths(Predicate::equal_to("project", "[[alpha#Plan]]")),
        ["notes/c.md"]
    );
    assert_eq!(
        vault.paths(Predicate::equal_to("project", "[[Alpha]]")),
        ["notes/d.md"]
    );
}

#[test]
fn an_inequality_on_a_link_key_excludes_every_alias_of_the_link() {
    let vault = Vault::new("find-link-ne");
    assert_eq!(
        vault.paths(Predicate::not_equal_to("project", "[[alpha|x]]")),
        ["notes/c.md", "notes/d.md", "notes/e.md"]
    );
}

#[test]
fn a_membership_on_a_link_key_matches_by_key() {
    let vault = Vault::new("find-link-in");
    assert_eq!(
        vault.paths(Predicate::in_any(
            "project",
            ["[[alpha|x]]".to_string(), "[[Alpha]]".to_string()]
        )),
        ["notes/a.md", "notes/b.md", "notes/d.md"]
    );
}

#[test]
fn an_equality_on_a_link_list_matches_any_element_by_key() {
    let vault = Vault::new("find-link-list");
    assert_eq!(
        vault.paths(Predicate::equal_to("related", "[[beta|B]]")),
        ["notes/e.md"]
    );
    assert_eq!(
        vault.paths(Predicate::equal_to("related", "[[alpha]]")),
        ["notes/e.md"]
    );
}

#[test]
fn a_value_that_is_no_link_is_refused_by_an_equality_on_a_link_key() {
    let vault = Vault::new("find-link-unreadable");
    for part in [
        Predicate::equal_to("project", "alpha"),
        Predicate::not_equal_to("project", "alpha"),
        Predicate::in_any("project", ["[[alpha]]".to_string(), "alpha".to_string()]),
    ] {
        assert_eq!(
            vault
                .find(part.clone())
                .expect_err("a bound that is no link"),
            PageRefusal::UnreadableBound {
                key: "project".to_string(),
                value: "alpha".to_string(),
            },
            "{part:?}"
        );
    }
}
