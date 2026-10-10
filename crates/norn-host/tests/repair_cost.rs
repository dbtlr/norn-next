//! **How long a repair of many routes takes to preview**, end to end through
//! `Host::repair`, measured in a release build.
//!
//! A route's link cascade is judged by construction rather than by generating
//! it per route (`planner::repair::declared`'s respell check): one batched
//! read of the links naming every routed document, and one judgment of each
//! document holding a frontmatter link a cascade may respell. The plan's one
//! resolution then writes every cascade. So the preview's cost follows the
//! routes and the links they touch: doubling both doubles it, and one hub
//! linking every moved document — in its body, or in a frontmatter list no
//! rule reads by value — adds its links once, not once per route.
//!
//! This is a clock, so it is the soak lane's (ADR 0004), not a per-PR gate.
//! It records each case's best of three previews and holds 800 routes to
//! within 2.5 times 400 in any build; in a release build — run it with
//! `cargo test --release -p norn-host --test repair_cost -- --ignored --nocapture`
//! — it holds them to the bars the routes were accepted on as well: 400
//! routes within 0.48 s, and 400 routes and a hub within 1.28 s.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::path::Path;
use std::time::{Duration, Instant};

use norn_testkit::process::Sandbox;
use norn_wire::{ApplyMode, ApplyReport, OperationKind, Predicate, RepairParams, VaultAddress};

/// Where every routed document and the hub stand.
const FOLDER: &str = "zz-repair/";

/// One rule routing a task into `tasks/`.
const SCHEMA: &str = "version: 1\nrules:\n  tasks:\n    match: {frontmatter: {type: task}}\n    allowed_paths: {paths: ['zz-repair/tasks/**'], route: 'zz-repair/tasks/'}\n";

/// How one hub links every moved document, where there is one.
#[derive(Clone, Copy, Debug)]
enum Hub {
    /// No hub.
    None,
    /// A body list of wikilinks, one per moved document.
    Body,
    /// A frontmatter list field no rule reads by value, one wikilink per
    /// moved document. A frontmatter block is read up to 16 KiB
    /// (`norn_text::FRONTMATTER_MAX_BYTES`), which holds some 400 such
    /// links, so the list is split across one hub per 400 moved documents:
    /// the links touched still grow as the routes do.
    Frontmatter,
}

/// The best of three previews of the repair of `routes` misplaced tasks, the
/// hub linking them as `hub` says.
fn previewing(routes: usize, hub: Hub) -> Duration {
    let sandbox = Sandbox::new(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("repair-cost-{routes}-{hub:?}"),
    )
    .expect("a sandbox");
    let vault = attach::Vault::generate(&sandbox.work_dir().join("attached"), "tiny");
    std::fs::write(vault.path().join(".norn/schema.yaml"), SCHEMA).expect("the schema");
    let folder = vault.path().join(FOLDER);
    std::fs::create_dir_all(folder.join("loose")).expect("the loose folder");
    let mut listed = String::new();
    for at in 0..routes {
        std::fs::write(
            folder.join(format!("loose/t{at:04}.md")),
            format!(
                "---\ntype: task\n---\n# Task {at}\n\nNext [[{FOLDER}loose/t{:04}]].\n",
                (at + 1) % routes
            ),
        )
        .expect("a task");
        listed.push_str(&format!("[[{FOLDER}loose/t{at:04}]]"));
        listed.push('\n');
    }
    match hub {
        Hub::None => {}
        Hub::Body => {
            let body: String = listed.lines().map(|link| format!("- {link}\n")).collect();
            std::fs::write(
                folder.join("hub.md"),
                format!("---\ntype: hub\n---\n{body}"),
            )
            .expect("the hub");
        }
        Hub::Frontmatter => {
            let links: Vec<&str> = listed.lines().collect();
            for (at, chunk) in links.chunks(400).enumerate() {
                let items: String = chunk
                    .iter()
                    .map(|link| format!("  - \"{link}\"\n"))
                    .collect();
                let name = if at == 0 {
                    "hub.md".to_string()
                } else {
                    format!("hub-{at}.md")
                };
                std::fs::write(
                    folder.join(name),
                    format!("---\ntype: hub\nsee:\n{items}---\n# Hub\n"),
                )
                .expect("a hub");
            }
        }
    }
    let host = vault.host();
    let _lease = attach::attach_and_wait(&host, vault.name());
    let request = RepairParams::new(VaultAddress::name(vault.name().clone()), ApplyMode::Preview)
        .with_predicates([Predicate::path(format!("{FOLDER}**"))])
        .with_limit(u32::try_from(routes).expect("a page") + 1);
    let mut best = Duration::MAX;
    for _ in 0..3 {
        let started = Instant::now();
        let report = host
            .repair(request.clone())
            .expect("the repair is answered")
            .wait()
            .expect("the repair previews")
            .report;
        best = best.min(started.elapsed());
        let ApplyReport::Previewed { plan, .. } = report else {
            panic!("a preview answered {report:?}");
        };
        let moves = plan
            .operations
            .iter()
            .filter(|operation| matches!(operation.kind, OperationKind::MoveDocument { .. }))
            .count();
        assert_eq!(moves, routes, "every route is planned");
        if !matches!(hub, Hub::None) {
            assert!(
                plan.operations
                    .iter()
                    .flat_map(|operation| &operation.cascade)
                    .any(|rewrite| rewrite.path.as_str() == format!("{FOLDER}hub.md")),
                "the hub's links are respelled"
            );
        }
    }
    best
}

/// **A repair of many routes previews in time linear in its routes and the
/// links they touch**: 400 routes, 400 routes with a hub linking every one
/// from its body and from a frontmatter list, and the same at 800.
#[test]
#[ignore = "soak-lane case: a clock of a repair's routes, its absolute bars read in a release build"]
fn a_repair_of_routes_previews_in_time_linear_in_routes_and_links_touched() {
    let mut readings = Vec::new();
    for routes in [400, 800] {
        for hub in [Hub::None, Hub::Body, Hub::Frontmatter] {
            readings.push(((routes, format!("{hub:?}")), previewing(routes, hub)));
        }
    }
    let at = |routes: usize, hub: &str| {
        readings
            .iter()
            .find(|((at, named), _)| *at == routes && named == hub)
            .map(|(_, took)| *took)
            .expect("a reading")
    };
    let hubs = ["None", "Body", "Frontmatter"];
    let mut recorded: Vec<(String, String)> = readings
        .iter()
        .map(|((routes, hub), took)| (format!("{routes} routes, hub {hub}"), format!("{took:?}")))
        .collect();
    recorded.extend(hubs.iter().map(|hub| {
        let ratio = at(800, hub).as_secs_f64() / at(400, hub).as_secs_f64();
        (format!("800 over 400, hub {hub}"), format!("{ratio:.2}"))
    }));
    norn_testkit::readings::record(
        "a repair's preview of many routes, best of three",
        &recorded
            .iter()
            .map(|(label, value)| (label.as_str(), value.clone()))
            .collect::<Vec<_>>(),
    );
    for hub in hubs {
        let ratio = at(800, hub).as_secs_f64() / at(400, hub).as_secs_f64();
        assert!(ratio <= 2.5, "{hub} grows faster than linear: {readings:?}");
    }
    // A debug build's clock says nothing of the absolute bars, which are
    // read in a release build only.
    if cfg!(debug_assertions) {
        return;
    }
    assert!(
        at(400, "None") <= Duration::from_millis(480),
        "{readings:?}"
    );
    for hub in ["Body", "Frontmatter"] {
        assert!(at(400, hub) <= Duration::from_millis(1280), "{readings:?}");
    }
}
