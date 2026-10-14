//! Repair advisories computed on the repair's snapshot.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)]

mod attach;

use std::path::Path;

use norn_testkit::process::Sandbox;
use norn_wire::{ApplyMode, ApplyReport, RepairParams, VaultAddress};
use serde_json::{Value, json};

fn a_vault_under(
    label: &str,
    schema: &str,
    documents: &[(&str, &str)],
) -> (Sandbox, attach::Vault, attach::ServingHost) {
    let sandbox = Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), label).expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), "tiny");
    for entry in std::fs::read_dir(vault.path()).expect("the fixture tree") {
        let entry = entry.expect("a fixture entry");
        if entry.file_name() == ".norn" {
            continue;
        }
        if entry.file_type().expect("the fixture type").is_dir() {
            std::fs::remove_dir_all(entry.path()).expect("remove the generated folder");
        } else {
            std::fs::remove_file(entry.path()).expect("remove the generated file");
        }
    }
    std::fs::write(vault.path().join(".norn/schema.yaml"), schema).expect("write the schema");
    for (name, text) in documents {
        let at = vault.path().join(name);
        std::fs::create_dir_all(at.parent().expect("a parent")).expect("make the folder");
        std::fs::write(at, text).expect("write a document");
    }
    let host = vault.host();
    (sandbox, vault, host)
}

fn advisories(host: &attach::ServingHost, vault: &attach::Vault) -> Value {
    let _lease = attach::attach_and_wait(host, vault.name());
    let answer = host
        .repair(RepairParams::new(
            VaultAddress::name(vault.name().clone()),
            ApplyMode::Preview,
        ))
        .expect("repair starts")
        .wait()
        .expect("repair answers");
    let ApplyReport::Previewed { plan, .. } = answer.report else {
        panic!("repair previews");
    };
    serde_json::to_value(
        plan.provenance
            .expect("provenance")
            .advisories
            .expect("advisories"),
    )
    .expect("serialize advisories")["computed"]
        .clone()
}

fn item<'a>(advisories: &'a Value, kind: &str, key: &str) -> &'a Value {
    advisories["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["kind"] == kind && item["key"] == key)
        .expect("the advisory")
}

#[test]
fn mixed_types_count_each_document_once_per_type_and_bound_the_sample() {
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-types",
        "version: 1\n",
        &[
            ("zz-advisory/a.md", "---\nzz_kind: [one, one, 1, 1]\n---\n"),
            ("zz-advisory/b.md", "---\nzz_kind: 2\n---\n"),
            ("zz-advisory/c.md", "---\nzz_kind: 1.5\n---\n"),
            ("zz-advisory/d.md", "---\nzz_kind: true\n---\n"),
            ("zz-advisory/e.md", "---\nzz_kind: null\n---\n"),
            ("zz-advisory/f.md", "---\nzz_kind: {inside: value}\n---\n"),
        ],
    );
    let found = advisories(&host, &vault);
    let types = item(&found, "mixed_types", "zz_kind");
    assert_eq!(
        types["types"],
        json!([
            {"type":"boolean","documents":1}, {"type":"integer","documents":2},
            {"type":"map","documents":1}, {"type":"null","documents":1},
            {"type":"real","documents":1}, {"type":"string","documents":1}
        ])
    );
    assert_eq!(
        types["sample"],
        json!({"paths":["zz-advisory/a.md","zz-advisory/b.md","zz-advisory/c.md","zz-advisory/d.md","zz-advisory/e.md"],"total":6})
    );
}

#[test]
fn mixed_shapes_count_documents_in_each_container() {
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-shapes",
        "version: 1\n",
        &[
            ("zz-advisory/a.md", "---\nzz_shape: one\n---\n"),
            ("zz-advisory/b.md", "---\nzz_shape: [one, two]\n---\n"),
            ("zz-advisory/c.md", "---\nzz_shape: {}\n---\n"),
        ],
    );
    let found = advisories(&host, &vault);
    let shapes = item(&found, "mixed_shapes", "zz_shape");
    assert_eq!(
        shapes["shapes"],
        json!([
            {"shape":"scalar","documents":1}, {"shape":"sequence","documents":1},
            {"shape":"map","documents":1}
        ])
    );
    assert_eq!(
        shapes["sample"],
        json!({"paths":["zz-advisory/a.md","zz-advisory/b.md","zz-advisory/c.md"],"total":3})
    );
}

