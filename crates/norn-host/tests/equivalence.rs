//! **One vault, derived twice, compared.** The comparator's own acceptance
//! test.
//!
//! A generated tree is attached by a host that derives it into one store, and
//! then by a second host whose machine-local directories are somewhere else
//! entirely — no derived row, no maintainer lock and no shadow of the first
//! one's is reachable from it, so the second derivation starts from zero over
//! the same documents. Two derivations of one vault either agree or one of them
//! is wrong, and this is what asks.
//!
//! **The relative claim is paired with an absolute one.** Two hosts that both
//! derived nothing would agree, so the case states what the tree really holds:
//! the population floor comes off the profile, the pinned vault schema is read
//! for its bytes, and the one document in the tree that cannot be decoded is
//! read back on both sides as a finding at a place no document row stands at.
//! A comparator that stopped projecting a field would pass
//! the first claim and fail the second only where the second names that field —
//! which is why the mutation pins in `norn-store`'s own suite are the other half
//! of this, and this case is the half that says the projection describes a real
//! attachment.
//!
//! **The evidence leg runs where its readers are compiled.** What a host's jobs
//! spent and did is written on every build and read behind `induced-failure`,
//! so the claims about what each attachment really read stand under that
//! feature while the comparison itself — the whole subject of this case — runs
//! in every lane.
//!
//! **This is not the churn suite.** Nothing here edits the tree while a host is
//! serving it: what is compared is two cold derivations of one unchanging tree,
//! which is the weakest claim the machinery can carry and the one that has to
//! hold before any stronger one means anything.
//!
//! Each generated tree sits in a testkit sandbox, which is a unix-only harness.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)] // Harness scaffolding: this suite's own generated tree.

mod attach;

use std::path::Path;

use norn_testkit::equivalence::{Population, StoreProjection, assert_operationally_valid};
use norn_testkit::process::Sandbox;
use norn_wire::{FindingKind, VaultName};

/// The profile this case derives twice.
///
/// Small enough that two attachments are cheap, and shaped like a real vault:
/// it carries ambiguity classes, dangling links, unicode and spaced stems,
/// clutter that is not a document, and symbolic links a walk refuses to follow.
/// Every one of those is a place two derivations could disagree.
///
/// **None of them leaves a finding row behind.** An ambiguity class and a
/// dangling link are concluded where a target is resolved, and a heal records
/// neither: the kinds a heal writes are the ones it reaches by reading a
/// document — a path or a body it cannot decode, and a frontmatter block that
/// did not read. A tree of well-formed documents derives no finding at all,
/// which is why this case writes [`UNDECODABLE_AT`] beside the generated one.
const PROFILE: &str = "tiny";

/// The one document in the tree the heal cannot decode.
///
/// It is written beside the generated tree rather than drawn from it, because
/// the generator draws documents that derive. A vault with no finding in it
/// would leave the findings leg of the comparison standing on nothing: two
/// stores that each derived zero findings agree about findings however broken
/// the finding pillar is.
const UNDECODABLE_AT: &str = "undecodable.md";

/// Bytes no UTF-8 decoder accepts, in a file a walk reads as a document.
///
/// The heal opens it, refuses to decode the body, and quarantines it: no
/// document row is written and a finding is recorded at the place instead —
/// which is the case only an enumeration of the findings table reaches, since
/// no keyed read starts from a row that is not there.
const UNDECODABLE: &[u8] = b"# a title\n\nthese bytes are not text: \xff\xfe\n";

