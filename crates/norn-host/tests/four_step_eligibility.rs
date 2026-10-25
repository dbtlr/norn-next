//! Reproducer for the lab's unresolved early link-target eligibility boundary.
//! A names-only row resolves during Get before content can quarantine it.
#![cfg(feature = "induced-failure")]
#![allow(clippy::disallowed_methods, clippy::disallowed_macros)]
use norn_store::*;
use std::sync::Arc;

struct Text;
impl DocumentText for Text {
    fn section(&self, _: &[HeadingFact], _: &str, _: usize, _: &str) -> Option<SectionAt> {
        unreachable!()
    }
    fn block(&self, _: &str, _: usize, _: u64) -> std::ops::Range<usize> {
        unreachable!()
    }
}
#[test]
fn provisional_name_changes_whole_get_links() {
    let scratch = norn_testkit::scratch::Scratch::under(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch"),
        "eligibility-probe",
    )
    .unwrap();
    let mut store = Store::open(
        scratch.join("store.sqlite3"),
        StoredPathOrder::Sensitive,
        DerivationVersion::new(1),
    )
    .unwrap();
    let a = DocumentPath::new("a.md").unwrap();
    let z = DocumentPath::new("z.md").unwrap();
    let mut facts = DocumentFacts::new(z.clone(), "hash", "[[a]]", 5);
    facts.links.push(LinkFact {
        field: None,
        family: LinkFamily::Wikilink,
        embed: false,
        protocol: None,
        target: "a".into(),
        title: None,
        anchor: None,
        span: None,
    });
    let model = ContentModel::none();
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [
                Change::Upsert(DocumentFacts::new(a.clone(), "", "", 0)),
                Change::Upsert(facts),
            ],
            &[],
            &model,
        )
        .unwrap();
    let budget = ReadBudget::new(ReadBudgetPolicy::default());
    let reader = Arc::new(store.reader_pool(&budget));
    let params = norn_wire::GetParams::new(
        norn_wire::VaultAddress::name(norn_wire::VaultName::new("notes").unwrap()),
        norn_wire::ResolutionTarget::new("z").unwrap(),
    );
    let early = reader
        .wait_for_the_connection_until(
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap()
        .establish()
        .unwrap()
        .get(&params, &model, &Text)
        .unwrap();
    // An ordinary first build over undecodable a.md never inserts that row.
    // Remove the provisional name to represent that final candidate set. The
    // tombstone is irrelevant to Get resolution; only the document row counts.
    store
        .begin_request()
        .apply_increment(
            IncrementProvenance::Derived,
            [Change::Death {
                path: a,
                provenance: Provenance::Quarantine,
            }],
            &[],
            &model,
        )
        .unwrap();
    let final_get = reader
        .wait_for_the_connection_until(
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap()
        .establish()
        .unwrap()
        .get(&params, &model, &Text)
        .unwrap();
    let early = serde_json::to_value(early.report).unwrap();
    let final_get = serde_json::to_value(final_get.report).unwrap();
    println!("early links: {}", early["document"]["links"]);
    println!("final links: {}", final_get["document"]["links"]);
    assert_eq!(early["document"]["links"]["items"][0]["health"], "healthy");
    assert_eq!(
        final_get["document"]["links"]["items"][0]["health"],
        "broken"
    );
    assert_ne!(early["document"]["links"], final_get["document"]["links"]);
}