#[test]
fn list_containers_count_as_types_without_a_uniform_scalar_declaration() {
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-list-containers",
        "version: 1\n",
        &[
            (
                "zz-advisory/a.md",
                "---\nzz_n: [{k: v}, {k: w}]\nzz_map: [{k: v}, {k: w}]\n---\n",
            ),
            ("zz-advisory/b.md", "---\nzz_n: [x]\nzz_map: x\n---\n"),
            ("zz-advisory/c.md", "---\nzz_list: [[x], [y], z]\n---\n"),
            ("zz-advisory/d.md", "---\nzz_map: {k: v}\n---\n"),
        ],
    );
    let found = advisories(&host, &vault);
    assert_eq!(
        item(&found, "mixed_types", "zz_n")["types"],
        json!([{"type":"map","documents":1}, {"type":"string","documents":1}])
    );
    assert_eq!(
        item(&found, "mixed_types", "zz_list")["types"],
        json!([{"type":"sequence","documents":1}, {"type":"string","documents":1}])
    );
    assert_eq!(
        item(&found, "mixed_types", "zz_map")["types"],
        json!([{"type":"map","documents":2}, {"type":"string","documents":1}])
    );
    assert!(
        !found["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["kind"] == "undeclared_uniform" && item["key"] == "zz_n")
    );
}

#[test]
fn undeclared_uniform_fields_render_a_declaration_that_removes_the_advisory() {
    let key = "zz: \"owner\"\u{7f}\u{85}\u{fffe}";
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-uniform",
        "version: 1\n",
        &[
            (
                "zz-advisory/a.md",
                "---\n\"zz: \\\"owner\\\"\\u007f\\u0085\\ufffe\": ['yes', 'no']\n---\n",
            ),
            (
                "zz-advisory/b.md",
                "---\n\"zz: \\\"owner\\\"\\u007f\\u0085\\ufffe\": ['yes']\n---\n",
            ),
        ],
    );
    let found = advisories(&host, &vault);
    let uniform = item(&found, "undeclared_uniform", key);
    assert_eq!(uniform["type"], "string");
    assert_eq!(uniform["documents"], 2);
    assert_eq!(
        uniform["sample"],
        json!({"paths":["zz-advisory/a.md","zz-advisory/b.md"],"total":2})
    );
    assert_eq!(
        uniform["yaml"],
        concat!(
            "fields:\n  ? ",
            r#""zz: \"owner\"\u007f\u0085\ufffe""#,
            "\n  : {type: text, shape: list}\n"
        )
    );
    let schema = format!("version: 1\n{}", uniform["yaml"].as_str().expect("yaml"));
    let parsed =
        norn_config::schema::VaultSchema::parse(schema.as_bytes()).expect("the declaration loads");
    assert!(
        parsed.field(key).is_some(),
        "the declaration key round-trips exactly"
    );
    reload(&host, &vault, &schema);
    let next = advisories(&host, &vault);
    assert!(
        !next["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["kind"] == "undeclared_uniform" && item["key"] == key)
    );
}

fn reload(host: &attach::ServingHost, vault: &attach::Vault, schema: &str) {
    std::fs::write(vault.path().join(".norn/schema.yaml"), schema).expect("rewrite schema");
    norn_testkit::wait::wait_until(
        "the schema reload",
        attach::state_budget(attach::READY_LIMIT),
        || match host.reload(vault.name()) {
            Ok(_) => norn_testkit::wait::Observed::Met(()),
            Err(norn_host::ReloadRefusal::Unavailable(state)) => {
                norn_testkit::wait::Observed::pending(format!("{state:?}"))
            }
            Err(refused) => panic!("reload refused: {refused:?}"),
        },
    )
    .expect("reload converges");
}

