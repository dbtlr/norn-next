//! Incremental inventory for a disk-ordered background walk.

use super::*;

/// An inventoried name, keyed by the vault's path ordering. The payload is
/// re-observed through the anchored vault before its content is derived.
pub struct InventoryRecord {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// One bounded preparation step. Empty directories consume work too.
pub struct InventoryPage {
    /// Listed names, excluding dot entries.
    pub visited: u64,
    pub records: Vec<InventoryRecord>,
    pub complete: bool,
}

struct Scan {
    fd: Arc<OwnedFd>,
    relative: PathBuf,
    entries: Dir,
}

/// An unsorted, incremental inventory whose ordering is supplied on disk by
/// its caller. A directory stream is retained between claims, so each listed
/// name costs one visit rather than one visit per sorted page.
pub struct InventoryWalk {
    vault: Vault,
    stack: Vec<Scan>,
}

impl InventoryWalk {
    pub fn new(root: &Path, exclusions: &[PathBuf]) -> Result<Self, WalkError> {
        Self::from_vault(&Vault::open(root, exclusions)?)
    }

    /// Reuse an attachment's proven root instead of probing it per slice.
    pub fn from_vault(vault: &Vault) -> Result<Self, WalkError> {
        let vault = vault.clone();
        let root = vault.root.as_path();
        let entries = Dir::read_from(&vault.root_fd)
            .map_err(|error| environment_errno("reading directory", root, error))?;
        let stack = vec![Scan {
            fd: vault.root_fd.clone(),
            relative: PathBuf::new(),
            entries,
        }];
        Ok(Self { vault, stack })
    }

    pub fn case_sensitivity(&self) -> CaseSensitivity {
        self.vault.case_sensitivity()
    }

    /// Spend at most `work` enumeration acts. Directory ends and dot entries
    /// consume an act, keeping a chain of empty folders bounded as well.
    pub fn advance(&mut self, work: usize) -> Result<InventoryPage, WalkError> {
        let mut records = Vec::new();
        let mut visited = 0;
        for _ in 0..work {
            let Some(scan) = self.stack.last_mut() else {
                break;
            };
            let Some(entry) = scan.entries.next() else {
                self.stack.pop();
                continue;
            };
            let entry = entry
                .map_err(|error| environment_errno("reading entry in", &scan.relative, error))?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            crate::reads::count_dirents(1);
            visited += 1;
            let name = OsString::from_vec(name.to_vec());
            let relative = scan.relative.join(&name);
            let path = self.vault.normalize(&relative)?;
            let listed = if entry.file_type() == FileType::Unknown {
                stat_listed(&scan.fd, &name, &relative, Paged::default())?
                    .map(|stat| classify_file_type(FileType::from_raw_mode(stat.st_mode as _)))
            } else {
                Some(classify_file_type(entry.file_type()))
            };
            let observed = observe_listed(
                &self.vault.root_fd,
                &scan.fd,
                &name,
                &relative,
                &path,
                listed,
                &self.vault.normalizer,
                &self.vault.exclusions,
                &WalkFaults::default(),
            )?;
            match observed {
                Observed::Directory => {
                    match openat(&scan.fd, &name, directory_flags(), Mode::empty()) {
                        Ok(fd) => {
                            let entries = Dir::read_from(&fd).map_err(|error| {
                                environment_errno("reading directory", &relative, error)
                            })?;
                            self.stack.push(Scan {
                                fd: Arc::new(fd),
                                relative,
                                entries,
                            });
                        }
                        Err(
                            rustix::io::Errno::NOENT
                            | rustix::io::Errno::NOTDIR
                            | rustix::io::Errno::LOOP,
                        ) => records.push(record(&path)),
                        Err(error) => {
                            return Err(environment_errno("opening directory", &relative, error));
                        }
                    }
                }
                _ => records.push(record(&path)),
            }
        }
        Ok(InventoryPage {
            visited,
            records,
            complete: self.stack.is_empty(),
        })
    }

