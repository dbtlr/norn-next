use super::*;
use std::os::unix::fs::PermissionsExt;

/// Folders a case took search permission from, given back when the case ends
/// however it ends, so the fixture's own cleanup can remove them.
struct Denied(Vec<PathBuf>);

impl Denied {
    /// Take search permission from `folder`, answering `None` where this
    /// account still stats `probe` beneath it (as root does), in which case the
    /// folder is given back at once and the case proves nothing.
    fn search(folder: &Path, probe: &Path) -> Option<Self> {
        fs::set_permissions(folder, fs::Permissions::from_mode(0o400)).unwrap();
        let denied = Denied(vec![folder.to_owned()]);
        if fs::metadata(probe).is_ok() {
            eprintln!("skipped: this account stats beneath a mode-400 directory");
            return None;
        }
        Some(denied)
    }

    /// Give `folder` back as well, where a case moved the denied one there.
    fn also(&mut self, folder: &Path) {
        self.0.push(folder.to_owned());
    }

    /// Give every folder back now.
    fn restore(&self) {
        for folder in &self.0 {
            let _ = fs::set_permissions(folder, fs::Permissions::from_mode(0o755));
        }
    }
}

impl Drop for Denied {
    fn drop(&mut self) {
        self.restore();
    }
}

/// Attach over `folders` (created in order) and `files`, answering `None` where
/// this filesystem refuses one of the names.
fn attach_over(
    f: &Fixture,
    folders: &[&str],
    files: &[&str],
) -> Option<(
    ProductionEntryOps,
    VaultName,
    ProgressReporter<ProductionAttachment>,
    ProductionAttachment,
)> {
    for folder in folders {
        if fs::create_dir(f.vault().join(folder)).is_err() {
            eprintln!("skipped: this filesystem does not create `{folder:?}`");
            return None;
        }
    }
    for file in files {
        if !write_or_report(&f.vault().join(file), b"body [[kept]]\n") {
            return None;
        }
    }
    fs::write(f.vault().join("kept.md"), "kept\n").unwrap();
    let (ops, name) = f.ops(2);
    let progress = ProgressReporter::disconnected();
    let attachment = ops.attach(&f.registration(), &progress).unwrap();
    Some((ops, name, progress, attachment))
}

/// How a folder holding a mark leaves the tree.
#[derive(Clone, Copy, Debug)]
enum Leaves {
    /// Its search permission comes back.
    Relisted,
    /// It is deleted with everything under it.
    Deleted,
    /// It is renamed, mark and all, to a sibling name.
    Moved,
    /// It is deleted and a file takes its name.
    BecameAFile,
}

/// Mark a denied folder beneath `top` through a watcher report on `top`, then
/// let `top` leave the way `leaves` says, and hold the store to a build from
/// zero over the tree that results.
///
/// `denied` is the folder losing search permission, `place` the place its mark
/// files at, and `moved` the name `top` takes when it moves.
#[allow(clippy::too_many_arguments)]
fn a_mark_follows_its_folder(
    label: &str,
    folders: &[&str],
    files: &[&str],
    top: &str,
    denied: &str,
    place: &str,
    moved: &str,
    leaves: Leaves,
) {
    let f = Fixture::new(label);
    let Some((ops, name, progress, mut attachment)) = attach_over(&f, folders, files) else {
        return;
    };
    let vault = f.vault();
    let denied_at = vault.join(denied);
    let probe = fs::read_dir(&denied_at)
        .unwrap()
        .next()
        .expect("the denied folder holds an entry")
        .unwrap()
        .path();
    let Some(mut denial) = Denied::search(&denied_at, &probe) else {
        ops.detach(&name, attachment);
        return;
    };
    watcher_leg(
        &ops,
        &name,
        &mut attachment,
        &progress,
        vault.as_path(),
        &[top],
    );
    assert_eq!(
        sorted_kinds(&mut attachment.store, place),
        ["directory/unreadable"],
        "the watcher leg did not mark the folder, so this proves nothing"
    );

    let mut reported = vec![top];
    match leaves {
        Leaves::Relisted => denial.restore(),
        Leaves::Deleted => {
            denial.restore();
            fs::remove_dir_all(vault.join(top)).unwrap();
        }
        Leaves::Moved => {
            fs::rename(vault.join(top), vault.join(moved)).unwrap();
            let relative = Path::new(denied).strip_prefix(top).unwrap();
            denial.also(&vault.join(moved).join(relative));
            reported.push(moved);
        }
        Leaves::BecameAFile => {
            denial.restore();
            fs::remove_dir_all(vault.join(top)).unwrap();
            fs::write(vault.join(top), "now a file\n").unwrap();
        }
    }
    watcher_leg(
        &ops,
        &name,
        &mut attachment,
        &progress,
        vault.as_path(),
        &reported,
    );
    let marked = sorted_kinds(&mut attachment.store, place);
    let agreed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_a_build_from_zero_agrees(&f, &mut attachment)
    }));
    denial.restore();
    assert!(
        marked.is_empty(),
        "{leaves:?}: the mark outlived its folder: {marked:?}"
    );
    if let Err(disagreement) = agreed {
        std::panic::resume_unwind(disagreement);
    }
    ops.detach(&name, attachment);
}