#[test]
fn a_required_spelling_is_the_target_even_when_another_spelling_is_more_used() {
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-required-target",
        "version: 1\nrules:\n  named:\n    required: {zz_name: null}\n",
        &[
            ("a.md", "---\nZZ Name: one\n---\n"),
            ("b.md", "---\nZZ Name: two\n---\n"),
            ("c.md", "---\nzz_name: three\n---\n"),
        ],
    );
    let found = advisories(&host, &vault);
    let duplicate = found["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["kind"] == "near_duplicate_fields")
        .expect("near duplicates");
    let yaml = "rules:\n  near_duplicate_1:\n    forbidden:\n      ? \"ZZ Name\"\n      : {rename_to: \"zz_name\"}\n";
    assert_eq!(duplicate["yaml"], yaml);
    let schema = format!("version: 1\n{yaml}  named:\n    required: {{zz_name: null}}\n");
    norn_config::schema::VaultSchema::parse(schema.as_bytes()).expect("the rename loads");
    reload(&host, &vault, &schema);
    host.repair(RepairParams::new(
        VaultAddress::name(vault.name().clone()),
        ApplyMode::Apply,
    ))
    .expect("repair starts")
    .wait()
    .expect("repair applies");
    let next = advisories(&host, &vault);
    assert!(
        !next["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["kind"] == "near_duplicate_fields")
    );
}

#[test]
fn a_group_with_multiple_schema_named_spellings_renames_only_between_unnamed_spellings() {
    let fields = "fields:\n  'ZZ Name': {type: text}\n  'AA Empty': {type: text}\n";
    let named = "  named:\n    required: {zz_name: null, aa_empty: null}\n";
    let schema = format!("version: 1\n{fields}rules:\n{named}");
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-multiple-named-targets",
        &schema,
        &[(
            "a.md",
            "---\nZZ Name: one\nzz_name: two\nzz-name: three\nzz.name: four\nAA Empty: old\naa_empty: old\n---\n",
        )],
    );
    let found = advisories(&host, &vault);
    let groups: Vec<_> = found["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "near_duplicate_fields")
        .collect();
    assert_eq!(groups.len(), 2);
    let yaml = "rules:\n  near_duplicate_1:\n    forbidden:\n      ? \"zz.name\"\n      : {rename_to: \"zz-name\"}\n";
    assert_eq!(groups[1]["yaml"], yaml);
    let merged = format!("version: 1\n{fields}{yaml}{named}");
    norn_config::schema::VaultSchema::parse(merged.as_bytes()).expect("the rename loads");
    assert_eq!(groups[0]["yaml"], "rules: {}\n");
    norn_config::schema::VaultSchema::parse(b"version: 1\nrules: {}\n")
        .expect("an empty rule fragment loads");
}

#[test]
fn a_document_holding_two_spellings_keeps_both_and_is_named_by_the_occupied_rename_skip() {
    let text = "---\nZZ Name: one\nzz_name: two\n---\n";
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-coholding-spellings",
        "version: 1\n",
        &[("a.md", text)],
    );
    let found = advisories(&host, &vault);
    let duplicate = found["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["kind"] == "near_duplicate_fields")
        .expect("near duplicates");
    let schema = format!("version: 1\n{}", duplicate["yaml"].as_str().expect("yaml"));
    norn_config::schema::VaultSchema::parse(schema.as_bytes()).expect("the rename loads");
    reload(&host, &vault, &schema);
    let address = VaultAddress::name(vault.name().clone());
    host.repair(RepairParams::new(address.clone(), ApplyMode::Apply))
        .expect("repair starts")
        .wait()
        .expect("repair applies");
    assert_eq!(
        std::fs::read_to_string(vault.path().join("a.md")).expect("the document"),
        text
    );
    let answer = host
        .repair(RepairParams::new(address.clone(), ApplyMode::Preview))
        .expect("the next repair starts")
        .wait()
        .expect("the next repair answers");
    let ApplyReport::Previewed { plan, .. } = answer.report else {
        panic!("the next repair previews");
    };
    assert!(plan.operations.is_empty());
    let skipped = plan.provenance.expect("provenance").skipped;
    assert_eq!(skipped.len(), 1);
    assert_eq!(
        skipped[0].reason,
        norn_wire::SkipReason::RenameOntoOccupiedField
    );
    let answer = host
        .validate(&norn_wire::ValidateParams::new(address))
        .expect("the findings");
    let norn_wire::ValidateReport::Findings { page, .. } = answer.answer.report else {
        panic!("the validate names findings");
    };
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].id, skipped[0].finding);
    assert_eq!(page.rows[0].path.as_str(), "a.md");
}

