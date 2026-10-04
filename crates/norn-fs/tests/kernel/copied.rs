//! A create whose content is a streamed copy of another file below the same
//! root: the source is reached as a target is, copied into the shadow a chunk
//! at a time, and held to the hash the create names.

use std::path::Path;

use norn_fs::reads::{FileRead, ReadAct, ReadWindow, record_files};
use norn_fs::{AfterState, Content, Refusal, Staging, Transition};

use crate::common::{Scratch, bytes_at, exists, hash, staged, symlink, wrote};

/// The copy's chunk, which the bodies here are longer than.
const CHUNK: usize = 64 * 1024;

/// A body that crosses two of the copy's chunk boundaries.
fn long_body() -> Vec<u8> {
    (0..CHUNK * 2 + 7)
        .map(|i| b"abcdefghij\n"[i % 11])
        .collect()
}

/// A create of a copy of `source`, which must hash to `bytes`' hash.
fn copy_of<'a>(source: &'a str, bytes: &[u8]) -> Transition<'a> {
    Transition::Create {
        content: Content::CopyOf {
            source: Path::new(source),
            hash: hash(bytes),
        },
    }
}

/// Judge `transition` at `relative` as staging would.
fn judge(scratch: &Scratch, relative: &str, transition: Transition<'_>) -> Result<(), Refusal> {
    norn_fs::judge(
        &scratch.vault(),
        scratch.root(),
        Path::new(relative),
        transition,
    )
}

/// **A copy publishes its source's bytes, and only the create changes the
/// vault.** The published file holds the source's bytes byte for byte across
/// the copy's chunk boundaries, reports the hash the create named, and the
/// source is left as it was: a copy is a create, and removing a move's source
/// is a transition of its own.
#[test]
fn a_copied_create_publishes_the_sources_bytes_and_leaves_the_source() {
    let scratch = Scratch::new("copy-publishes");
    let body = long_body();
    let source = scratch.place("notes/source.md", &body);

    let published = scratch
        .stage_and_publish("moved/dest.md", copy_of("notes/source.md", &body))
        .expect("a copy onto nothing");

    let AfterState::Present(state) = published.after else {
        panic!("a create published absence");
    };
    assert_eq!(state.content_hash, hash(&body));
    assert_eq!(state.len, body.len() as u64);
    assert_eq!(bytes_at(&scratch.at("moved/dest.md")), body);
    assert_eq!(bytes_at(&source), body);
    assert!(
        scratch.shadow_names().is_empty(),
        "a shadow was left behind"
    );
}

/// **A copy is judged by its hash, as held bytes of that hash are.** Over an
/// absent target, a target taken by other bytes and a target already at the
/// hash, staging and [`norn_fs::judge`] answer for a copy exactly what they
/// answer for the same bytes held.
///
/// The forbidden shape is a second judgment for the second source of bytes: a
/// preview that answered a move's destination differently from the bytes it
/// stands for would preview a plan the apply does not run.
#[test]
fn a_copied_create_is_judged_as_held_bytes_of_its_hash() {
    let scratch = Scratch::new("copy-judged");
    let body = long_body();
    scratch.place("source.md", &body);
    let held = Transition::Create {
        content: Content::Held(&body),
    };
    let copied = copy_of("source.md", &body);

    // An absent target stages for both, and is judged ready for both.
    for transition in [held, copied] {
        assert_eq!(judge(&scratch, "dest.md", transition), Ok(()));
        let staging = staged(scratch.stage("dest.md", transition).expect("staged"));
        scratch.discard(staging);
    }

    // A target taken by other bytes refuses both alike.
    scratch.place("taken.md", b"somebody else's");
    let answers = [held, copied].map(|transition| {
        (
            judge(&scratch, "taken.md", transition),
            scratch.stage("taken.md", transition).map(drop),
        )
    });
    assert_eq!(answers[0], answers[1]);
    assert!(
        matches!(answers[1].0, Err(Refusal::DestinationExists { .. })),
        "{answers:?}"
    );

    // A target already at the hash has landed for both, as one landing.
    scratch.place("landed.md", &body);
    let landings = [held, copied].map(|transition| {
        assert_eq!(judge(&scratch, "landed.md", transition), Ok(()));
        match scratch.stage("landed.md", transition).expect("a landing") {
            Staging::Landed(landed) => landed,
            Staging::Staged(staged) => panic!("{:?} staged over its landing", staged.path()),
        }
    });
    assert_eq!(landings[0], landings[1]);
    assert!(scratch.shadow_names().is_empty());
}

/// **The after-state is the hash the create names, known without the
/// source.** A target already at the hash lands and the source is never
/// opened, and judging a copy reads no source either — here there is none to
/// read.
#[test]
fn a_copys_after_state_is_known_without_reading_its_source() {
    let scratch = Scratch::new("copy-unread");
    let body = long_body();
    scratch.place("landed.md", &body);

    let window = ReadWindow::open();
    let staging = scratch
        .stage("landed.md", copy_of("gone.md", &body))
        .expect("a landing");
    assert!(matches!(staging, Staging::Landed(_)), "{staging:?}");
    assert_eq!(
        judge(&scratch, "dest.md", copy_of("gone.md", &body)),
        Ok(())
    );
    let tally = window.finish();
    assert_eq!(tally.document_opens, 0, "a source was read: {tally:?}");
}

