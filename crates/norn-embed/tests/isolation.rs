//! Heavy-dependency isolation, asserted against the manifest.
//!
//! Half of why this crate is a crate is that a model runtime never enters a
//! development build. The dependency allowlist in `docs/architecture.md` says
//! nothing about that: its subject is workspace edges, and a model runtime is
//! a registry crate. So the rule is read here, off this crate's own manifest,
//! at the only place it can enter.
//!
//! **The rule is that a default build of `norn-embed` links nothing.** A
//! runtime arrives as an `optional = true` dependency behind the release/soak
//! feature, and a default feature set that turned one on would defeat the
//! whole arrangement — so both halves are asserted. Development dependencies
//! are outside it: they are absent from every build that is not a test run.
//!
//! The absent-edge half of the blind-crate invariant — no `embed -> store` and
//! no `embed -> fs`, direct or composed — is the architecture gate's, in
//! `norn-testkit`. What is asserted here is that no dependency reaches a local
//! crate at all, which fails at this crate's own boundary rather than at the
//! workspace's.

const MANIFEST: &str = include_str!("../Cargo.toml");

fn manifest() -> toml::Table {
    MANIFEST.parse().expect("the manifest is TOML")
}

/// Every table of dependencies that a non-test build resolves, named by where
/// it sits — `[dependencies]` and the per-target tables under `[target]`.
fn shipped_dependency_tables(manifest: &toml::Table) -> Vec<(String, toml::Table)> {
    let mut tables = Vec::new();

    for kind in ["dependencies", "build-dependencies"] {
        if let Some(table) = manifest.get(kind).and_then(toml::Value::as_table) {
            tables.push((format!("[{kind}]"), table.clone()));
        }
    }

    let targets = manifest.get("target").and_then(toml::Value::as_table);
    for (target, section) in targets.into_iter().flatten() {
        let Some(section) = section.as_table() else {
            continue;
        };
        for kind in ["dependencies", "build-dependencies"] {
            if let Some(table) = section.get(kind).and_then(toml::Value::as_table) {
                tables.push((format!("[target.{target}.{kind}]"), table.clone()));
            }
        }
    }

    tables
}

#[test]
fn a_default_build_of_this_crate_links_no_third_party_code() {
    for (where_it_sits, table) in shipped_dependency_tables(&manifest()) {
        for (name, spec) in &table {
            let optional = spec
                .as_table()
                .and_then(|spec| spec.get("optional"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            assert!(
                optional,
                "`{name}` in {where_it_sits} is not `optional = true`, so a \
                 default build of norn-embed links it. A model runtime belongs \
                 behind the release/soak feature: this crate exists so that no \
                 development build pays for one."
            );
        }
    }
}

#[test]
fn no_feature_a_default_build_turns_on_pulls_a_dependency_in() {
    let manifest = manifest();
    let Some(features) = manifest.get("features").and_then(toml::Value::as_table) else {
        return;
    };
    let Some(default) = features.get("default") else {
        return;
    };
    let enabled = default.as_array().expect("`default` is a list of features");
    assert!(
        enabled.is_empty(),
        "the default feature set turns on {enabled:?}. An optional dependency \
         reached through a default feature is a dependency of a default build."
    );
}

#[test]
fn no_dependency_of_this_crate_is_a_local_crate() {
    for (where_it_sits, table) in shipped_dependency_tables(&manifest()) {
        for (name, spec) in &table {
            let local = spec
                .as_table()
                .is_some_and(|spec| spec.contains_key("path"));
            assert!(
                !local,
                "`{name}` in {where_it_sits} names a local crate. norn-embed is \
                 blind: it reaches no workspace crate, which is what keeps \
                 inference structurally unable to touch findings or plans."
            );
        }
    }
}