#[test]
fn one_vault_derived_twice_from_zero_holds_the_same_derived_facts() {
    let sandbox =
        Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "equivalence").expect("a sandbox");
    let first = attach::Vault::generate(&sandbox.work_dir().join("attached"), PROFILE);
    std::fs::write(first.path().join(UNDECODABLE_AT), UNDECODABLE)
        .expect("writing the document neither derivation can decode");
    let second = first.beside(&sandbox.work_dir().join("second-machine"));

    derive(&first, "the first derivation");
    derive(&second, "the second derivation");

    let mut left = first.store();
    let mut right = second.store();
    assert_operationally_valid(&mut left, "the first derivation");
    assert_operationally_valid(&mut right, "the second derivation");

    let left = StoreProjection::read(&mut left).expect("projecting the first store");
    let right = StoreProjection::read(&mut right).expect("projecting the second store");

    // The absolute floor, stated before the relative claim so a failure says
    // which of the two it is. The document count comes off the profile rather
    // than a number written here, and the rest are floors a real attachment
    // clears by construction: a tree of documents holds facts, and an
    // attachment pins the vault's schema.
    let profile = norn_fixtures::Profile::by_name(PROFILE).expect("the profile this case derives");
    for (label, projection) in [
        ("the first derivation", &left),
        ("the second derivation", &right),
    ] {
        projection.assert_population_at_least(
            label,
            Population {
                documents: profile.docs,
                facts: profile.docs,
                findings: 1,
                indexed_terms: profile.docs,
                vault_schema_pinned: true,
            },
        );
        assert_eq!(
            projection
                .vault_schema()
                .expect("a pinned vault schema")
                .bytes,
            attach::SCHEMA,
            "{label} pinned bytes the vault does not hold"
        );
        // **The finding stands where no document row does.** The quarantine is
        // what makes it so, and it is the shape the comparison could only reach
        // by enumerating the findings table: a projection assembled from the
        // rows the store holds would never ask about this path.
        let quarantined = projection
            .findings()
            .iter()
            .find(|finding| finding.path == UNDECODABLE_AT)
            .unwrap_or_else(|| {
                panic!(
                    "{label} recorded no finding at `{UNDECODABLE_AT}`; it holds {}",
                    projection.population()
                )
            });
        assert_eq!(
            quarantined.kind,
            FindingKind::BodyBytesNotUtf8.as_str(),
            "{label} recorded another cause at `{UNDECODABLE_AT}`"
        );
        assert!(
            projection.document(UNDECODABLE_AT).is_none(),
            "{label} derived a document row at `{UNDECODABLE_AT}`, which the heal quarantined"
        );
    }

    left.assert_equivalent(&right, "one vault derived twice from zero");
}

/// Attach `vault`, wait for it to be ready, and let it go.
///
/// The host and its demand are dropped inside this call, so what the comparison
/// reads afterwards is what an attachment left behind rather than a store a host
/// is still writing to. The two derivations are therefore sequential: each takes
/// its own maintainer lock and its own platform watcher, one at a time.
fn derive(vault: &attach::Vault, label: &str) {
    let host = vault.host();
    let lease = attach::attach_and_wait(&host, vault.name());
    assert_the_attach_stated_the_root(&host, vault.name(), label);
    drop(lease);
    assert_the_derivation_read_the_vault(&host, label);
}

/// **The attach stated the root it served.** The classification is what read the
/// root and decided no other served name stands at it; a host that classified
/// nothing attached over a root it never looked at. A ready entry owes no
/// recovery, so nothing is waiting on one either.
#[cfg(feature = "induced-failure")]
fn assert_the_attach_stated_the_root(host: &attach::ServingHost, name: &VaultName, label: &str) {
    assert!(
        host.classifications() > 0,
        "{label} stated no root against the serving set"
    );
    assert_eq!(host.recovery_demands(name), Some(0), "{label}");
}

/// The same claim, in a build whose readers are not compiled.
#[cfg(not(feature = "induced-failure"))]
fn assert_the_attach_stated_the_root(_: &attach::ServingHost, _: &VaultName, _: &str) {}

/// **The evidence says the derivation really read the vault.** A host that
/// attached without walking would open no document, land no changeset and still
/// leave a store behind for the comparison to find equal to the other empty
/// one.
#[cfg(feature = "induced-failure")]
fn assert_the_derivation_read_the_vault(host: &attach::ServingHost, label: &str) {
    let evidence = host.evidence();
    assert!(
        evidence.document_opens > 0 && evidence.walk_dirents > 0 && evidence.stats > 0,
        "{label} asked the filesystem for nothing: {evidence:?}"
    );
    assert!(
        evidence.changesets_applied > 0 && evidence.documents_upserted > 0,
        "{label} landed no changeset: {evidence:?}"
    );
    assert_eq!(
        (evidence.recoveries_run, evidence.rebuilds_run),
        (0, 0),
        "{label} climbed a rung of the ladder over a vault nothing damaged: {evidence:?}"
    );
}

/// The same claim, in a build whose readers are not compiled.
#[cfg(not(feature = "induced-failure"))]
fn assert_the_derivation_read_the_vault(_: &attach::ServingHost, _: &str) {}
