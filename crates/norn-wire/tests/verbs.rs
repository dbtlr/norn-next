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
//! The table holds all fifteen verbs the registry declares, and the suite
//! holds the table equal to [`Verb::ALL`]: a verb minted without a row here is
//! a verb no surface can render, and a row here naming a verb the registry
//! does not hold spells a request nobody can make.
//!
//! One verb carries its vault somewhere other than its params' own `vault`:
//! `apply` sends a plan, and a plan names its vault by address, so every plan
//! document the params can hold is where its addressing is checked. Each row
//! says where its vault sits, and the checks read that place.

use norn_wire::{
    Addressing, ApplyParams, ApplyReport, CountParams, CountReport, DescribeParams, DescribeReport,
    DoctorRegistryParams, DoctorRegistryReport, FindParams, FindReport, GetParams, GetReport,
    ListParams, ListReport, RegisterParams, RegisterReport, ReloadParams, ReloadReport,
    ResolveParams, ResolveReport, SearchParams, SearchReport, SetParams, SetReport, StatusParams,
    StatusReport, UnregisterParams, UnregisterReport, ValidateParams, ValidateReport, Verb,
};
use serde_json::Value;
use std::collections::BTreeSet;

fn schema_of<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("a schema as JSON")
}

/// One verb, the two types that spell it named by hand, and the schemas those
/// two types advertise.
///
/// The names are written out beside the schemas so that a row spelling a verb
/// by the wrong type fails: a schema advertises its own `title`, and the suite
/// holds the title against the name the row claims. Without that, a table that
/// spelled `vault_set` by `ResolveParams` would still pass every other check
/// here, because both are params types that derive a schema and carry no
/// vault.
///
/// A paged report is an alias for `Page` over the row it carries, so `Page` is
/// the title it advertises, and `Page` advertises that one title whatever its
/// row is. Those rows name the row type as well: the title alone would hold
/// `find` and `count` equal. A search report holds its page beside the ladder
/// that ranked it, so it is a type of its own rather than a page.
struct Spelling {
    verb: Verb,
    /// Where the request's vault address sits.
    vault_at: VaultAt,
    params_type: &'static str,
    report_type: &'static str,
    /// The row a paged report carries, and `None` where the report is not a
    /// page.
    report_rows: Option<&'static str>,
    params: Value,
    report: Value,
}

/// Where a request's vault address sits.
#[derive(Clone, Copy)]
enum VaultAt {
    /// On the params themselves, as their `vault` field.
    Params,
    /// On every plan document the params' `plan` field can hold.
    Plan,
}

