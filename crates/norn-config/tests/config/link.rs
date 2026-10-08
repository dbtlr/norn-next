//! The `link` field type: which strings read as a link, and the one equality
//! key link values compare by.
//!
//! Every case judges a document a vault could hold against schema bytes a
//! vault author could have written. A value reads as a link only by the
//! wikilink spelling an editor's property recognizes, whole and untrimmed, so
//! each spelling that reads and each that does not is a case of its own.

use norn_config::schema::{Breach, CaseFold, ElementProblem, FieldType, RuleProblem, VaultSchema};
use norn_config::schema::{RuleFinding, TypedValue, VaultSchemaError};
use norn_wire::{AuthoredValue, ValueMap};

fn schema(bytes: &str) -> VaultSchema {
    VaultSchema::parse(bytes.as_bytes()).expect("a schema that reads")
}

/// A schema declaring `project` a link with `shape` (or none).
fn declaring(shape: Option<&str>) -> VaultSchema {
    match shape {
        Some(shape) => schema(&format!(
            "version: 1\nfields:\n  project: {{ type: link, shape: {shape} }}\n"
        )),
        None => schema("version: 1\nfields:\n  project: { type: link }\n"),
    }
}

fn text(value: &str) -> AuthoredValue {
    AuthoredValue::string(value)
}

fn judged(schema: &VaultSchema, value: AuthoredValue) -> Vec<RuleFinding> {
    let frontmatter =
        ValueMap::new([("project".to_string(), value)]).expect("one key, written once");
    schema
        .judge("a.md", &frontmatter, CaseFold::Exact)
        .into_findings()
}

/// The breaches a lone `project` link value draws, with no shape declared.
fn breaches_of(value: &str) -> Vec<Breach> {
    judged(&declaring(None), text(value))
        .iter()
        .map(RuleFinding::breach)
        .collect()
}

fn assert_reads_as_a_link(value: &str) {
    assert_eq!(breaches_of(value), [], "`{value}` reads as a link");
}

fn assert_is_a_type_mismatch(value: &str) {
    assert_eq!(
        breaches_of(value),
        [Breach::TypeMismatch],
        "`{value}` does not read as a link"
    );
}

/// The key `raw` is compared by under a link field, or nothing.
fn key_of(raw: &str) -> Option<String> {
    match FieldType::Link.read(raw) {
        Ok(TypedValue::Link(key)) => Some(key),
        Ok(other) => panic!("a link reads as a link key: {other:?}"),
        Err(_) => None,
    }
}

// ---- declaring the type ----

#[test]
fn a_link_field_is_declared_under_single_list_and_no_shape() {
    for shape in [Some("single"), Some("list"), None] {
        let schema = declaring(shape);
        assert_eq!(
            schema.declared_type("project"),
            FieldType::Link,
            "{shape:?}"
        );
    }
}

#[test]
fn a_link_orders_as_the_text_it_is_written_as() {
    assert!(FieldType::Link.orders_as_text());
    assert_eq!(FieldType::named("link"), Some(FieldType::Link));
    assert_eq!(FieldType::Link.as_str(), "link");
}

// ---- which spellings read as a link ----

#[test]
fn a_plain_wikilink_reads_as_a_link() {
    assert_reads_as_a_link("[[t]]");
}

#[test]
fn a_wikilink_to_a_heading_reads_as_a_link() {
    assert_reads_as_a_link("[[t#Heading]]");
}

#[test]
fn a_wikilink_to_a_block_reads_as_a_link() {
    assert_reads_as_a_link("[[t#^block]]");
}

#[test]
fn a_wikilink_with_an_alias_reads_as_a_link() {
    assert_reads_as_a_link("[[t|alias]]");
}

#[test]
fn a_wikilink_with_a_folder_a_heading_and_an_alias_reads_as_a_link() {
    assert_reads_as_a_link("[[folder/t#H|a]]");
}

#[test]
fn a_wikilink_with_a_protocol_reads_as_a_link() {
    assert_reads_as_a_link("[[vault://t]]");
    assert_reads_as_a_link("[[https://example.com/page|Docs]]");
}

#[test]
fn a_bare_name_is_a_type_mismatch() {
    assert_is_a_type_mismatch("alpha");
}