/// **The bar on a source that is not the content the create names.** Staging
/// refuses as drift naming the source — onto the bytes it found, or onto
/// absence where nothing is there — and as a non-file where the source is a
/// folder. Nothing is created and no shadow is left.
///
/// The forbidden shape is publishing whatever the source holds now: the
/// create's hash is what its caller composed against, and a copy of other
/// bytes is a write nobody planned.
#[test]
fn a_copy_of_a_source_not_at_its_hash_refuses_and_leaves_nothing() {
    let scratch = Scratch::new("copy-drifted");
    let body = long_body();
    let edited = [body.as_slice(), b"an edit"].concat();
    let source = scratch.place("source.md", &edited);
    scratch.place("file", b"a file where a folder would be");
    scratch.directory("folder.md");

    let refusal = scratch
        .stage("dest.md", copy_of("source.md", &body))
        .expect_err("a source holding other bytes");
    let Refusal::Drifted {
        path,
        expected,
        observed: Some(observed),
    } = &refusal
    else {
        panic!("not drift onto other bytes: {refusal}");
    };
    assert_eq!(
        (path, *expected, observed.content_hash, observed.len),
        (&source, hash(&body), hash(&edited), edited.len() as u64)
    );

    for absent in ["absent.md", "missing/source.md", "file/source.md"] {
        assert_eq!(
            scratch.stage("dest.md", copy_of(absent, &body)).map(drop),
            Err(Refusal::Drifted {
                path: scratch.at(absent),
                expected: hash(&body),
                observed: None,
            }),
            "{absent}"
        );
    }
    assert_eq!(
        scratch
            .stage("dest.md", copy_of("folder.md", &body))
            .map(drop),
        Err(Refusal::NotRegularFile {
            path: scratch.at("folder.md"),
        })
    );

    assert!(!exists(&scratch.at("dest.md")));
    assert!(
        scratch.shadow_names().is_empty(),
        "a shadow was left behind"
    );
    assert_eq!(bytes_at(&source), edited);
}

/// **The bar on containment.** A source is reached from the root as a target
/// is: a link at its name, a linked folder on its path, a parent name and an
/// absolute name each refuse before a byte is read — though the file outside
/// holds exactly the bytes the create names — and nothing is created.
///
/// The forbidden shape is a copy that follows a name out of the vault: a move
/// whose source is a link to a private file would publish that file's bytes
/// as a document inside it.
#[test]
fn a_copy_never_reads_a_source_outside_the_root() {
    let scratch = Scratch::new("copy-contained");
    let body = long_body();
    let outside = scratch.vault().with_extension("outside");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a folder outside the vault.
    std::fs::create_dir_all(&outside).expect("a folder outside the vault");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a document outside the vault.
    std::fs::write(outside.join("secret.md"), &body).expect("a document outside the vault");
    symlink(&outside.join("secret.md"), &scratch.at("escape.md"));
    symlink(&outside, &scratch.at("linked"));
    let parent = format!(
        "../{}/secret.md",
        outside.file_name().unwrap().to_string_lossy()
    );
    let absolute = outside.join("secret.md").to_string_lossy().into_owned();

    let window = ReadWindow::open();
    assert_eq!(
        scratch
            .stage("dest.md", copy_of("escape.md", &body))
            .map(drop),
        Err(Refusal::SymlinkDestination {
            path: scratch.at("escape.md"),
        })
    );
    assert_eq!(
        scratch
            .stage("dest.md", copy_of("linked/secret.md", &body))
            .map(drop),
        Err(Refusal::LinkedAncestor {
            path: scratch.at("linked/secret.md"),
            ancestor: scratch.at("linked"),
        })
    );
    for uncontained in [parent.as_str(), absolute.as_str()] {
        let refusal = scratch
            .stage("dest.md", copy_of(uncontained, &body))
            .expect_err("a source that leaves the root");
        assert!(
            matches!(refusal, Refusal::InvalidRequest { .. }),
            "{uncontained}: {refusal}"
        );
    }
    assert_eq!(window.finish().document_opens, 0, "a source was read");

    assert!(!exists(&scratch.at("dest.md")));
    assert!(
        scratch.shadow_names().is_empty(),
        "a shadow was left behind"
    );
}

/// **A copy reads its source once, and counts it.** Staging a copy over an
/// absent target is one read of the source's content — counted as a
/// document read, named at the source's full path — and no target read,
/// since nothing is at the target to hash; publication then reads the
/// target's name and the shadow as for any create.
#[test]
fn a_copy_reads_its_source_once_and_names_it() {
    let scratch = Scratch::new("copy-reads");
    let body = long_body();
    let source = scratch.place("source.md", &body);
    let _recording = record_files();

    let window = ReadWindow::open();
    let staging = staged(
        scratch
            .stage("dest.md", copy_of("source.md", &body))
            .expect("a copy"),
    );
    let (tally, files) = window.finish_with_files();
    assert_eq!(
        (tally.document_opens, tally.target_reads, tally.shadow_reads),
        (1, 0, 0),
        "a copy's staging reads: {tally:?}"
    );
    assert_eq!(
        files,
        vec![FileRead {
            act: ReadAct::Document,
            path: source.clone(),
        }]
    );

    let window = ReadWindow::open();
    let published = wrote(scratch.publish(staging).expect("publishing a copy"));
    let tally = window.finish();
    assert_eq!(
        (tally.document_opens, tally.shadow_reads),
        (0, 1),
        "a copy's publication reads: {tally:?}"
    );
    assert!(matches!(published.after, AfterState::Present(_)));
    assert_eq!(bytes_at(&scratch.at("dest.md")), body);
}
