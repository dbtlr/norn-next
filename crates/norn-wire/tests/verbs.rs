//! The registry's verbs against the types that spell them.
//!
//! A verb in the registry with no params type is a verb no surface can render
//! a request for, and a verb with no report type is one no surface can render
//! an answer of. The table below is that pairing, written out by hand because
//! Rust holds no list of types: what the suite checks is that each pair
//! derives a schema, that the verb's addressing agrees with the params type
//! that spells it, and that the vault a request carries is the one vault
//! address the vocabulary has.
//!
//! The table holds the six read verbs. The other eight are the vault-namespace
//! and doctor verbs, whose params and reports land with the layer above; they
//! extend this table rather than starting another.

use norn_wire::{
    Addressing, CountParams, CountReport, DescribeParams, DescribeReport, FindParams, FindReport,
    GetParams, GetReport, SearchParams, SearchReport, ValidateParams, ValidateReport, Verb,
};
use serde_json::Value;
use std::collections::BTreeSet;

fn schema_of<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("a schema as JSON")
}

/// One verb, the schema of what a request for it carries, and the schema of
/// what it answers with.
struct Spelling {
    verb: Verb,
    params: Value,
    report: Value,
}

/// Every read verb, paired with the two types that spell it.
fn read_verbs() -> Vec<Spelling> {
    vec![
        Spelling {
            verb: Verb::Find,
            params: schema_of::<FindParams>(),
            report: schema_of::<FindReport>(),
        },
        Spelling {
            verb: Verb::Search,
            params: schema_of::<SearchParams>(),
            report: schema_of::<SearchReport>(),
        },
        Spelling {
            verb: Verb::Get,
            params: schema_of::<GetParams>(),
            report: schema_of::<GetReport>(),
        },
        Spelling {
            verb: Verb::Count,
            params: schema_of::<CountParams>(),
            report: schema_of::<CountReport>(),
        },
        Spelling {
            verb: Verb::Validate,
            params: schema_of::<ValidateParams>(),
            report: schema_of::<ValidateReport>(),
        },
        Spelling {
            verb: Verb::Describe,
            params: schema_of::<DescribeParams>(),
            report: schema_of::<DescribeReport>(),
        },
    ]
}

/// Which verbs the registry holds that are read verbs, read off the registry
/// itself rather than off a second list: the six that carry a vault address
/// and are not the reload.
fn read_verbs_in_the_registry() -> BTreeSet<&'static str> {
    Verb::ALL
        .into_iter()
        .filter(|verb| verb.addressing() == Addressing::Required && *verb != Verb::VaultReload)
        .map(|verb| verb.as_str())
        .collect()
}

/// Every read verb the registry holds is spelled by a params type and a report
/// type, and both of them derive a schema a surface can advertise.
#[test]
fn every_read_verb_is_spelled_by_a_params_type_and_a_report_type() {
    let spelled: BTreeSet<&str> = read_verbs()
        .iter()
        .map(|spelling| spelling.verb.as_str())
        .collect();
    assert_eq!(
        spelled,
        read_verbs_in_the_registry(),
        "the verbs spelled here are not the read verbs the registry holds"
    );
    for spelling in read_verbs() {
        for schema in [&spelling.params, &spelling.report] {
            assert!(
                schema.get("$schema").is_some(),
                "{} is spelled by a type that advertises no schema: {schema}",
                spelling.verb
            );
        }
    }
}

/// A read carries a vault address, and the params type that spells it carries
/// the one address the vocabulary has — the verb's answer and the type's field
/// say one thing.
#[test]
fn every_read_verbs_params_carry_the_vault_address_its_verb_requires() {
    for spelling in read_verbs() {
        assert_eq!(
            spelling.verb.addressing(),
            Addressing::Required,
            "{} is a read verb that carries no vault address",
            spelling.verb
        );
        let vault = &spelling.params["properties"]["vault"];
        assert_eq!(
            vault["$ref"].as_str(),
            Some("#/$defs/VaultAddress"),
            "{} names its vault by something other than a vault address",
            spelling.verb
        );
        let required: BTreeSet<&str> = spelling.params["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{} advertises no required list", spelling.verb))
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            required.contains("vault"),
            "{} advertises its vault as optional",
            spelling.verb
        );
    }
}