#[test]
fn a_wikilink_inside_other_text_is_a_type_mismatch() {
    assert_is_a_type_mismatch("see [[x]]");
}

#[test]
fn two_wikilinks_are_a_type_mismatch() {
    assert_is_a_type_mismatch("[[a]] [[b]]");
}

#[test]
fn an_embed_is_a_type_mismatch() {
    assert_is_a_type_mismatch("![[x]]");
}

#[test]
fn a_markdown_link_is_a_type_mismatch() {
    assert_is_a_type_mismatch("[x](x.md)");
}

#[test]
fn an_empty_string_is_a_type_mismatch() {
    assert_is_a_type_mismatch("");
}

#[test]
fn a_leading_space_is_a_type_mismatch() {
    assert_is_a_type_mismatch(" [[x]]");
}

#[test]
fn a_trailing_space_is_a_type_mismatch() {
    assert_is_a_type_mismatch("[[x]] ");
}

#[test]
fn a_number_is_a_type_mismatch() {
    assert_eq!(
        judged(&declaring(None), AuthoredValue::Integer(7))
            .iter()
            .map(RuleFinding::breach)
            .collect::<Vec<_>>(),
        [Breach::TypeMismatch]
    );
}

#[test]
fn a_boolean_is_a_type_mismatch() {
    assert_eq!(
        judged(&declaring(None), AuthoredValue::Bool(true))
            .iter()
            .map(RuleFinding::breach)
            .collect::<Vec<_>>(),
        [Breach::TypeMismatch]
    );
}

// ---- the unquoted `[[x]]`, which YAML reads as a nested list ----

fn nested_list() -> AuthoredValue {
    AuthoredValue::list([AuthoredValue::list([text("x")])])
}

#[test]
fn an_unquoted_wikilink_under_single_is_a_shape_mismatch() {
    let findings = judged(&declaring(Some("single")), nested_list());
    assert_eq!(
        findings.iter().map(RuleFinding::breach).collect::<Vec<_>>(),
        [Breach::ShapeMismatch]
    );
}

#[test]
fn an_unquoted_wikilink_under_list_is_a_type_mismatch_on_its_element() {
    let findings = judged(&declaring(Some("list")), nested_list());
    assert_eq!(
        findings.iter().map(RuleFinding::breach).collect::<Vec<_>>(),
        [Breach::TypeMismatch]
    );
}

#[test]
fn an_unquoted_wikilink_under_no_shape_is_a_type_mismatch_on_its_element() {
    let findings = judged(&declaring(None), nested_list());
    assert_eq!(
        findings.iter().map(RuleFinding::breach).collect::<Vec<_>>(),
        [Breach::TypeMismatch]
    );
}

#[test]
fn a_list_of_links_is_judged_element_by_element() {
    let value = AuthoredValue::list([text("[[a]]"), text("b"), text("[[c|C]]")]);
    let findings = judged(&declaring(Some("list")), value);
    assert_eq!(
        findings
            .iter()
            .map(|finding| (
                finding.breach(),
                finding.value().and_then(|v| v.scalar_text())
            ))
            .collect::<Vec<_>>(),
        [(Breach::TypeMismatch, Some("b".to_string()))]
    );
}

// ---- the equality key ----

#[test]
fn an_alias_is_no_part_of_a_links_key() {
    assert_eq!(key_of("[[alpha]]"), key_of("[[alpha|Alpha]]"));
    assert_eq!(key_of("[[alpha]]").as_deref(), Some("[[alpha]]"));
}

#[test]
fn an_anchor_is_part_of_a_links_key() {
    assert_ne!(key_of("[[alpha#Plan]]"), key_of("[[alpha]]"));
    assert_eq!(
        key_of("[[alpha#Plan|x]]").as_deref(),
        Some("[[alpha#Plan]]")
    );
}

#[test]
fn a_block_reference_is_part_of_a_links_key() {
    assert_ne!(key_of("[[alpha#^b1]]"), key_of("[[alpha]]"));
    assert_ne!(key_of("[[alpha#^b1]]"), key_of("[[alpha#b1]]"));
    assert_eq!(key_of("[[alpha#^b1|x]]").as_deref(), Some("[[alpha#^b1]]"));
}

