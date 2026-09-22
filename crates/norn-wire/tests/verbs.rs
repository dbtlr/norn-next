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
//! The table holds all fourteen verbs the registry declares, and the suite
//! holds the table equal to [`Verb::ALL`]: a verb minted without a row here is
//! a verb no surface can render, and a row here naming a verb the registry
//! does not hold spells a request nobody can make.

use norn_wire::{
    Addressing, CountParams, CountReport, DescribeParams, DescribeReport, DoctorRegistryParams,
    DoctorRegistryReport, FindParams, FindReport, GetParams, GetReport, ListParams, ListReport,
    RegisterParams, RegisterReport, ReloadParams, ReloadReport, ResolveParams, ResolveReport,
    SearchParams, SearchReport, SetParams, SetReport, StatusParams, StatusReport, UnregisterParams,
    UnregisterReport, ValidateParams, ValidateReport, Verb,
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

/// Every verb the registry holds, paired with the two types that spell it.
fn verb_table() -> Vec<Spelling> {
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
        Spelling {
            verb: Verb::VaultRegister,
            params: schema_of::<RegisterParams>(),
            report: schema_of::<RegisterReport>(),
        },
        Spelling {
            verb: Verb::VaultUnregister,
            params: schema_of::<UnregisterParams>(),
            report: schema_of::<UnregisterReport>(),
        },
        Spelling {
            verb: Verb::VaultList,
            params: schema_of::<ListParams>(),
            report: schema_of::<ListReport>(),
        },
        Spelling {
            verb: Verb::VaultSet,
            params: schema_of::<SetParams>(),
            report: schema_of::<SetReport>(),
        },
        Spelling {
            verb: Verb::VaultResolve,
            params: schema_of::<ResolveParams>(),
            report: schema_of::<ResolveReport>(),
        },
        Spelling {
            verb: Verb::VaultStatus,
            params: schema_of::<StatusParams>(),
            report: schema_of::<StatusReport>(),
        },
        Spelling {
            verb: Verb::VaultReload,
            params: schema_of::<ReloadParams>(),
            report: schema_of::<ReloadReport>(),
        },
        Spelling {
            verb: Verb::DoctorRegistry,
            params: schema_of::<DoctorRegistryParams>(),
            report: schema_of::<DoctorRegistryReport>(),
        },
    ]
}

/// The property names a params schema advertises.
fn property_names(schema: &Value) -> BTreeSet<&str> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

/// The names a params schema advertises as required. A shape that requires
/// nothing advertises no list at all, which is an empty one.
fn required_names(schema: &Value) -> BTreeSet<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|required| required.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The type a params schema names its vault by: the reference a field carries
/// directly, or the one the nullable half of an optional field carries.
fn vault_reference(params: &Value) -> Option<&str> {
    let vault = params.get("properties")?.get("vault")?;
    if let Some(reference) = vault.get("$ref").and_then(Value::as_str) {
        return Some(reference);
    }
    vault
        .get("anyOf")?
        .as_array()?
        .iter()
        .find_map(|branch| branch.get("$ref").and_then(Value::as_str))
}

/// The six read verbs, read off the registry itself rather than off a second
/// list: the ones that carry a vault address and are not the reload.
fn read_verbs_in_the_registry() -> BTreeSet<&'static str> {
    Verb::ALL
        .into_iter()
        .filter(|verb| verb.addressing() == Addressing::Required && *verb != Verb::VaultReload)
        .map(|verb| verb.as_str())
        .collect()
}

/// Every verb the registry holds is spelled by a params type and a report
/// type, and both of them derive a schema a surface can advertise.
#[test]
fn every_verb_is_spelled_by_a_params_type_and_a_report_type() {
    let spelled: BTreeSet<&str> = verb_table()
        .iter()
        .map(|spelling| spelling.verb.as_str())
        .collect();
    assert_eq!(
        spelled,
        Verb::ALL.into_iter().map(|verb| verb.as_str()).collect(),
        "the verbs spelled here are not the verbs the registry holds"
    );
    assert_eq!(
        spelled.len(),
        verb_table().len(),
        "a verb is spelled twice in the table"
    );
    for spelling in verb_table() {
        for schema in [&spelling.params, &spelling.report] {
            assert!(
                schema.get("$schema").is_some(),
                "{} is spelled by a type that advertises no schema: {schema}",
                spelling.verb
            );
        }
    }
}

/// What a verb says about its addressing and what its params type carries are
/// one statement: a required address is a required field, an optional one is a
/// field that may be left out, and a registry verb's params carry no vault at
/// all.
#[test]
fn every_verbs_params_carry_the_vault_its_addressing_says_it_does() {
    for spelling in verb_table() {
        let properties = property_names(&spelling.params);
        let required = required_names(&spelling.params);
        match spelling.verb.addressing() {
            Addressing::Required => {
                assert!(
                    properties.contains("vault"),
                    "{} carries a vault address and its params name no vault",
                    spelling.verb
                );
                assert!(
                    required.contains("vault"),
                    "{} advertises its vault as optional",
                    spelling.verb
                );
            }
            Addressing::Optional => {
                assert!(
                    properties.contains("vault"),
                    "{} may carry a vault and its params name none",
                    spelling.verb
                );
                assert!(
                    !required.contains("vault"),
                    "{} advertises as required the vault its addressing says is optional",
                    spelling.verb
                );
            }
            Addressing::None => {
                assert!(
                    !properties.contains("vault"),
                    "{} is answered from the registry and its params address a vault",
                    spelling.verb
                );
            }
            // `Addressing` is `#[non_exhaustive]`, so a suite outside the
            // crate cannot match it without an arm for what it does not know.
            // An addressing minted without a rule here fails rather than
            // passing unchecked.
            addressing => panic!("{addressing} says nothing about what a params type carries"),
        }
    }
}

/// A read addresses a vault, and a lifecycle observation names a registration:
/// a root addresses a throwaway attach, which has no lifecycle to observe and
/// holds no control files to re-read.
#[test]
fn a_read_addresses_a_vault_and_a_lifecycle_observation_names_a_registration() {
    for spelling in verb_table() {
        let expected = match spelling.verb {
            verb if read_verbs_in_the_registry().contains(verb.as_str()) => {
                Some("#/$defs/VaultAddress")
            }
            Verb::VaultStatus | Verb::VaultReload => Some("#/$defs/VaultName"),
            _ => None,
        };
        assert_eq!(
            vault_reference(&spelling.params),
            expected,
            "{} names its vault by the wrong type",
            spelling.verb
        );
    }
}
