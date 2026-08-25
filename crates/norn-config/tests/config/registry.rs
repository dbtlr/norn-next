//! The registry file: what round-trips, what is refused, and what a read
//! never does.
use norn_config::ConfigError;
use norn_config::registry::{Entry, PollBackend, SchemaSource};

use crate::common::{Scratch, entry, name, registry, root};

/// A first run and a machine with no vaults are the same reading, and neither
/// is an error.
#[test]
fn a_registry_that_is_not_there_reads_as_an_empty_one() {
    let scratch = Scratch::new("absent");
    let registry = registry::read(scratch.dirs()).expect("an empty registry");
    assert_eq!(registry.entries().count(), 0);
}

/// A read never writes. The file is not created, the config directory is not
/// created, and nothing is left behind.
#[test]
fn reading_an_absent_registry_writes_nothing() {
    let scratch = Scratch::new("read-no-write");
    registry::read(scratch.dirs()).expect("an empty registry");
    assert_eq!(
        scratch.names_in(&scratch.config_base()),
        Vec::<String>::new()
    );
}

#[test]
fn an_entry_round_trips_through_the_file() {
    let scratch = Scratch::new("round-trip");
    let dirs = scratch.dirs();

    let mut written = entry("notes", "/home/person/notes");
    written.schema_source =
        Some(SchemaSource::new("/home/person/schemas/notes.yaml").expect("a schema source"));
    written.poll_backend = Some(PollBackend::Poll);

    registry::mutate(dirs, |registry| {
        registry.insert(written.clone());
        Ok(())
    })
    .expect("a registration");

    let read = registry::read(dirs).expect("the registry");
    assert_eq!(read.get(&name("notes")), Some(&written));
}

/// The defaults, and how they are spelled at rest: an absent optional field is
/// absent from the file rather than written as a marker, so the reader's
/// default is the one place the meaning of absence lives.
#[test]
fn an_entry_with_the_defaults_writes_no_optional_field() {
    let scratch = Scratch::new("defaults");
    let dirs = scratch.dirs();

    registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect("a registration");

    let text = scratch.text_at(&scratch.registry_file());
    assert!(text.contains("root = \"/home/person/notes\""), "{text}");
    assert!(!text.contains("schema_source"), "{text}");
    assert!(!text.contains("poll_backend"), "{text}");

    let read = registry::read(dirs).expect("the registry");
    let entry = read.get(&name("notes")).expect("the entry");
    assert_eq!(entry.schema_source, None);
    assert_eq!(entry.poll_backend, None);
}

#[test]
fn entries_are_read_back_in_name_order() {
    let scratch = Scratch::new("order");
    let dirs = scratch.dirs();
    registry::mutate(dirs, |registry| {
        registry.insert(entry("work", "/home/person/work"));
        registry.insert(entry("archive", "/home/person/archive"));
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect("three registrations");

    let read = registry::read(dirs).expect("the registry");
    let names: Vec<&str> = read.entries().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["archive", "notes", "work"]);
}

#[test]
fn an_entry_is_replaced_by_name_and_removed_by_name() {
    let scratch = Scratch::new("replace");
    let dirs = scratch.dirs();

    registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect("a registration");

    let displaced = registry::mutate(dirs, |registry| {
        Ok(registry.insert(entry("notes", "/home/person/moved")))
    })
    .expect("a replacement");
    assert_eq!(
        displaced.expect("the displaced entry").root,
        root("/home/person/notes")
    );

    let read = registry::read(dirs).expect("the registry");
    assert_eq!(read.entries().count(), 1);
    assert_eq!(
        read.get(&name("notes")).expect("the entry").root,
        root("/home/person/moved")
    );

    let removed =
        registry::mutate(dirs, |registry| Ok(registry.remove(&name("notes")))).expect("a removal");
    assert!(removed.is_some());
    assert_eq!(
        registry::read(dirs)
            .expect("the registry")
            .entries()
            .count(),
        0
    );

    let missing = registry::mutate(dirs, |registry| Ok(registry.remove(&name("notes"))))
        .expect("a removal of nothing");
    assert!(missing.is_none());
}

/// A refused mutation leaves the file exactly as it was read, which is what
/// makes attempting one safe.
#[test]
fn a_mutation_that_refuses_writes_nothing() {
    let scratch = Scratch::new("refused");
    let dirs = scratch.dirs();

    registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/notes"));
        Ok(())
    })
    .expect("a registration");
    let before = scratch.text_at(&scratch.registry_file());

    let error = registry::mutate(dirs, |registry| {
        registry.insert(entry("work", "/home/person/work"));
        Err::<(), _>(ConfigError::DuplicateLabel {
            label: "the caller changed its mind".to_string(),
        })
    })
    .expect_err("a refused mutation");
    assert!(matches!(error, ConfigError::DuplicateLabel { .. }));

    assert_eq!(scratch.text_at(&scratch.registry_file()), before);
    assert_eq!(
        registry::read(dirs)
            .expect("the registry")
            .entries()
            .count(),
        1
    );
}

/// The registry names roots; it does not resolve them. Two entries pointing at
/// one directory is a question for whoever serves them.
#[test]
fn two_entries_may_name_the_same_root() {
    let scratch = Scratch::new("same-root");
    let dirs = scratch.dirs();
    registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/vault"));
        registry.insert(entry("second", "/home/person/vault"));
        Ok(())
    })
    .expect("two registrations naming one root");
    assert_eq!(
        registry::read(dirs)
            .expect("the registry")
            .entries()
            .count(),
        2
    );
}