// **A refused folder takes the marks beneath it when it leaves.** Its spelling
// addresses no stored range, so the mark of a denied folder under it stands at
// a rendered place no scope the leg registers reaches.

const REFUSED: &[&str] = &["bad\u{1}dir", "bad\u{1}dir/lk"];
const REFUSED_FILES: &[&str] = &["bad\u{1}dir/lk/a.md"];

#[test]
fn a_relisted_folder_under_a_refused_folder_ends_its_mark() {
    a_mark_follows_its_folder(
        "refused-folder-relisted",
        REFUSED,
        REFUSED_FILES,
        "bad\u{1}dir",
        "bad\u{1}dir/lk",
        "bad\u{fffd}dir/lk",
        "",
        Leaves::Relisted,
    );
}

#[test]
fn a_deleted_refused_folder_takes_the_mark_beneath_it() {
    a_mark_follows_its_folder(
        "refused-folder-deleted",
        REFUSED,
        REFUSED_FILES,
        "bad\u{1}dir",
        "bad\u{1}dir/lk",
        "bad\u{fffd}dir/lk",
        "",
        Leaves::Deleted,
    );
}

#[test]
fn a_moved_refused_folder_takes_the_mark_beneath_it() {
    a_mark_follows_its_folder(
        "refused-folder-moved",
        REFUSED,
        REFUSED_FILES,
        "bad\u{1}dir",
        "bad\u{1}dir/lk",
        "bad\u{fffd}dir/lk",
        "bad\u{1}moved",
        Leaves::Moved,
    );
}

#[test]
fn a_refused_folder_that_becomes_a_file_takes_the_mark_beneath_it() {
    a_mark_follows_its_folder(
        "refused-folder-file",
        REFUSED,
        REFUSED_FILES,
        "bad\u{1}dir",
        "bad\u{1}dir/lk",
        "bad\u{fffd}dir/lk",
        "",
        Leaves::BecameAFile,
    );
}

// **An admitted folder whose name carries the marker ends its mark, and the
// marks beneath it.** `q\u{fffd}` is spelled as the vault holds it, and its
// place is still one `q\\` renders onto too, so no scope below the vault
// root concludes it.

const MARKER_NAMED: &[&str] = &["q\u{fffd}"];
const MARKER_NAMED_FILES: &[&str] = &["q\u{fffd}/a.md"];

#[test]
fn a_relisted_folder_at_a_rendered_place_ends_its_mark() {
    a_mark_follows_its_folder(
        "rendered-place-relisted",
        MARKER_NAMED,
        MARKER_NAMED_FILES,
        "q\u{fffd}",
        "q\u{fffd}",
        "q\u{fffd}",
        "",
        Leaves::Relisted,
    );
}

#[test]
fn a_deleted_folder_at_a_rendered_place_ends_its_mark() {
    a_mark_follows_its_folder(
        "rendered-place-deleted",
        MARKER_NAMED,
        MARKER_NAMED_FILES,
        "q\u{fffd}",
        "q\u{fffd}",
        "q\u{fffd}",
        "",
        Leaves::Deleted,
    );
}

#[test]
fn a_folder_at_a_rendered_place_that_becomes_a_file_ends_its_mark() {
    a_mark_follows_its_folder(
        "rendered-place-file",
        MARKER_NAMED,
        MARKER_NAMED_FILES,
        "q\u{fffd}",
        "q\u{fffd}",
        "q\u{fffd}",
        "",
        Leaves::BecameAFile,
    );
}

#[test]
fn a_deleted_folder_at_a_rendered_place_takes_the_mark_beneath_it() {
    a_mark_follows_its_folder(
        "rendered-place-beneath-deleted",
        &["q\u{fffd}", "q\u{fffd}/lk"],
        &["q\u{fffd}/lk/a.md"],
        "q\u{fffd}",
        "q\u{fffd}/lk",
        "q\u{fffd}/lk",
        "",
        Leaves::Deleted,
    );
}

#[test]
fn a_moved_folder_at_a_rendered_place_takes_the_mark_beneath_it() {
    a_mark_follows_its_folder(
        "rendered-place-beneath-moved",
        &["q\u{fffd}", "q\u{fffd}/lk"],
        &["q\u{fffd}/lk/a.md"],
        "q\u{fffd}",
        "q\u{fffd}/lk",
        "q\u{fffd}/lk",
        "r\u{fffd}",
        Leaves::Moved,
    );
}

// **A plain folder that becomes a file ends its own mark** (NORN-479).

