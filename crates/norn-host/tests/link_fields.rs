//! **A field declared `link`, end to end**: a real vault, a real attachment,
//! and the findings and reads the host answers over documents holding link
//! values.
//!
//! The type judges what a value is, never where it points. A link to nothing
//! is still a link, so link health files its own finding for it and the field
//! declaration files none; a value that is no link at all is the declaration's
//! finding and link health never sees it. The reads compare a link value by
//! the key the schema reads it into, so two aliases of one link are one value.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::path::Path;

use norn_testkit::process::Sandbox;
use norn_wire::{
    DescribeParams, Facet, FacetKind, FieldShape, FieldType, FindParams, FindReport, FindingKind,
    Predicate, ValidateParams, ValidateReport, VaultAddress, VaultName,
};

const SCHEMA: &str = "\
version: 1
fields:
  project: { type: link, shape: single }
  related: { type: link, shape: list }
  area: { type: link }
";

/// The documents the cases add beside the generated tree. `zz-link-target`
/// has a `Plan` heading, so `[[zz-link-target#Plan]]` resolves whole.
const DOCUMENTS: &[(&str, &str)] = &[
    (
        "zz-link-target.md",
        "---\ntitle: target\n---\n# Plan\n\nbody\n",
    ),
    (
        "zz-links/ok.md",
        "---\nproject: \"[[zz-link-target#Plan]]\"\n---\nbody\n",
    ),
    (
        "zz-links/missing.md",
        "---\nproject: \"[[zz-nowhere]]\"\n---\nbody\n",
    ),
    (
        "zz-links/anchorless.md",
        "---\nproject: \"[[zz-link-target#No Such Heading]]\"\n---\nbody\n",
    ),
    ("zz-links/bare.md", "---\nproject: alpha\n---\nbody\n"),
    (
        "zz-links/unquoted.md",
        "---\nproject: [[zz-link-target]]\n---\nbody\n",
    ),
    (
        "zz-links/listed.md",
        "---\nrelated:\n  - \"[[zz-link-target]]\"\n  - plain\n---\nbody\n",
    ),
    (
        "zz-links/plain.md",
        "---\nproject: \"[[zz-link-target]]\"\n---\nbody\n",
    ),
    (
        "zz-links/aliased.md",
        "---\nproject: \"[[zz-link-target|The Target]]\"\n---\nbody\n",
    ),
    (
        "zz-links/elsewhere.md",
        "---\nproject: \"[[zz-link-target#Plan|Plan]]\"\n---\nbody\n",
    ),
];

fn a_link_vault(label: &str) -> (Sandbox, attach::Vault, attach::ServingHost) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), "tiny");
    std::fs::write(vault.path().join(".norn/schema.yaml"), SCHEMA).expect("write the link schema");
    for (path, text) in DOCUMENTS {
        let at = vault.path().join(path);
        std::fs::create_dir_all(at.parent().expect("a document's folder"))
            .expect("create the document's folder");
        std::fs::write(at, text).expect("write the document");
    }
    let host = vault.host();
    (sandbox, vault, host)
}

fn address(name: &VaultName) -> VaultAddress {
    VaultAddress::name(name.clone())
}

/// The `(kind, path, value)` of every finding of `kinds` over the documents
/// the cases wrote under `zz-links/`.
fn findings(
    host: &attach::ServingHost,
    vault: &attach::Vault,
    kinds: &[FindingKind],
) -> Vec<(FindingKind, String, Option<String>)> {
    let answered = host
        .validate(
            &ValidateParams::new(address(vault.name()))
                .with_kinds(kinds.iter().copied())
                .with_limit(1000),
        )
        .expect("an attached vault answers a validate");
    let ValidateReport::Findings { page, .. } = &answered.answer.report else {
        panic!("a validate answered {:?}", answered.answer.report);
    };
    let mut rows: Vec<_> = page
        .rows
        .iter()
        .filter(|row| row.path.as_str().starts_with("zz-links/"))
        .map(|row| {
            (
                row.kind,
                row.path.as_str().to_string(),
                row.value.as_ref().map(|value| value.text().to_string()),
            )
        })
        .collect();
    rows.sort_by(|left, right| {
        (left.0.as_str(), &left.1, &left.2).cmp(&(right.0.as_str(), &right.1, &right.2))
    });
    rows
}

fn paths_of(page: &FindReport) -> Vec<String> {
    page.page
        .rows
        .iter()
        .map(|row| row.path.as_str().to_string())
        .collect()
}

#[test]
fn a_describe_reports_a_link_field_with_its_shape() {
    let (_sandbox, vault, host) = a_link_vault("host-link-describe");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let answered = host
        .describe(
            &DescribeParams::new(address(vault.name())).with_facets([FacetKind::DeclaredField]),
        )
        .expect("an attached vault answers a describe");
    assert_eq!(
        answered.answer.report.rows,
        vec![
            Facet::declared_field("area", FieldType::Link, None),
            Facet::declared_field("project", FieldType::Link, Some(FieldShape::Single)),
            Facet::declared_field("related", FieldType::Link, Some(FieldShape::List)),
        ]
    );
}