#[test]
fn near_duplicate_fields_render_renames_that_remove_the_advisory() {
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-spellings",
        "version: 1\nrules:\n  near_duplicate_1: {}\n",
        &[
            ("zz-advisory/a.md", "---\n'ZZ Name': 'quoted: value'\n---\n"),
            ("zz-advisory/b.md", "---\n'ZZ Name': another\n---\n"),
            ("zz-advisory/c.md", "---\nzz_name: third\n---\n"),
            ("zz-advisory/d.md", "---\nzz.name: fourth\n---\n"),
        ],
    );
    let found = advisories(&host, &vault);
    let duplicate = found["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| {
            item["kind"] == "near_duplicate_fields" && item["spellings"][0]["key"] == "ZZ Name"
        })
        .expect("near duplicates");
    assert_eq!(
        duplicate["spellings"],
        json!([{"key":"ZZ Name","documents":2},{"key":"zz.name","documents":1},{"key":"zz_name","documents":1}])
    );
    assert_eq!(
        duplicate["sample"],
        json!({"paths":["zz-advisory/a.md","zz-advisory/b.md","zz-advisory/c.md","zz-advisory/d.md"],"total":4})
    );
    assert_eq!(
        duplicate["yaml"],
        "rules:\n  near_duplicate_2:\n    forbidden:\n      ? \"zz.name\"\n      : {rename_to: \"ZZ Name\"}\n  near_duplicate_3:\n    forbidden:\n      ? \"zz_name\"\n      : {rename_to: \"ZZ Name\"}\n"
    );
    let schema = format!(
        "version: 1\n{}  near_duplicate_1: {{}}\n",
        duplicate["yaml"].as_str().expect("yaml")
    );
    norn_config::schema::VaultSchema::parse(schema.as_bytes()).expect("the renames load");
    reload(&host, &vault, &schema);
    host.repair(RepairParams::new(
        VaultAddress::name(vault.name().clone()),
        ApplyMode::Apply,
    ))
    .expect("repair starts")
    .wait()
    .expect("repair applies");
    let next = advisories(&host, &vault);
    assert!(
        !next["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["kind"] == "near_duplicate_fields"
                && item["spellings"][0]["key"] == "ZZ Name")
    );
}

#[test]
fn a_rule_matching_nothing_accounts_for_excluded_paths_and_captures() {
    let schema = "version: 1\nrules:\n  excluded:\n    match: {frontmatter: {zz_select: [task, issue]}, path: 'zz-advisory/<folder>/*.md'}\n    exclude: {path: ['zz-advisory/**']}\n  selected:\n    match: {frontmatter: {zz_select: [task, issue]}, path: 'zz-advisory/<folder>/*.md'}\n    exclude: {path: ['elsewhere/**']}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-empty-rule",
        schema,
        &[("zz-advisory/group/a.md", "---\nzz_select: task\n---\n")],
    );
    let found = advisories(&host, &vault);
    let hygiene: Vec<&Value> = found["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["hygiene"]["case"] == "rule_matches_nothing")
        .collect();
    assert_eq!(
        hygiene,
        vec![
            &json!({"kind":"schema_hygiene","hygiene":{"case":"rule_matches_nothing","rule":"excluded"}})
        ]
    );
}

#[test]
fn unused_one_of_values_count_synonyms_as_uses_of_their_members() {
    let schema = "version: 1\nrules:\n  statuses:\n    match: {frontmatter: {zz_select: task}}\n    one_of:\n      zz_status: {values: ['done: \"yes\"', someday], synonyms: {'complete: \"yes\"': 'done: \"yes\"'}}\n";
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-unused-value",
        schema,
        &[(
            "zz-advisory/a.md",
            "---\nzz_status: 'complete: \"yes\"'\n---\n",
        )],
    );
    let found = advisories(&host, &vault);
    let unused: Vec<&Value> = found["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["hygiene"]["case"] == "unused_one_of_value")
        .collect();
    assert_eq!(
        unused,
        vec![
            &json!({"kind":"schema_hygiene","hygiene":{"case":"unused_one_of_value","rule":"statuses","field":"zz_status","value":"someday"}})
        ]
    );
}