/// An entry whose name is not a name at rest. The file is machine-local and a
/// person may edit it, so the grammar is enforced on the way in as well as at
/// construction.
#[test]
fn a_file_naming_a_vault_illegally_is_refused_on_read() {
    let scratch = Scratch::new("illegal-in-file");
    let dirs = scratch.dirs();
    scratch.place(
        &scratch.registry_file(),
        "version = 1\n\n[vaults.Notes_1]\nroot = \"/home/person/notes\"\n",
    );
    let error = registry::read(dirs).expect_err("an illegal name in the file");
    assert!(matches!(error, ConfigError::IllegalName { .. }), "{error}");
}

/// A path in the file is held to what the constructor holds a path to, in both
/// fields: the boundary is the parse, not the API.
#[test]
fn a_file_naming_a_relative_path_is_refused_on_read() {
    for (body, subject) in [
        (
            "version = 1\n\n[vaults.notes]\nroot = \"notes\"\n",
            "vault root",
        ),
        (
            "version = 1\n\n[vaults.notes]\nroot = \"/home/person/notes\"\n\
             schema_source = \"schemas/notes.yaml\"\n",
            "schema source",
        ),
    ] {
        let scratch = Scratch::new("relative-in-file");
        let dirs = scratch.dirs();
        scratch.place(&scratch.registry_file(), body);
        let error = registry::read(dirs).expect_err("a relative path in the file");
        let ConfigError::IllegalPath {
            subject: refused, ..
        } = error
        else {
            panic!("`{body}` was refused as something other than an illegal path");
        };
        assert_eq!(refused, subject);
    }
}

/// A name is a key for as long as an entry lives under it, and no longer.
/// Keeping a removed entry's unknown keys against its name would graft a
/// newer build's state — a revocation, a scope — onto whatever is registered
/// under that name next.
#[test]
fn a_name_registered_again_carries_nothing_of_the_entry_that_had_it() {
    let scratch = Scratch::new("re-registration");
    let dirs = scratch.dirs();
    scratch.place(
        &scratch.registry_file(),
        "version = 1\n\n[vaults.notes]\nroot = \"/home/person/notes\"\nfuture_field = \"stale\"\n",
    );

    registry::mutate(dirs, |registry| {
        registry.remove(&name("notes"));
        registry.insert(entry("notes", "/home/person/fresh"));
        Ok(())
    })
    .expect("a removal and a re-registration");

    let text = scratch.text_at(&scratch.registry_file());
    assert!(
        !text.contains("future_field"),
        "the removed entry's fields were grafted onto the new one: {text}"
    );
    assert!(text.contains("/home/person/fresh"), "{text}");
}

/// The same scope holds when the old entry is displaced rather than removed:
/// a bare `insert` under a taken name starts a new entry, and the displaced
/// one's unmodeled keys go with it.
#[test]
fn an_entry_displaced_by_insert_carries_nothing_of_its_predecessor() {
    let scratch = Scratch::new("displacement");
    let dirs = scratch.dirs();
    scratch.place(
        &scratch.registry_file(),
        "version = 1\n\n[vaults.notes]\nroot = \"/home/person/notes\"\nfuture_field = \"stale\"\n",
    );

    registry::mutate(dirs, |registry| {
        registry.insert(entry("notes", "/home/person/fresh"));
        Ok(())
    })
    .expect("a displacement");

    let text = scratch.text_at(&scratch.registry_file());
    assert!(
        !text.contains("future_field"),
        "the displaced entry's fields were grafted onto its replacement: {text}"
    );
    assert!(text.contains("/home/person/fresh"), "{text}");
}

/// The other direction, which was already right and stays that way: an entry
/// that is renamed rather than replaced leaves its unknown keys behind with
/// the name it had.
#[test]
fn renaming_an_entry_leaves_its_unknown_keys_with_the_old_name() {
    let scratch = Scratch::new("rename");
    let dirs = scratch.dirs();
    scratch.place(
        &scratch.registry_file(),
        "version = 1\n\n[vaults.notes]\nroot = \"/home/person/notes\"\nfuture_field = \"stale\"\n",
    );

    registry::mutate(dirs, |registry| {
        let moved = registry.remove(&name("notes")).expect("the entry");
        registry.insert(Entry::new(name("archive"), moved.root));
        Ok(())
    })
    .expect("a rename");

    let text = scratch.text_at(&scratch.registry_file());
    assert!(!text.contains("future_field"), "{text}");
    assert!(text.contains("[vaults.archive]"), "{text}");
}

/// Every corrupt shape says what is wrong, because the resolution is a person
/// opening the file.
#[test]
fn a_malformed_entry_is_refused_with_the_reason() {
    for (body, needle) in [
        ("version = 1\n\n[vaults.notes]\n", "`root` is required"),
        (
            "version = 1\n\n[vaults.notes]\nroot = 4\n",
            "`root` is integer",
        ),
        (
            "version = 1\n\n[vaults.notes]\nroot = \"/x\"\npoll_backend = \"kqueue\"\n",
            "names no backend",
        ),
        ("version = 1\nvaults = 3\n", "`vaults` is integer"),
        (
            "version = 1\n\nvaults = { notes = 3 }\n",
            "an entry is a table",
        ),
    ] {
        let scratch = Scratch::new("malformed");
        let dirs = scratch.dirs();
        scratch.place(&scratch.registry_file(), body);
        let error = registry::read(dirs).expect_err("a malformed registry");
        let ConfigError::Corrupt { reason, .. } = error else {
            panic!("`{body}` was refused as something other than corrupt");
        };
        assert!(
            reason.contains(needle),
            "`{body}` was refused with `{reason}`, which does not mention `{needle}`"
        );
    }
}