#[test]
fn a_marked_folder_that_becomes_a_file_ends_its_mark() {
    a_mark_follows_its_folder(
        "plain-folder-file",
        &["d"],
        &["d/a.md"],
        "d",
        "d",
        "d",
        "",
        Leaves::BecameAFile,
    );
}

#[test]
fn a_deleted_marked_folder_ends_its_mark() {
    a_mark_follows_its_folder(
        "plain-folder-deleted",
        &["d"],
        &["d/a.md"],
        "d",
        "d",
        "d",
        "",
        Leaves::Deleted,
    );
}

// **A refused folder ends its own mark**, which stands at its rendering rather
// than at a place any scope addresses.

#[test]
fn a_relisted_refused_folder_ends_its_own_mark() {
    a_mark_follows_its_folder(
        "refused-own-relisted",
        &["bad\u{1}top"],
        &["bad\u{1}top/a.md"],
        "bad\u{1}top",
        "bad\u{1}top",
        "bad\u{fffd}top",
        "",
        Leaves::Relisted,
    );
}

#[test]
fn a_deleted_refused_folder_ends_its_own_mark() {
    a_mark_follows_its_folder(
        "refused-own-deleted",
        &["bad\u{1}top"],
        &["bad\u{1}top/a.md"],
        "bad\u{1}top",
        "bad\u{1}top",
        "bad\u{fffd}top",
        "",
        Leaves::Deleted,
    );
}

#[test]
fn a_marked_folder_that_becomes_a_markdown_file_ends_its_mark() {
    a_mark_follows_its_folder(
        "markdown-folder-file",
        &["n.md"],
        &["n.md/a.md"],
        "n.md",
        "n.md",
        "n.md",
        "",
        Leaves::BecameAFile,
    );
}

#[test]
fn a_hidden_rendering_keeps_a_relisted_folders_mark() {
    let f = Fixture::new("mark-hidden-rendering");
    let Some((ops, name, progress, mut attachment)) = attach_over(
        &f,
        &["q\u{fffd}", "q\u{fffd}/lk", "q\\"],
        &["q\u{fffd}/lk/a.md", "q\\/probe.md"],
    ) else {
        return;
    };
    let root = f.vault();
    let folder = root.join("q\u{fffd}/lk");
    let Some(denial) = Denied::search(&folder, &folder.join("a.md")) else {
        ops.detach(&name, attachment);
        return;
    };
    watcher_leg(
        &ops,
        &name,
        &mut attachment,
        &progress,
        &root,
        &["q\u{fffd}"],
    );
    assert_eq!(
        sorted_kinds(&mut attachment.store, "q\u{fffd}/lk"),
        ["directory/unreadable"]
    );
    denial.restore();
    let hidden = root.join("q\\");
    let Some(_hidden) = Denied::search(&hidden, &hidden.join("probe.md")) else {
        ops.detach(&name, attachment);
        return;
    };
    watcher_leg(
        &ops,
        &name,
        &mut attachment,
        &progress,
        &root,
        &["q\\", "q\u{fffd}"],
    );
    // The visible folder lists again, but the refused, unlistable sibling
    // hides another rendering. ADR 0046 withholds this job's conclusion.
    // A build from zero sees readable lk and has no earlier mark to hold.
    assert_eq!(
        sorted_kinds(&mut attachment.store, "q\u{fffd}/lk"),
        ["directory/unreadable"]
    );
    ops.detach(&name, attachment);
}

#[test]
fn a_place_reading_keeps_the_mark_its_job_just_filed() {
    let f = Fixture::new("mark-filed-this-job");
    let Some((ops, name, _progress, mut attachment)) =
        attach_over(&f, &["q\u{fffd}"], &["q\u{fffd}/a.md"])
    else {
        return;
    };
    let root = f.vault();
    let folder = root.join("q\u{fffd}");
    let Some(denial) = Denied::search(&folder, &folder.join("a.md")) else {
        ops.detach(&name, attachment);
        return;
    };
    let vault = norn_fs::Vault::open(&root, &[]).unwrap();
    let norn_fs::WalkFact::Unreadable(fact) = vault.fact(Path::new("q\u{fffd}")).unwrap() else {
        panic!("the walk must see the unlistable folder");
    };
    let mut account = Account::default();
    {
        let mut pending = Pending::new(&mut attachment.store, 2, &root, &[], &mut account).unwrap();
        pending.unreadable_fact(&fact, None).unwrap();
        pending.flush().unwrap();
        // Deterministically stage the window between the walk's mark and its
        // place reading: permissions return, but this job's reading stands.
        denial.restore();
        pending
            .read_place(&vault, &DocumentPath::new("q\u{fffd}").unwrap())
            .unwrap();
        pending.flush().unwrap();
    }
    // A fresh build sees restored permissions, so it cannot reproduce the
    // mark this job earned before the restoration.
    assert_eq!(
        sorted_kinds(&mut attachment.store, "q\u{fffd}"),
        ["directory/unreadable"]
    );
    ops.detach(&name, attachment);
}