#[test]
fn empty_combinations_report_only_pairs_that_admit_no_document() {
    let schema = "version: 1\nfields:\n  zz_kind: {type: text, shape: single}\nrules:\n  alpha:\n    match: {frontmatter: {zz_kind: [a, common]}}\n    required: {zz_required: null, zz_status: null}\n    one_of: {zz_status: {values: [todo]}, zz_optional: {values: [left]}}\n    allowed_paths: {paths: ['left/**']}\n  beta:\n    match: {frontmatter: {zz_kind: [b, common]}}\n    forbidden: {zz_required: null}\n    one_of: {zz_status: {values: [done]}, zz_optional: {values: [right]}}\n    allowed_paths: {paths: ['right/**']}\n  gamma:\n    match: {frontmatter: {zz_kind: other}}\n    forbidden: {zz_required: null}\n";
    let (_sandbox, vault, host) = a_vault_under("advisory-combinations", schema, &[]);
    let found = advisories(&host, &vault);
    let conflicts: Vec<&Value> = found["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["hygiene"]["case"] == "empty_combination")
        .collect();
    assert_eq!(
        conflicts,
        vec![
            &json!({"kind":"schema_hygiene","hygiene":{"case":"empty_combination","rules":["alpha","beta"],"conflict":{"of":"required_and_forbidden","field":"zz_required"}}}),
            &json!({"kind":"schema_hygiene","hygiene":{"case":"empty_combination","rules":["alpha","beta"],"conflict":{"of":"empty_one_of","field":"zz_status"}}}),
            &json!({"kind":"schema_hygiene","hygiene":{"case":"empty_combination","rules":["alpha","beta"],"conflict":{"of":"disjoint_placement"}}}),
        ]
    );
}

#[test]
fn two_repairs_over_the_same_vault_are_byte_identical() {
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-determinism",
        "version: 1\n",
        &[
            ("a.md", "---\nZZ Kind: [one, 1]\nzz_kind: true\n---\n"),
            ("b.md", "---\nzz_kind: [false]\n---\n"),
        ],
    );
    let first = serde_json::to_vec(&advisories(&host, &vault)).expect("first bytes");
    let second = serde_json::to_vec(&advisories(&host, &vault)).expect("second bytes");
    assert_eq!(first, second);
}

#[test]
fn a_synonym_written_outside_the_declared_type_counts_as_a_used_member() {
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-written-synonym",
        "version: 1\nfields:\n  zz_n: {type: number, shape: single}\nrules:\n  numbered:\n    one_of:\n      zz_n: {values: ['1', '2'], synonyms: {one: '1'}}\n",
        &[("a.md", "---\nzz_n: one\n---\n")],
    );
    let found = advisories(&host, &vault);
    let unused: Vec<_> = found["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["hygiene"]["case"] == "unused_one_of_value")
        .collect();
    assert_eq!(
        unused,
        vec![
            &json!({"kind":"schema_hygiene","hygiene":{"case":"unused_one_of_value","rule":"numbered","field":"zz_n","value":"2"}})
        ]
    );
}

#[test]
fn the_entry_ceiling_keeps_the_first_sixty_four_items_and_the_exact_total() {
    let text = format!(
        "---\n{}---\n",
        (0..70)
            .map(|at| format!("key{at:02}: [one, 1]\n"))
            .collect::<String>()
    );
    let (_sandbox, vault, host) =
        a_vault_under("advisory-entry-ceiling", "version: 1\n", &[("a.md", &text)]);
    let found = advisories(&host, &vault);
    assert_eq!(found["total"], 70);
    let items = found["items"].as_array().expect("items");
    assert_eq!(items.len(), 64);
    for (at, item) in items.iter().enumerate() {
        assert_eq!(item["kind"], "mixed_types");
        assert_eq!(item["key"], format!("key{at:02}"));
    }
}