#[test]
fn a_link_to_nothing_is_a_broken_link_and_no_type_mismatch() {
    let (_sandbox, vault, host) = a_link_vault("host-link-broken");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let seen = findings(
        &host,
        &vault,
        &[FindingKind::Broken, FindingKind::TypeMismatch],
    );
    let missing: Vec<_> = seen
        .iter()
        .filter(|(_, path, _)| path == "zz-links/missing.md")
        .map(|(kind, _, _)| *kind)
        .collect();
    assert_eq!(missing, [FindingKind::Broken]);
}

#[test]
fn a_link_to_a_missing_heading_is_a_missing_anchor_and_no_type_mismatch() {
    let (_sandbox, vault, host) = a_link_vault("host-link-anchor");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let seen = findings(
        &host,
        &vault,
        &[FindingKind::MissingAnchor, FindingKind::TypeMismatch],
    );
    let anchorless: Vec<_> = seen
        .iter()
        .filter(|(_, path, _)| path == "zz-links/anchorless.md")
        .map(|(kind, _, _)| *kind)
        .collect();
    assert_eq!(anchorless, [FindingKind::MissingAnchor]);
}

#[test]
fn a_bare_name_is_a_type_mismatch_and_no_link_finding() {
    let (_sandbox, vault, host) = a_link_vault("host-link-bare");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let seen = findings(
        &host,
        &vault,
        &[
            FindingKind::TypeMismatch,
            FindingKind::Broken,
            FindingKind::Ambiguous,
            FindingKind::MissingAnchor,
        ],
    );
    let bare: Vec<_> = seen
        .iter()
        .filter(|(_, path, _)| path == "zz-links/bare.md")
        .cloned()
        .collect();
    assert_eq!(
        bare,
        [(
            FindingKind::TypeMismatch,
            "zz-links/bare.md".to_string(),
            Some("alpha".to_string())
        )]
    );
}

#[test]
fn a_resolving_link_with_its_heading_draws_no_finding() {
    let (_sandbox, vault, host) = a_link_vault("host-link-clean");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let seen = findings(
        &host,
        &vault,
        &[
            FindingKind::TypeMismatch,
            FindingKind::Broken,
            FindingKind::Ambiguous,
            FindingKind::MissingAnchor,
        ],
    );
    for clean in ["zz-links/ok.md", "zz-links/plain.md", "zz-links/aliased.md"] {
        assert!(
            seen.iter().all(|(_, path, _)| path != clean),
            "`{clean}` drew {seen:?}"
        );
    }
}

#[test]
fn an_unquoted_wikilink_is_a_shape_mismatch_under_single() {
    let (_sandbox, vault, host) = a_link_vault("host-link-unquoted");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let seen = findings(
        &host,
        &vault,
        &[FindingKind::ShapeMismatch, FindingKind::TypeMismatch],
    );
    let unquoted: Vec<_> = seen
        .iter()
        .filter(|(_, path, _)| path == "zz-links/unquoted.md")
        .map(|(kind, _, _)| *kind)
        .collect();
    assert_eq!(unquoted, [FindingKind::ShapeMismatch]);
}

#[test]
fn a_list_of_links_is_judged_by_element() {
    let (_sandbox, vault, host) = a_link_vault("host-link-list");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let seen = findings(&host, &vault, &[FindingKind::TypeMismatch]);
    let listed: Vec<_> = seen
        .iter()
        .filter(|(_, path, _)| path == "zz-links/listed.md")
        .cloned()
        .collect();
    assert_eq!(
        listed,
        [(
            FindingKind::TypeMismatch,
            "zz-links/listed.md".to_string(),
            Some("plain".to_string())
        )]
    );
}

#[test]
fn a_find_equality_on_a_link_field_compares_by_the_link_key() {
    let (_sandbox, vault, host) = a_link_vault("host-link-find");
    let _lease = attach::attach_and_wait(&host, vault.name());
    let found = |part: Predicate| {
        paths_of(
            &host
                .find(&FindParams::new(address(vault.name())).with_predicates([part]))
                .expect("a find over a link field answers")
                .answer
                .report,
        )
    };
    assert_eq!(
        found(Predicate::equal_to(
            "project",
            "[[zz-link-target|Anything]]"
        )),
        ["zz-links/aliased.md", "zz-links/plain.md"]
    );
    assert_eq!(
        found(Predicate::in_any(
            "project",
            [
                "[[zz-link-target#Plan]]".to_string(),
                "[[zz-nowhere]]".to_string()
            ]
        )),
        [
            "zz-links/elsewhere.md",
            "zz-links/missing.md",
            "zz-links/ok.md"
        ]
    );
}