/// Every verb the registry holds, paired with the two types that spell it.
fn verb_table() -> Vec<Spelling> {
    vec![
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::Find,
            params_type: "FindParams",
            report_type: "Page",
            report_rows: Some("DocumentRow"),
            params: schema_of::<FindParams>(),
            report: schema_of::<FindReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::Search,
            params_type: "SearchParams",
            report_type: "SearchReport",
            report_rows: None,
            params: schema_of::<SearchParams>(),
            report: schema_of::<SearchReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::Get,
            params_type: "GetParams",
            report_type: "GetReport",
            report_rows: None,
            params: schema_of::<GetParams>(),
            report: schema_of::<GetReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::Count,
            params_type: "CountParams",
            report_type: "Page",
            report_rows: Some("Tally"),
            params: schema_of::<CountParams>(),
            report: schema_of::<CountReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::Validate,
            params_type: "ValidateParams",
            report_type: "ValidateReport",
            report_rows: None,
            params: schema_of::<ValidateParams>(),
            report: schema_of::<ValidateReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::Describe,
            params_type: "DescribeParams",
            report_type: "Page",
            report_rows: Some("Facet"),
            params: schema_of::<DescribeParams>(),
            report: schema_of::<DescribeReport>(),
        },
        Spelling {
            vault_at: VaultAt::Plan,
            verb: Verb::Apply,
            params_type: "ApplyParams",
            report_type: "ApplyReport",
            report_rows: None,
            params: schema_of::<ApplyParams>(),
            report: schema_of::<ApplyReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::VaultRegister,
            params_type: "RegisterParams",
            report_type: "RegisterReport",
            report_rows: None,
            params: schema_of::<RegisterParams>(),
            report: schema_of::<RegisterReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::VaultUnregister,
            params_type: "UnregisterParams",
            report_type: "UnregisterReport",
            report_rows: None,
            params: schema_of::<UnregisterParams>(),
            report: schema_of::<UnregisterReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::VaultList,
            params_type: "ListParams",
            report_type: "ListReport",
            report_rows: None,
            params: schema_of::<ListParams>(),
            report: schema_of::<ListReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::VaultSet,
            params_type: "SetParams",
            report_type: "SetReport",
            report_rows: None,
            params: schema_of::<SetParams>(),
            report: schema_of::<SetReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::VaultResolve,
            params_type: "ResolveParams",
            report_type: "ResolveReport",
            report_rows: None,
            params: schema_of::<ResolveParams>(),
            report: schema_of::<ResolveReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::VaultStatus,
            params_type: "StatusParams",
            report_type: "StatusReport",
            report_rows: None,
            params: schema_of::<StatusParams>(),
            report: schema_of::<StatusReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::VaultReload,
            params_type: "ReloadParams",
            report_type: "ReloadReport",
            report_rows: None,
            params: schema_of::<ReloadParams>(),
            report: schema_of::<ReloadReport>(),
        },
        Spelling {
            vault_at: VaultAt::Params,
            verb: Verb::DoctorRegistry,
            params_type: "DoctorRegistryParams",
            report_type: "DoctorRegistryReport",
            report_rows: None,
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

/// The schemas a request's vault address is read off: the params themselves,
/// or each plan document their `plan` field can hold.
fn vault_holders(spelling: &Spelling) -> Vec<&Value> {
    match spelling.vault_at {
        VaultAt::Params => vec![&spelling.params],
        VaultAt::Plan => {
            let reference = spelling.params["properties"]["plan"]["$ref"]
                .as_str()
                .unwrap_or_else(|| panic!("{} names no plan type", spelling.verb));
            let name = reference
                .strip_prefix("#/$defs/")
                .unwrap_or_else(|| panic!("{} refers to its plan as {reference}", spelling.verb));
            let documents = spelling.params["$defs"][name]["oneOf"]
                .as_array()
                .unwrap_or_else(|| panic!("{} carries a plan of no kinds", spelling.verb));
            assert!(
                !documents.is_empty(),
                "{} carries a plan of no kinds",
                spelling.verb
            );
            assert!(
                !property_names(&spelling.params).contains("vault"),
                "{} names its vault twice, on its params and on its plan",
                spelling.verb
            );
            documents.iter().collect()
        }
    }
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

/// Each row names the two types its verb is spelled by, and the schema beside
/// the name is that type's own: a row that reached for the wrong type
/// advertises a title the row did not claim.
#[test]
fn every_row_pins_the_two_types_its_verb_is_spelled_by() {
    for spelling in verb_table() {
        for (claimed, schema) in [
            (spelling.params_type, &spelling.params),
            (spelling.report_type, &spelling.report),
        ] {
            assert_eq!(
                schema.get("title").and_then(Value::as_str),
                Some(claimed),
                "{} is spelled by a type that is not {claimed}",
                spelling.verb
            );
        }
        assert_eq!(
            spelling.report["properties"]["rows"]["items"]["$ref"].as_str(),
            spelling
                .report_rows
                .map(|rows| format!("#/$defs/{rows}"))
                .as_deref(),
            "{} pages a row it does not page",
            spelling.verb
        );
    }
}

/// What a verb says about its addressing and what its params type carries are
/// one statement: a required address is a required field, an optional one is a
/// field that may be left out, and a registry verb's params carry no vault at
/// all.
#[test]
fn every_verbs_params_carry_the_vault_its_addressing_says_it_does() {
    let table = verb_table();
    for (spelling, holder) in table.iter().flat_map(|spelling| {
        vault_holders(spelling)
            .into_iter()
            .map(move |holder| (spelling, holder))
    }) {
        let properties = property_names(holder);
        let required = required_names(holder);
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

/// Every verb that carries a vault names it by the one vault address the
/// vocabulary has, read off the registry's own addressing rather than off a
/// second list. `vault status` and `vault reload` are in that set: a root
/// addresses a throwaway attach, which has no lifecycle to observe and holds
/// no control files to re-read, and refusing one is the host's job rather
/// than a narrower type's.
#[test]
fn every_verb_that_carries_a_vault_names_it_by_the_one_vault_address() {
    for spelling in verb_table() {
        let expected = match spelling.verb.addressing() {
            Addressing::None => None,
            _ => Some("#/$defs/VaultAddress"),
        };
        for holder in vault_holders(&spelling) {
            assert_eq!(
                vault_reference(holder),
                expected,
                "{} names its vault by the wrong type",
                spelling.verb
            );
        }
    }
}