#[test]
fn the_byte_ceiling_keeps_the_longest_prefix_and_the_exact_total() {
    let keys: Vec<String> = (0..12)
        .map(|at| format!("key{at:02}{}", "x".repeat(2000)))
        .collect();
    let documents: Vec<(String, String)> = keys
        .iter()
        .enumerate()
        .map(|(at, key)| {
            (
                format!("a{at:02}.md"),
                format!("---\n? {key}\n: value\n---\n"),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = documents
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    let (_sandbox, vault, host) = a_vault_under("advisory-byte-ceiling", "version: 1\n", &borrowed);
    let found = advisories(&host, &vault);
    assert_eq!(found["total"], 12);
    let items = found["items"].as_array().expect("items");
    assert!(items.len() < 12);
    assert!(!items.is_empty());
    for item in items {
        let schema = format!("version: 1\n{}", item["yaml"].as_str().expect("yaml"));
        norn_config::schema::VaultSchema::parse(schema.as_bytes())
            .expect("even the long declaration key loads");
    }
    let bytes = serde_json::to_vec(items).expect("item bytes").len();
    assert!(bytes <= 32 * 1024);
    let next = json!({"kind":"undeclared_uniform","key":keys[items.len()],"type":"string","documents":1,"sample":{"paths":[format!("a{:02}.md",items.len())],"total":1},"yaml":format!("fields:\n  ? \"{}\"\n  : {{type: text, shape: single}}\n",keys[items.len()])});
    assert!(bytes + 1 + serde_json::to_vec(&next).expect("next bytes").len() > 32 * 1024);
    for (at, item) in items.iter().enumerate() {
        assert_eq!(item["key"], keys[at]);
    }
}

#[test]
fn a_byte_truncated_kind_keeps_its_prefix_before_smaller_and_later_items() {
    let keys: Vec<_> = (0..12)
        .map(|at| format!("key{at:02}{}", "x".repeat(2000)))
        .collect();
    let mut documents: Vec<_> = keys
        .iter()
        .enumerate()
        .map(|(at, key)| {
            (
                format!("a{at:02}.md"),
                format!("---\n? {key}\n: value\n---\n"),
            )
        })
        .collect();
    documents.extend([
        ("z1.md".to_string(), "---\nzz_a: small\n---\n".to_string()),
        ("z2.md".to_string(), "---\nzz-a: small\n---\n".to_string()),
    ]);
    let borrowed: Vec<_> = documents
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-prefix-after-byte-limit",
        "version: 1\n",
        &borrowed,
    );
    let found = advisories(&host, &vault);
    let expected: Vec<_> = keys[..7]
        .iter()
        .enumerate()
        .map(|(at, key)| {
            json!({"kind":"undeclared_uniform","key":key,"type":"string","documents":1,"sample":{"paths":[format!("a{at:02}.md")],"total":1},"yaml":format!("fields:\n  ? \"{key}\"\n  : {{type: text, shape: single}}\n")})
        })
        .collect();
    assert_eq!(found["total"], 15);
    assert_eq!(found["items"], json!(expected));
}

#[test]
fn near_duplicates_choose_the_smallest_tied_spelling_and_order_groups_by_first_key() {
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-spelling-order",
        "version: 1\n",
        &[
            ("a.md", "---\nA-b: 1\nab: 2\nAa: 3\naa: 4\n---\n"),
            ("b.md", "---\nZZ B: 5\n---\n"),
            ("c.md", "---\nzz_b: 6\n---\n"),
        ],
    );
    let found = advisories(&host, &vault);
    let groups: Vec<_> = found["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["kind"] == "near_duplicate_fields")
        .collect();
    assert_eq!(
        groups
            .iter()
            .map(|item| &item["spellings"][0]["key"])
            .collect::<Vec<_>>(),
        vec![&json!("A-b"), &json!("Aa"), &json!("ZZ B")]
    );
    assert_eq!(
        groups[2]["yaml"],
        "rules:\n  near_duplicate_3:\n    forbidden:\n      ? \"zz_b\"\n      : {rename_to: \"ZZ B\"}\n"
    );
}

#[test]
fn narrowed_first_batches_omit_advisories_and_continuations_carry_no_slot() {
    let (_sandbox, vault, host) = a_vault_under(
        "advisory-continuation",
        "version: 1\n",
        &[
            ("a.md", "See [[missing-a]].\n"),
            ("b.md", "See [[missing-b]].\n"),
        ],
    );
    let _lease = attach::attach_and_wait(&host, vault.name());
    let address = VaultAddress::name(vault.name().clone());
    let request = RepairParams::new(address, ApplyMode::Preview).with_limit(1);
    let preview = |params| {
        let answer = host
            .repair(params)
            .expect("repair starts")
            .wait()
            .expect("repair answers");
        let ApplyReport::Previewed { plan, .. } = answer.report else {
            panic!("repair previews");
        };
        plan.provenance.expect("provenance")
    };
    let narrowed = preview(
        request
            .clone()
            .with_predicates([norn_wire::Predicate::path("a.md")]),
    );
    assert_eq!(
        serde_json::to_value(narrowed.advisories.expect("omitted")).expect("json"),
        json!({"omitted":"narrowed"})
    );
    let first = preview(request.clone());
    assert!(first.advisories.is_some());
    let next = preview(request.with_after(*first.cursor.expect("the continuation")));
    assert!(next.advisories.is_none());
}
