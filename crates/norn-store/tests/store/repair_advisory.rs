//! Repair-advisory reads tally parsed fields through bounded index searches.

use std::sync::Arc;

use crate::common::{Scratch, document, write_documents};
use crate::find::{map, string};
use norn_store::{ContentModel, FrontmatterValue, RepairAdvisoryPlan, Snapshot, ValidateWork};
use norn_testkit::explain::{Access, PlanRow, QueryPlan};
use norn_wire::{AdvisoryValueType, ContainerKind, ShapeUse, TypeUse};

fn judge(plans: &[RepairAdvisoryPlan]) {
    for emitted in plans {
        let plan = QueryPlan::new(
            emitted.plan.sql.clone(),
            emitted
                .plan
                .steps
                .iter()
                .map(|row| PlanRow {
                    id: row.id,
                    parent: row.parent,
                    detail: row.detail.clone(),
                })
                .collect(),
        );
        plan.assert_no_full_scan_of("document_fields");
        plan.assert_no_temp_btree();
        let index = if emitted.plan.sql.contains("value_type, document") {
            "document_fields_value_type"
        } else {
            "document_fields_presence"
        };
        plan.assert_searches_through("document_fields", Access::Index(index));
    }
}

fn bounded(work: ValidateWork) {
    assert_eq!(work.full_scan_steps, 0);
    assert_eq!(work.sorts, 0);
}

fn with_fields(run: impl FnOnce(&Snapshot)) {
    let scratch = Scratch::new("repair-advisory-reads");
    let mut store = scratch.open();
    let declared = ContentModel::none();
    let facts = vec![
        document("a.md", "a", "body").with_frontmatter(
            Some(map(vec![
                ("Due", string("tomorrow")),
                ("due", FrontmatterValue::Int(2)),
            ])),
            &declared,
        ),
        document("b.md", "b", "body").with_frontmatter(
            Some(map(vec![(
                "due",
                FrontmatterValue::Sequence(vec![
                    FrontmatterValue::Int(3),
                    FrontmatterValue::Int(4),
                    string("soon"),
                ]),
            )])),
            &declared,
        ),
        document("c.md", "c", "body").with_frontmatter(
            Some(map(vec![("due", map(vec![("nested", string("value"))]))])),
            &declared,
        ),
    ];
    write_documents(&mut store.begin_request(), &facts);
    let reader = Arc::new(store.open_reader().reader.expect("a live reader"));
    let snapshot = reader
        .try_take()
        .expect("an available reader")
        .establish()
        .expect("a snapshot");
    run(&snapshot);
}

#[test]
fn a_fields_types_and_shapes_are_tallied_once_per_document_through_searches() {
    with_fields(|snapshot| {
        let first = snapshot.repair_key_after(None).expect("the first key");
        assert_eq!(first.key.as_deref(), Some("Due"));
        bounded(first.work);
        let next = snapshot
            .repair_key_after(Some("Due"))
            .expect("the next key");
        assert_eq!(next.key.as_deref(), Some("due"));
        bounded(next.work);
        assert!(
            snapshot
                .repair_key_after(Some("due"))
                .expect("the last key")
                .key
                .is_none()
        );
        let tally = snapshot.repair_field_tally("due").expect("a tally");
        assert_eq!(tally.documents, 3);
        assert_eq!(tally.value_rows, 4);
        assert_eq!(
            tally.types,
            vec![
                TypeUse::new(AdvisoryValueType::Integer, 2),
                TypeUse::new(AdvisoryValueType::Map, 1),
                TypeUse::new(AdvisoryValueType::String, 1)
            ]
        );
        assert_eq!(
            tally.shapes,
            vec![
                ShapeUse::new(ContainerKind::Scalar, 1),
                ShapeUse::new(ContainerKind::Sequence, 1),
                ShapeUse::new(ContainerKind::Map, 1)
            ]
        );
        assert_eq!(
            tally
                .sample
                .paths()
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.md", "b.md", "c.md"]
        );
        assert_eq!(tally.sample.total(), 3);
        assert_eq!(tally.work.statements, 2);
        bounded(tally.work);
        judge(
            &snapshot
                .repair_key_after_plans(None)
                .expect("the initial seek plan"),
        );
        judge(
            &snapshot
                .repair_key_after_plans(Some("Due"))
                .expect("the next seek plan"),
        );
        judge(
            &snapshot
                .repair_field_tally_plans("due")
                .expect("the tally plans"),
        );
    });
}