#[test]
fn a_links_key_is_case_sensitive() {
    assert_ne!(key_of("[[Alpha]]"), key_of("[[alpha]]"));
    assert_ne!(key_of("[[alpha#plan]]"), key_of("[[alpha#Plan]]"));
}

#[test]
fn a_links_key_does_not_resolve_its_target() {
    assert_ne!(key_of("[[alpha]]"), key_of("[[projects/alpha]]"));
    assert_ne!(key_of("[[alpha]]"), key_of("[[alpha.md]]"));
}

#[test]
fn a_links_key_keeps_its_protocol() {
    assert_eq!(key_of("[[vault://t|x]]").as_deref(), Some("[[vault://t]]"));
    assert_ne!(key_of("[[vault://t]]"), key_of("[[t]]"));
}

#[test]
fn padding_inside_the_brackets_is_no_part_of_a_links_key() {
    assert_eq!(key_of("[[ alpha ]]"), key_of("[[alpha]]"));
}

#[test]
fn a_string_that_is_no_link_has_no_key() {
    assert_eq!(key_of("alpha"), None);
    assert_eq!(key_of("![[alpha]]"), None);
}

// ---- the key where a closed set and a finding identity compare ----

const PROJECTS: &str = "version: 1
fields:
  project: { type: link, shape: single }
rules:
  task:
    one_of:
      project: { values: ['[[alpha]]'] }
";

fn not_one_of(value: &str) -> usize {
    judged(&schema(PROJECTS), text(value))
        .iter()
        .filter(|finding| finding.breach() == Breach::NotOneOf)
        .count()
}

#[test]
fn a_closed_set_admits_a_link_by_its_key_whatever_its_alias() {
    assert_eq!(not_one_of("[[alpha]]"), 0);
    assert_eq!(not_one_of("[[alpha|Alpha]]"), 0);
}

#[test]
fn a_closed_set_refuses_a_link_with_another_anchor_case_or_target() {
    for other in [
        "[[alpha#Plan]]",
        "[[Alpha]]",
        "[[projects/alpha]]",
        "[[beta]]",
    ] {
        assert_eq!(not_one_of(other), 1, "`{other}`");
    }
}

#[test]
fn a_closed_set_member_that_is_no_link_is_refused_at_schema_read() {
    let refused = VaultSchema::parse(
        b"version: 1
fields:
  project: { type: link }
rules:
  task:
    one_of:
      project: { values: [alpha] }
",
    )
    .expect_err("a member that is no link");
    match refused {
        VaultSchemaError::Rule {
            problem:
                RuleProblem::Member {
                    value,
                    problem: ElementProblem::NotType(FieldType::Link),
                },
            ..
        } => assert_eq!(value, "alpha"),
        other => panic!("a member refusal: {other}"),
    }
}

#[test]
fn a_closed_set_synonym_reads_a_link_by_its_key() {
    let schema = schema(
        "version: 1
fields:
  project: { type: link }
rules:
  task:
    one_of:
      project: { values: ['[[alpha]]'], synonyms: { '[[a]]': '[[alpha|Alpha]]' } }
",
    );
    assert!(
        judged(&schema, text("[[alpha]]"))
            .iter()
            .all(|finding| finding.breach() != Breach::NotOneOf)
    );
}

#[test]
fn two_spellings_of_one_offending_link_are_one_finding_naming_the_first() {
    let findings = judged(
        &schema(PROJECTS.replace("shape: single", "shape: list").as_str()),
        AuthoredValue::list([
            text("[[beta|First]]"),
            text("[[beta|Second]]"),
            text("[[beta]]"),
        ]),
    );
    assert_eq!(
        findings
            .iter()
            .map(|finding| (
                finding.breach(),
                finding.value().and_then(|v| v.scalar_text())
            ))
            .collect::<Vec<_>>(),
        [(Breach::NotOneOf, Some("[[beta|First]]".to_string()))]
    );
}

