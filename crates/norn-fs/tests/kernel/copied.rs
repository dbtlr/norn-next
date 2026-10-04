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

    let _recording = record_files();
    let window = ReadWindow::open();
    let staging = scratch
        .stage("landed.md", copy_of("gone.md", &body))
        .expect("a landing");
    assert!(matches!(staging, Staging::Landed(_)), "{staging:?}");
    assert_eq!(
        judge(&scratch, "dest.md", copy_of("gone.md", &body)),
        Ok(())
    );
    let (tally, files) = window.finish_with_files();
    assert_eq!(
        files,
        vec![FileRead {
            act: ReadAct::Target,
            path: scratch.at("landed.md"),
        }],
        "only the landed target is read: {tally:?}"
    );
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
    let tally = window.finish();
    assert_eq!(
        (tally.document_opens, tally.target_reads),
        (0, 0),
        "a source was read: {tally:?}"
    );

    assert!(!exists(&scratch.at("dest.md")));
    assert!(
        scratch.shadow_names().is_empty(),
        "a shadow was left behind"
    );
}

/// **A copy reads its source once, and counts it as the write kernel's
/// read.** Staging a copy over an absent target is one read of the source —
/// counted as a target read, the kernel's own no-follow open whose bytes
/// reach no derivation, named at the source's full path — and no document
/// read; the absent target opens nothing to hash. Publication then reads the
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
        (0, 1, 0),
        "a copy's staging reads: {tally:?}"
    );
    assert_eq!(
        files,
        vec![FileRead {
            act: ReadAct::Target,
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

/// **The bar on a preview that answers a copy's source as staging does.**
/// For every shape of source staging refuses before it reads a byte — a
/// parent name, an absolute name, an empty name, a link at the source's name,
/// a linked folder on its path, a folder and a pipe where a file would be —
/// [`norn_fs::judge`] answers the refusal staging meets, and reads no file to
/// reach it. Where the target has already landed, neither asks after the
/// source at all, so both answer as for the landing.
///
/// The forbidden shape is a preview that answers ready where the apply of the
/// same plan refuses: a move previewed clean whose destination's copy then
/// refuses for a source that was never containable.
#[test]
fn judge_refuses_a_sources_shape_as_staging_does_and_reads_nothing() {
    let scratch = Scratch::new("copy-judged-shape");
    let body = long_body();
    let outside = scratch.vault().with_extension("outside");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a folder outside the vault.
    std::fs::create_dir_all(&outside).expect("a folder outside the vault");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: a document outside the vault.
    std::fs::write(outside.join("secret.md"), &body).expect("a document outside the vault");
    symlink(&outside.join("secret.md"), &scratch.at("escape.md"));
    symlink(&outside, &scratch.at("linked"));
    scratch.directory("folder.md");
    let made = std::process::Command::new("mkfifo")
        .arg(scratch.at("pipe.md"))
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed");
    scratch.place("landed.md", &body);
    let parent = format!(
        "../{}/secret.md",
        outside.file_name().unwrap().to_string_lossy()
    );
    let absolute = outside.join("secret.md").to_string_lossy().into_owned();

    let shapes = [
        parent.as_str(),
        absolute.as_str(),
        "",
        ".",
        "escape.md",
        "linked/secret.md",
        "folder.md",
        "pipe.md",
    ];
    for source in shapes {
        let window = ReadWindow::open();
        let judged = judge(&scratch, "dest.md", copy_of(source, &body));
        let tally = window.finish();
        assert_eq!(
            (tally.document_opens, tally.target_reads, tally.shadow_reads),
            (0, 0, 0),
            "judging a copy of {source:?} read a file: {tally:?}"
        );
        let staged = scratch.stage("dest.md", copy_of(source, &body)).map(drop);
        assert!(
            judged.is_err(),
            "{source:?} judged ready, staged {staged:?}"
        );
        assert_eq!(judged, staged, "{source:?}");
    }

    for source in shapes {
        assert_eq!(
            judge(&scratch, "landed.md", copy_of(source, &body)),
            Ok(()),
            "{source:?}"
        );
        assert!(
            matches!(
                scratch.stage("landed.md", copy_of(source, &body)),
                Ok(Staging::Landed(_))
            ),
            "{source:?}"
        );
    }

    assert!(!exists(&scratch.at("dest.md")));
    assert!(
        scratch.shadow_names().is_empty(),
        "a shadow was left behind"
    );
}

/// **A source's state is its own transition's question, not the create's.**
/// Where the source is absent — no file, a missing folder, a file where a
/// folder would be — or holds bytes other than the create's hash, staging
/// refuses as drift when it reaches the source to copy it, but
/// [`norn_fs::judge`] answers the create ready: telling what a file holds
/// takes reading it, and a judgment reads no source. A move's source carries a
/// transition of its own, a removal held to the same hash, and that
/// transition's judgment is what refuses the plan.
#[test]
fn judge_leaves_a_sources_state_to_its_own_transition() {
    let scratch = Scratch::new("copy-judged-state");
    let body = long_body();
    let edited = [body.as_slice(), b"an edit"].concat();
    scratch.place("source.md", &edited);
    scratch.place("file", b"a file where a folder would be");

    for source in [
        "source.md",
        "absent.md",
        "missing/source.md",
        "file/source.md",
    ] {
        assert_eq!(
            judge(&scratch, "dest.md", copy_of(source, &body)),
            Ok(()),
            "{source:?}"
        );
        assert!(
            matches!(
                scratch.stage("dest.md", copy_of(source, &body)),
                Err(Refusal::Drifted { .. })
            ),
            "{source:?}"
        );
    }
    assert!(matches!(
        norn_fs::judge(
            &scratch.vault(),
            scratch.root(),
            Path::new("source.md"),
            Transition::Remove {
                before: hash(&body)
            },
        ),
        Err(Refusal::Drifted { .. })
    ));
}
