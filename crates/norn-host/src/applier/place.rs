//! Where each target of a plan lands: beneath the vault root at its own
//! path, or, for the vault schema, at the file the registration reads it
//! from ([ADR 0034]).
//!
//! **A schema transition names its role, not its file.** It keeps naming
//! the default schema's path, and every phase that touches it — staging,
//! publication, a discard — resolves that role here, against the
//! [`SchemaPlace`] the apply was handed, so a schema read from a
//! `schema_source` lands at the source. A source inside the vault lands
//! beneath the root like any target. A source outside it is anchored at its
//! own folder: that folder's identity is read when the target is staged, and
//! the kernel holds publication to it as it holds every other target to the
//! vault root's.
//!
//! [ADR 0034]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0034-a-schema-write-lands-where-the-registration-reads-the-schema.md

use std::path::Path;

use norn_wire::DocumentPath;

use crate::planner::control::{SchemaPlace, control_path, role_at};

/// The ground a plan's targets land on: the vault root as it is spelled, its
/// identity, and where the vault schema lives.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ground<'a> {
    /// The vault root, as it is spelled.
    pub(crate) vault: &'a Path,
    /// The `(device, inode)` of the vault root.
    pub(crate) root: norn_fs::Identity,
    /// Where the vault schema the registration reads lives.
    pub(crate) schema: &'a SchemaPlace,
}

/// Where one target lands.
#[derive(Clone, Copy, Debug)]
pub(super) struct Landing<'a> {
    /// The folder the kernel anchors the write at.
    pub(super) anchor: &'a Path,
    /// The target, relative to `anchor`.
    pub(super) relative: &'a Path,
    /// Whether the target lies outside the vault root: its anchor's identity
    /// is read when it is staged, and no own-write ledger records it, since
    /// the ledger names vault paths only.
    pub(super) outside: bool,
}

impl<'a> Ground<'a> {
    /// Where the target at the plan path `path` lands, or why it lands
    /// nowhere: a schema source naming no file, which the check refuses
    /// before anything is staged.
    pub(super) fn landing(&self, path: &'a DocumentPath) -> Result<Landing<'a>, String> {
        let beneath = |relative: &'a Path| Landing {
            anchor: self.vault,
            relative,
            outside: false,
        };
        if role_at(path.as_str()) != Some(norn_wire::ControlFile::Schema) {
            return Ok(beneath(Path::new(path.as_str())));
        }
        debug_assert_eq!(path, control_path(norn_wire::ControlFile::Schema));
        match self.schema {
            SchemaPlace::InVault(relative) => Ok(beneath(relative)),
            SchemaPlace::Outside { folder, name, .. } => Ok(Landing {
                anchor: folder,
                relative: name,
                outside: true,
            }),
            SchemaPlace::NoFile(source) => Err(format!(
                "the schema source `{}` names no file",
                source.display()
            )),
        }
    }
}

impl Landing<'_> {
    /// The identity the kernel holds this target's anchor to: the vault
    /// root's, or, outside it, the anchor folder's as it stands now, `None`
    /// where nothing does.
    pub(super) fn root(&self, ground: &Ground<'_>) -> Result<Option<norn_fs::Identity>, String> {
        if !self.outside {
            return Ok(Some(ground.root));
        }
        norn_fs::path_identity(self.anchor).map_err(|refusal| refusal.to_string())
    }

    /// Why this target stops, in words, where the kernel found its anchor
    /// replaced and no vault root was: outside the vault, the folder it is
    /// anchored at, which is answered as an I/O failure naming that folder.
    /// `None` beneath the root, where the vault root's identity is what
    /// changed.
    pub(super) fn replaced_outside(&self) -> Option<String> {
        self.outside.then(|| {
            format!(
                "the folder `{}` holding the vault schema was replaced while the plan was applied",
                self.anchor.display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a vault path")
    }

    /// **Only the schema's role is resolved against the registration**: a
    /// document and the config land beneath the root at their own paths,
    /// and the schema at its default path, at a source inside the vault,
    /// or in the folder of one outside it.
    #[test]
    fn the_schema_lands_where_the_registration_reads_it() {
        let scratch = norn_testkit::scratch::Scratch::new("applier-place");
        let shadows = norn_fs::ShadowHome::resolve(
            scratch.root(),
            &scratch.join("data/tmp"),
            &norn_fs::MaintainershipKey::new("norn", "notes", "base").expect("a key"),
        )
        .expect("a shadow home");
        let vault = Path::new("/vault");
        let root = norn_fs::path_identity(scratch.root())
            .expect("a root")
            .expect("it stands");
        let at = |schema: &SchemaPlace, target: &str| {
            let target = path(target);
            let ground = Ground {
                vault,
                root,
                schema,
            };
            ground.landing(&target).map(|landing| {
                (
                    landing.anchor.to_owned(),
                    landing.relative.to_owned(),
                    landing.outside,
                )
            })
        };
        let default = SchemaPlace::default();
        let inside = SchemaPlace::InVault(PathBuf::from("schemas/notes.yaml"));
        let outside = SchemaPlace::Outside {
            folder: PathBuf::from("/shared"),
            name: PathBuf::from("schema.yaml"),
            shadows,
        };
        for schema in [&default, &inside, &outside] {
            assert_eq!(
                at(schema, "a.md"),
                Ok((vault.to_owned(), PathBuf::from("a.md"), false))
            );
            assert_eq!(
                at(schema, ".norn/config.toml"),
                Ok((vault.to_owned(), PathBuf::from(".norn/config.toml"), false))
            );
        }
        assert_eq!(
            at(&default, ".norn/schema.yaml"),
            Ok((vault.to_owned(), PathBuf::from(".norn/schema.yaml"), false))
        );
        assert_eq!(
            at(&inside, ".norn/schema.yaml"),
            Ok((vault.to_owned(), PathBuf::from("schemas/notes.yaml"), false))
        );
        assert_eq!(
            at(&outside, ".norn/schema.yaml"),
            Ok((PathBuf::from("/shared"), PathBuf::from("schema.yaml"), true))
        );
        assert!(
            at(
                &SchemaPlace::NoFile(PathBuf::from("/")),
                ".norn/schema.yaml"
            )
            .is_err()
        );
    }
}