#[test]
fn a_findings_identity_is_the_links_key_whatever_its_alias() {
    let identity = |value: &str| {
        let findings = judged(&schema(PROJECTS), text(value));
        findings
            .iter()
            .find(|finding| finding.breach() == Breach::NotOneOf)
            .expect("an offending link")
            .identity()
            .clone()
    };
    assert_eq!(identity("[[beta|One]]"), identity("[[beta|Two]]"));
    assert_ne!(identity("[[beta]]"), identity("[[beta#Plan]]"));
}

// ---- the key where a selector compares ----

#[test]
fn a_selector_on_a_link_field_matches_by_the_links_key() {
    let schema = schema(
        "version: 1
fields:
  project: { type: link }
rules:
  alpha:
    match: { frontmatter: { project: ['[[alpha|Alpha]]'] } }
    required: { owner: }
",
    );
    let rule = schema.rules().next().expect("the one rule");
    let selects = |value: &str| {
        let frontmatter =
            ValueMap::new([("project".to_string(), text(value))]).expect("one key, written once");
        schema.selects(rule, "a.md", &frontmatter, CaseFold::Exact)
    };
    assert!(selects("[[alpha]]"));
    assert!(selects("[[alpha|Other]]"));
    assert!(!selects("[[alpha#Plan]]"));
    assert!(!selects("[[Alpha]]"));
    assert!(
        !selects("alpha"),
        "a value that is no link matches no selector"
    );
}

// ---- the tags carrier cannot be a link ----

#[test]
fn the_tags_carrier_declared_a_link_is_refused_at_schema_read() {
    let refused = VaultSchema::parse(b"version: 1\nfields:\n  tags: { type: link }\n")
        .expect_err("the carrier declared a link");
    match refused {
        VaultSchemaError::Section { at, .. } => assert_eq!(at, "fields.tags.type"),
        other => panic!("a section refusal: {other}"),
    }
}

#[test]
fn another_key_may_be_declared_a_link_beside_the_tags_carrier() {
    schema("version: 1\nfields:\n  tags: { type: tags }\n  project: { type: link }\n");
}

// ---- a key that is a link, and the same link ----

#[test]
fn a_wikilink_naming_nothing_is_a_type_mismatch() {
    for empty in ["[[]]", "[[ ]]", "[[|x]]", "[[ |a]]", "[[ | ]]"] {
        assert_is_a_type_mismatch(empty);
        assert_eq!(key_of(empty), None, "`{empty}`");
    }
}

#[test]
fn a_same_note_anchor_is_a_link() {
    assert_reads_as_a_link("[[#H]]");
    assert_eq!(key_of("[[#H|x]]").as_deref(), Some("[[#H]]"));
    assert_reads_as_a_link("[[#^b]]");
}

#[test]
fn an_empty_anchor_is_no_anchor_in_a_links_key() {
    assert_eq!(key_of("[[a#]]"), key_of("[[a]]"));
    assert_eq!(key_of("[[a#|x]]"), key_of("[[a]]"));
    assert_eq!(key_of("[[vault://a#]]").as_deref(), Some("[[vault://a]]"));
}

#[test]
fn an_empty_block_reference_is_no_block_reference_in_a_links_key() {
    assert_eq!(key_of("[[a#^]]"), key_of("[[a]]"));
}

#[test]
fn a_leading_space_in_an_anchor_stays_in_the_key() {
    assert_eq!(key_of("[[a# H]]").as_deref(), Some("[[a# H]]"));
}

/// **A key is itself a link with the same key**, whatever spelling it came
/// from, so a key written back into a document compares equal to the value it
/// stood for.
#[test]
fn a_links_key_reads_as_a_link_with_the_same_key() {
    for spelling in [
        "[[t]]",
        "[[t#Heading]]",
        "[[t#^block]]",
        "[[t|alias]]",
        "[[folder/t#H|a]]",
        "[[vault://t]]",
        "[[https://example.com/page|Docs]]",
        "[[ alpha ]]",
        "[[#H]]",
        "[[#^b]]",
        "[[a#]]",
        "[[a#^]]",
        "[[a# H]]",
        "[[a#b#c]]",
    ] {
        let key = key_of(spelling).unwrap_or_else(|| panic!("`{spelling}` reads as a link"));
        assert_eq!(
            key_of(&key),
            Some(key.clone()),
            "`{spelling}` keyed `{key}`"
        );
    }
}