    /// Re-observe one inventoried name before deriving it. The inventory can
    /// span watcher turns, so replaying an old kind could otherwise undo a
    /// change those turns have already derived.
    pub fn replay(&self, value: &[u8]) -> Result<WalkFact, WalkError> {
        self.vault.fact(Path::new(OsStr::from_bytes(value)))
    }
}

fn record(path: &NormalizedPath) -> InventoryRecord {
    InventoryRecord {
        key: path.comparison_key().as_bytes().to_vec(),
        value: path.as_path().as_os_str().as_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReadFile, WalkFact, reads::ReadWindow, scratch::Scratch, walk};

    fn bounded_inventory(
        inventory: &mut InventoryWalk,
        budget: usize,
        expected_names: usize,
    ) -> Vec<InventoryRecord> {
        let mut records = Vec::new();
        let mut visited = 0;
        for _ in 0..4 * (expected_names + 1) {
            let window = ReadWindow::open();
            let page = inventory.advance(budget).unwrap();
            let tally = window.finish();
            assert!(tally.walk_dirents <= budget as u64, "{tally:?}");
            assert!(page.records.len() <= budget);
            assert_eq!(tally.document_opens, 0, "inventory opened content");
            assert_eq!(tally.document_bytes, 0, "inventory hashed content");
            visited += tally.walk_dirents;
            records.extend(page.records);
            if page.complete {
                assert_eq!(
                    visited, expected_names as u64,
                    "names were revisited or missed"
                );
                let window = ReadWindow::open();
                let ended = inventory.advance(budget).unwrap();
                let tally = window.finish();
                assert!(ended.complete);
                assert!(ended.records.is_empty());
                assert_eq!(tally.walk_dirents, 0, "completed inventory restarted");
                return records;
            }
        }
        panic!("inventory did not finish inside its one-visit work bound");
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree being observed.
    fn slice_enumeration_stays_bounded_at_every_position_and_tree_shape() {
        for (width, depth) in [(0, 0), (1, 0), (701, 0), (1, 64), (0, 64)] {
            let scratch = Scratch::new("inventory-tree-shapes");
            let mut relative = PathBuf::new();
            for _ in 0..depth {
                relative.push("nested");
                fs::create_dir(scratch.at("").join(&relative)).unwrap();
            }
            let mut expected = Vec::new();
            for index in 0..width {
                let path = relative.join(format!("{index:04}.md"));
                fs::write(scratch.at("").join(&path), b"body").unwrap();
                expected.push(path.as_os_str().as_bytes().to_vec());
            }
            let mut inventory = InventoryWalk::new(&scratch.at(""), &[]).unwrap();
            let mut records = bounded_inventory(&mut inventory, 1, width + depth);
            records.sort_by(|left, right| left.key.cmp(&right.key));
            let actual: Vec<_> = records.into_iter().map(|record| record.value).collect();
            assert_eq!(actual, expected, "width {width}, depth {depth}");
        }
    }

    #[test]
    fn zero_work_leaves_the_inventory_at_its_next_name() {
        let scratch = Scratch::new("inventory-zero-work");
        scratch.place("note.md", b"body");
        let mut inventory = InventoryWalk::new(&scratch.at(""), &[]).unwrap();
        let window = ReadWindow::open();
        let page = inventory.advance(0).unwrap();
        let tally = window.finish();
        assert!(!page.complete);
        assert!(page.records.is_empty());
        assert_eq!(tally.walk_dirents, 0);
        assert_eq!(tally.document_bytes, 0);
        let records = bounded_inventory(&mut inventory, 1, 1);
        assert_eq!(records[0].value, b"note.md");
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree being observed.
    fn excluded_and_link_roots_yield_one_record_and_never_visit_descendants() {
        let scratch = Scratch::new("inventory-skipped-roots");
        scratch.directory("vault/excluded/hidden");
        scratch.place("excluded/hidden/note.md", b"excluded");
        scratch.directory("vault/.norn/tmp");
        scratch.place(".norn/tmp/residue.md", b"mechanism");
        scratch.directory("vault/visible");
        scratch.place("visible/note.md", b"visible");
        let outside = scratch.directory("outside");
        fs::write(outside.join("note.md"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, scratch.at("outbound")).unwrap();
        std::os::unix::fs::symlink("visible", scratch.at("inside")).unwrap();
        let mut inventory =
            InventoryWalk::new(&scratch.at(""), &[PathBuf::from("excluded")]).unwrap();
        let mut records = bounded_inventory(&mut inventory, 2, 7);
        records.sort_by(|left, right| left.key.cmp(&right.key));
        let paths: Vec<_> = records
            .iter()
            .map(|record| record.value.as_slice())
            .collect();
        assert_eq!(
            paths,
            [
                b".norn/tmp".as_slice(),
                b"excluded",
                b"inside",
                b"outbound",
                b"visible/note.md",
            ]
        );
        let window = ReadWindow::open();
        for record in &records {
            let fact = inventory.replay(&record.value).unwrap();
            match record.value.as_slice() {
                b".norn/tmp" => assert!(matches!(fact,
                    WalkFact::Skipped(skip) if skip.reason() == SkipReason::Mechanism)),
                b"excluded" => assert!(matches!(fact,
                    WalkFact::Skipped(skip) if skip.reason() == SkipReason::HostExclusion)),
                b"inside" | b"outbound" => assert!(matches!(fact,
                    WalkFact::Skipped(skip) if matches!(skip.reason(), SkipReason::SymbolicLink(_)))),
                b"visible/note.md" => assert!(matches!(fact, WalkFact::File(_))),
                _ => unreachable!(),
            }
        }
        let tally = window.finish();
        assert_eq!(tally.document_opens, 0);
        assert_eq!(tally.document_bytes, 0);
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree being observed.
    fn replay_reobserves_a_file_deleted_after_inventory() {
        let scratch = Scratch::new("inventory-replay-deleted");
        scratch.place("gone.md", b"before");
        let mut inventory = InventoryWalk::new(&scratch.at(""), &[]).unwrap();
        let records = bounded_inventory(&mut inventory, 1, 1);
        fs::remove_file(scratch.at("gone.md")).unwrap();
        let window = ReadWindow::open();
        let fact = inventory.replay(&records[0].value).unwrap();
        let tally = window.finish();
        assert!(matches!(fact, WalkFact::Skipped(skip)
            if skip.reason() == SkipReason::Vanished
                && skip.path().as_path() == Path::new("gone.md")));
        assert_eq!(tally.document_opens, 0);
        assert_eq!(tally.document_bytes, 0);
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree being observed.
    fn replay_reobserves_an_ancestor_replaced_by_an_outside_symlink() {
        let scratch = Scratch::new("inventory-replay-ancestor-link");
        scratch.directory("vault/nested");
        scratch.place("nested/note.md", b"before");
        let outside = scratch.directory("outside");
        fs::write(outside.join("note.md"), b"outside must not be read").unwrap();
        let mut inventory = InventoryWalk::new(&scratch.at(""), &[]).unwrap();
        let records = bounded_inventory(&mut inventory, 1, 2);
        fs::remove_file(scratch.at("nested/note.md")).unwrap();
        fs::remove_dir(scratch.at("nested")).unwrap();
        std::os::unix::fs::symlink(&outside, scratch.at("nested")).unwrap();

        let window = ReadWindow::open();
        let fact = inventory.replay(&records[0].value).unwrap();
        let tally = window.finish();
        assert_eq!(tally.document_opens, 0);
        assert_eq!(tally.document_bytes, 0);
        let WalkFact::Skipped(skip) = fact else {
            panic!("replay followed an inventoried ancestor's replacement link");
        };
        assert_eq!(skip.path().as_path(), Path::new("nested"));
        assert_eq!(skip.reason(), SkipReason::SymbolicLink(LinkKind::Outbound));
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree being observed.
    fn inventory_preserves_and_orders_non_utf8_names_without_reading_content() {
        let scratch = Scratch::new("inventory-raw-names");
        let expected: &[&[u8]] = &[b"a.md", b"n\x80.md", b"n\xff.md", b"z.md"];
        for name in expected.iter().rev() {
            match fs::write(scratch.at("").join(OsStr::from_bytes(name)), b"raw") {
                Ok(()) => {}
                Err(error)
                    if std::str::from_utf8(name).is_err()
                        && error.raw_os_error() == Some(libc::EILSEQ) =>
                {
                    eprintln!("skipped: scratch filesystem refuses non-UTF-8 names: {error}");
                    return;
                }
                Err(error) => panic!("could not create inventory fixture {name:?}: {error}"),
            }
        }
        let mut inventory = InventoryWalk::new(&scratch.at(""), &[]).unwrap();
        let mut records = bounded_inventory(&mut inventory, 1, 4);
        records.sort_by(|left, right| left.key.cmp(&right.key));
        let actual: Vec<_> = records
            .iter()
            .map(|record| record.value.as_slice())
            .collect();
        assert_eq!(actual, expected);
        let window = ReadWindow::open();
        for record in &records {
            let WalkFact::File(file) = inventory.replay(&record.value).unwrap() else {
                panic!("raw file name was lost in replay");
            };
            assert_eq!(file.path().as_path().as_os_str().as_bytes(), record.value);
            assert_eq!(file.read_optional().unwrap().unwrap().bytes(), b"raw");
        }
        let tally = window.finish();
        assert_eq!(tally.document_opens, 4);
        assert_eq!(tally.document_bytes, 12);
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree being observed.
    fn replay_reobserves_a_file_replaced_by_an_outside_symlink() {
        let scratch = Scratch::new("inventory-replay-symlink");
        scratch.place("note.md", b"before");
        let outside = scratch.path("outside.md");
        fs::write(&outside, b"outside must not be read").unwrap();
        let root = scratch.at("");
        let mut inventory = InventoryWalk::new(&root, &[]).unwrap();
        let record = loop {
            let page = inventory.advance(1).unwrap();
            if let Some(record) = page.records.into_iter().next() {
                break record;
            }
            assert!(!page.complete, "the inventoried file was never yielded");
        };
        fs::remove_file(scratch.at("note.md")).unwrap();
        std::os::unix::fs::symlink(&outside, scratch.at("note.md")).unwrap();

        let window = ReadWindow::open();
        let fact = inventory.replay(&record.value).unwrap();
        let tally = window.finish();
        assert_eq!(tally.document_opens, 0, "replay read symlink content");
        assert_eq!(tally.document_bytes, 0, "replay read outside bytes");
        let WalkFact::Skipped(skip) = fact else {
            panic!("replay retained the inventoried file kind");
        };
        assert_eq!(skip.path().as_path(), Path::new("note.md"));
        assert_eq!(skip.reason(), SkipReason::SymbolicLink(LinkKind::Outbound));
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging the tree being observed.
    fn inventory_visits_each_name_once_in_bounded_steps_and_replays_the_walk() {
        let scratch = Scratch::new("incremental-inventory");
        scratch.directory("vault/nested");
        for index in 0..700 {
            scratch.place(&format!("{index:04}.md"), b"body");
        }
        scratch.place("nested/leaf.md", b"nested");
        std::os::unix::fs::symlink("missing", scratch.at("link")).unwrap();
        let root = scratch.path("vault");
        let expected: Vec<_> = walk(&root, &[])
            .unwrap()
            .map(|fact| fact.unwrap().path().as_path().to_owned())
            .collect();
        let mut inventory = InventoryWalk::new(&root, &[]).unwrap();
        let mut records = Vec::new();
        let mut visited = 0;
        loop {
            let window = ReadWindow::open();
            let page = inventory.advance(7).unwrap();
            let tally = window.finish();
            assert!(tally.walk_dirents <= 7, "{tally:?}");
            assert!(page.records.len() <= 7);
            visited += tally.walk_dirents;
            records.extend(page.records);
            if page.complete {
                break;
            }
        }
        assert_eq!(visited, 703);
        records.sort_by(|a, b| a.key.cmp(&b.key));
        let actual: Vec<_> = records
            .iter()
            .map(|record| {
                let fact = inventory.replay(&record.value).unwrap();
                if let WalkFact::File(file) = &fact {
                    assert_eq!(
                        file.stat().len,
                        if file.path().as_path() == std::path::Path::new("nested/leaf.md") {
                            6
                        } else {
                            4
                        }
                    );
                }
                fact.path().as_path().to_owned()
            })
            .collect();
        assert_eq!(actual, expected);
        let file = records
            .iter()
            .find_map(|record| match inventory.replay(&record.value).unwrap() {
                WalkFact::File(file) => Some(file),
                _ => None,
            })
            .unwrap();
        let read: ReadFile = file.read_optional().unwrap().unwrap();
        assert_eq!(read.bytes(), b"body");
    }
}