#[test]
fn a_repair_sample_has_five_paths_and_an_exact_distinct_total() {
    let scratch = Scratch::new("repair-advisory-samples");
    let mut store = scratch.open();
    let declared = ContentModel::none();
    let facts: Vec<_> = (b'a'..=b'g')
        .map(|letter| {
            let mut entries = vec![("due", string("soon"))];
            if letter % 2 == 1 {
                entries.push(("Due", string("soon")));
            }
            let path = format!("{}.md", char::from(letter));
            document(&path, &path, "body").with_frontmatter(Some(map(entries)), &declared)
        })
        .collect();
    write_documents(&mut store.begin_request(), &facts);
    let reader = Arc::new(store.open_reader().reader.expect("a live reader"));
    let snapshot = reader
        .try_take()
        .expect("a reader")
        .establish()
        .expect("a snapshot");
    let sample = snapshot.repair_field_tally("due").expect("a field tally");
    assert_eq!(
        sample
            .sample
            .paths()
            .iter()
            .map(|path| path.as_str())
            .collect::<Vec<_>>(),
        vec!["a.md", "b.md", "c.md", "d.md", "e.md"]
    );
    assert_eq!(sample.sample.total(), 7);
    assert_eq!(sample.documents, 7);
    assert_eq!(sample.work.statements, 2);
    bounded(sample.work);
    judge(
        &snapshot
            .repair_field_tally_plans("due")
            .expect("the field tally plans"),
    );
    let keys = vec!["Due".to_string(), "due".to_string()];
    let tally = snapshot
        .repair_spelling_tally(&keys)
        .expect("the spelling union tally");
    assert_eq!(tally.documents, 7);
    assert_eq!(tally.sample, sample.sample);
    assert_eq!(tally.work.statements, 2);
    bounded(tally.work);
    judge(
        &snapshot
            .repair_spelling_tally_plans(&keys)
            .expect("the spelling tally plan"),
    );
}

#[test]
fn a_spelling_group_beyond_sqlites_compound_limit_has_an_exact_ordered_union() {
    let keys: Vec<String> = (0..170)
        .map(|variant| {
            let mut key = String::new();
            let mut separators = variant / 16;
            for (at, letter) in "abcd".chars().enumerate() {
                if at > 0 {
                    key.push(['-', '_', ' ', '.'][separators % 4]);
                    separators /= 4;
                }
                key.push(if variant & (1 << at) == 0 {
                    letter
                } else {
                    letter.to_ascii_uppercase()
                });
            }
            key
        })
        .collect();
    let scratch = Scratch::new("repair-advisory-many-spellings");
    let mut store = scratch.open();
    let declared = ContentModel::none();
    let facts: Vec<_> = [
        "Z.md", "B.md", "a.md", "c.md", "D.md", "e.md", "f.md", "g.md",
    ]
    .into_iter()
    .enumerate()
    .map(|(at, path)| {
        let fields = keys
            .iter()
            .enumerate()
            .filter(|(index, _)| index % 8 == at || *index == 0)
            .map(|(_, key)| {
                let value = match at % 3 {
                    0 => string("value"),
                    1 => FrontmatterValue::Sequence(vec![string("value")]),
                    _ => map(vec![("nested", string("value"))]),
                };
                (key.as_str(), value)
            })
            .collect();
        document(path, path, "body").with_frontmatter(Some(map(fields)), &declared)
    })
    .collect();
    write_documents(&mut store.begin_request(), &facts);
    let reader = Arc::new(store.open_reader().reader.expect("a live reader"));
    let snapshot = reader
        .try_take()
        .expect("a reader")
        .establish()
        .expect("a snapshot");
    let tally = snapshot
        .repair_spelling_tally(&keys)
        .expect("the spelling union tally");
    assert_eq!(tally.documents, 8);
    assert_eq!(
        tally
            .sample
            .paths()
            .iter()
            .map(|path| path.as_str())
            .collect::<Vec<_>>(),
        vec!["a.md", "B.md", "c.md", "D.md", "e.md"]
    );
    assert_eq!(tally.sample.total(), 8);
    assert_eq!(tally.work.statements, 170);
    bounded(tally.work);
    let plans = snapshot
        .repair_spelling_tally_plans(&keys)
        .expect("one spelling tally plan per key");
    assert_eq!(plans.len(), 170);
    judge(&plans);
}
