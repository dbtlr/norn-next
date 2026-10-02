//! Composition: each target's after-bytes from its before-bytes and the
//! operations touching it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use norn_fs::NormalizedPath;
use norn_text::RewriteSkip;
use norn_wire::{
    ContentHash, DocumentPath, FileState, LinkFamily, LinkRewrite, Operation, OperationKind,
};

use super::edit;
use super::links::{self, wire_family};
use super::view::{Entry, VaultView, document_path, wire_hash};
use crate::derivation::{decodes, document_links};

/// What composing a plan's operations came to.
pub(crate) struct Composition {
    /// One per file an operation touches, by the spelling the plan writes it
    /// at.
    pub(crate) targets: BTreeMap<DocumentPath, ComposedTarget>,
    /// The spelling each identity the plan touches was read at, whose target
    /// carries what the vault held there before the plan.
    pub(crate) read_at: BTreeMap<NormalizedPath, DocumentPath>,
    /// Each operation that did not resolve against the state it met.
    pub(crate) unresolvable: Vec<Unresolvable>,
    /// Each link a cascade's rewrite matched and left as written, in the
    /// order the cascades composed.
    pub(crate) skipped: Vec<Skipped>,
    /// Each link a holder's rewrites left as written without matching any of
    /// them, at an address one of them writes, in the order the holders
    /// composed.
    pub(crate) kept: Vec<Kept>,
}

/// A link no rewrite of its holder's batch matched, written at an address a
/// rewrite of that batch respells another link to.
///
/// **Why it is named.** The change set keys a link by its holder, syntax and
/// address, so this link and the one respelled to its address are one key,
/// and every link under a key a rewrite writes reads as written. The
/// forecast says of a link the cascade left behind, or left for its
/// ambiguity, why it stayed; this names the key that still holds such a
/// link beside a written one, so its change of meaning is said. Keyed as
/// [`Skipped`] is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Kept {
    pub(crate) holder: DocumentPath,
    pub(crate) syntax: LinkFamily,
    pub(crate) address: String,
}

/// A link a cascade's rewrite matched and the text layer left as written.
///
/// **Keyed as the change set keys it**: the holder at the spelling the plan
/// writes it, the syntax, and the address the link is still written with —
/// the rewrite's `from`. The link keeps its entry in the plan's resolution
/// change set, and the forecast says why it stayed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Skipped {
    pub(crate) holder: DocumentPath,
    pub(crate) syntax: LinkFamily,
    pub(crate) address: String,
    pub(crate) reason: RewriteSkip,
}

impl Composition {
    /// What the vault held at `identity` before the plan, where the plan
    /// touches it.
    pub(crate) fn before(&self, identity: &NormalizedPath) -> Option<&FileState> {
        let spelling = self.read_at.get(identity)?;
        Some(&self.targets[spelling].before)
    }
}

/// One file's two sides.
pub(crate) struct ComposedTarget {
    /// What the file held before the plan.
    pub(crate) before: FileState,
    /// What it holds after, `None` where it is absent.
    pub(crate) after: Option<Arc<[u8]>>,
}

/// An operation that met a state it cannot act on.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Unresolvable {
    /// Its position in the plan's operation list.
    pub(crate) position: usize,
    /// Why, in words.
    pub(crate) detail: String,
}

/// Compose `operations`, taken in `order`, over what `view` holds.
///
/// **A cascade composes after every operation.** An operation and its link
/// cascade are one operation, but each of the cascade's rewrites is an edit
/// in place of a holder named at the spelling it holds after the plan, so
/// the rewrites of every operation that acted compose once all operations
/// have: a holder another operation edits is rewritten on its final bytes,
/// and a link a moved document holds is rewritten at its destination.
/// Planning read each link a cascade rewrites from exactly those bytes, so
/// generating a cascade and composing it are one function of the same state.
///
/// **A holder's rewrites compose as one batch.** Every rewrite naming one
/// holder, whichever operation's cascade carries it, is composed by one call
/// over the holder's final bytes ([`edit::rewritten`]), each link matched as
/// those bytes write it, so no rewrite respells a link another one wrote and
/// the order the operations compose in says nothing about what a holder
/// reads. A cascade's rewrite never fails; a cascade naming a holder that
/// holds no document leaves its operation unresolvable whole, its rewrites
/// composing nowhere, since planning names only holders that stand.
///
/// **An authored link rewrite joins its holder's batch.** A `rewrite_link`
/// names its document where the plan leaves it, as a cascade's rewrite
/// does, and composes in the same batch as every cascade rewrite naming that
/// document. It must match: where no document stands there, or no link of
/// its syntax there is written `from`, it is unresolvable, as an edit whose
/// text does not occur is, rather than landing as a change of nothing. One
/// matching a link the text layer leaves as written, or respelling a link to
/// what it already holds, acts, and its document may land found.
pub(crate) fn compose<V: VaultView>(
    operations: &[Operation],
    order: &[usize],
    view: &V,
) -> Result<Composition, V::Error> {
    let mut vault = Simulated::over(view);
    let mut unresolvable = Vec::new();
    for &position in order {
        if let Err(detail) = vault.apply(&operations[position].kind)? {
            unresolvable.push(Unresolvable { position, detail });
        }
    }
    let failed: std::collections::BTreeSet<usize> = unresolvable
        .iter()
        .map(|unresolvable: &Unresolvable| unresolvable.position)
        .collect();
    let acted = || order.iter().filter(|position| !failed.contains(position));
    // Each authored link rewrite, as the rewrite it writes into its holder's
    // batch.
    let authored: Vec<(usize, LinkRewrite)> = acted()
        .filter_map(|&position| match &operations[position].kind {
            OperationKind::RewriteLink {
                path,
                syntax,
                from,
                to,
            } => Some((
                position,
                LinkRewrite::new(path.clone(), *syntax, from.clone(), to.clone()),
            )),
            _ => None,
        })
        .collect();
    let mut holders: BTreeMap<DocumentPath, Vec<&LinkRewrite>> = BTreeMap::new();
    for &position in acted() {
        match vault.holders(&operations[position].cascade)? {
            Ok(named) => {
                for (holder, rewrite) in named {
                    holders.entry(holder).or_default().push(rewrite);
                }
            }
            Err(detail) => unresolvable.push(Unresolvable { position, detail }),
        }
    }
    for (position, rewrite) in &authored {
        match vault.matching(rewrite)? {
            Ok(holder) => holders.entry(holder).or_default().push(rewrite),
            Err(detail) => unresolvable.push(Unresolvable {
                position: *position,
                detail,
            }),
        }
    }
    for (holder, rewrites) in holders {
        vault.rewrite(&holder, &rewrites);
    }
    Ok(Composition {
        targets: vault.targets,
        read_at: vault.read_at,
        unresolvable,
        skipped: vault.skipped,
        kept: vault.kept,
    })
}

/// The files the plan touches, each as it stood before and as it stands so
/// far, over the view the before-states are read from.
///
/// **A file is its identity.** Every name an operation carries is normalized
/// by the view's one rule, so two spellings of one file — `a//b.md` and
/// `a/b.md`, or on a root that folds case `A.md` and `a.md` — are one file,
/// held at the spelling the tree lists. The one act that gives an identity a
/// second spelling is a case-only rename, which writes the file at its new
/// spelling and takes it away at its old one.
struct Simulated<'view, V> {
    view: &'view V,
    targets: BTreeMap<DocumentPath, ComposedTarget>,
    /// The spelling each identity was first read at.
    read_at: BTreeMap<NormalizedPath, DocumentPath>,
    /// The spelling each identity stands at now: where it was read, or where a
    /// case-only rename moved it.
    spelled: BTreeMap<NormalizedPath, DocumentPath>,
    /// How many documents stand so far beneath each folder identity, so
    /// whether a name is a folder the plan makes is one lookup however large
    /// the plan.
    standing_below: BTreeMap<NormalizedPath, usize>,
    /// The spelling each folder identity was first given by a document
    /// standing beneath it: the tree's, or the one an operation made it at.
    /// Publication makes a folder before the create it is made for and
    /// removes none until the end, so a folder keeps that spelling for the
    /// whole plan. The spelling is kept even when a later operation of the
    /// plan removes the document that gave it, so no publication makes the
    /// folder: a second spelling is then refused where it could have stood,
    /// which costs the author a re-plan and never a wrong transition.
    folder_spelled: BTreeMap<NormalizedPath, PathBuf>,
    /// Each link a cascade's rewrite left as written, so far.
    skipped: Vec<Skipped>,
    /// Each link a holder's batch kept at an address it writes, so far.
    kept: Vec<Kept>,
}

/// Why one operation cannot act on the state it met, in words.
pub(crate) type Unresolved = String;

/// Each rewrite of a cascade beside the spelling of the holder it names.
type HeldRewrites<'c> = Vec<(DocumentPath, &'c LinkRewrite)>;

/// Where one name an operation carries leads.
enum Place {
    /// A file the plan composes, at the spelling it is written at.
    File(NormalizedPath, DocumentPath),
    /// A place no document is read from or made at, and why.
    NoFile(Unresolved),
}

impl<'view, V: VaultView> Simulated<'view, V> {
    fn over(view: &'view V) -> Self {
        Simulated {
            view,
            targets: BTreeMap::new(),
            read_at: BTreeMap::new(),
            spelled: BTreeMap::new(),
            standing_below: BTreeMap::new(),
            folder_spelled: BTreeMap::new(),
            skipped: Vec::new(),
            kept: Vec::new(),
        }
    }

    /// Where `path` leads, reading its before-state the first time the plan
    /// touches its identity.
    fn place(&mut self, path: &DocumentPath) -> Result<Place, V::Error> {
        let identity = match self.view.normalizer().normalize(Path::new(path.as_str())) {
            Ok(identity) => identity,
            Err(error) => {
                return Ok(Place::NoFile(format!(
                    "`{path}` names no document in the vault: {error}"
                )));
            }
        };
        if let Some(spelling) = self.spelled.get(&identity) {
            return Ok(Place::File(identity, spelling.clone()));
        }
        let (spelling, before, after) = match self.view.entry(&identity)? {
            Entry::Document { at, bytes, hash } => (at, holding(&bytes, hash), Some(bytes)),
            Entry::Absent { at } => (at, FileState::absent(), None),
            Entry::Folder => {
                return Ok(Place::NoFile(format!(
                    "a folder stands at `{path}`, where a document would be"
                )));
            }
            Entry::Blocked { detail, .. } => return Ok(Place::NoFile(detail)),
        };
        self.targets.insert(
            spelling.clone(),
            ComposedTarget {
                before,
                after: None,
            },
        );
        self.set_after(&spelling, after);
        self.read_at.insert(identity.clone(), spelling.clone());
        self.spelled.insert(identity.clone(), spelling.clone());
        Ok(Place::File(identity, spelling))
    }

    /// The document standing so far at the file `path` leads to, or why none
    /// does.
    fn standing(
        &mut self,
        path: &DocumentPath,
    ) -> Result<Result<DocumentPath, Unresolved>, V::Error> {
        Ok(match self.place(path)? {
            Place::NoFile(detail) => Err(detail),
            Place::File(_, spelling) if self.targets[&spelling].after.is_some() => Ok(spelling),
            Place::File(..) => Err(format!("no document stands at `{path}`")),
        })
    }

    /// The file a document can be put at for `path`, or why none can: the
    /// name holds a document, a folder the plan makes, or lies beneath a
    /// document; or `path` spells a folder above it differently from the tree
    /// or from the operation that made it.
    fn vacant(
        &mut self,
        path: &DocumentPath,
    ) -> Result<Result<DocumentPath, Unresolved>, V::Error> {
        let (identity, spelling) = match self.place(path)? {
            Place::NoFile(detail) => return Ok(Err(detail)),
            Place::File(identity, spelling) => (identity, spelling),
        };
        if self.targets[&spelling].after.is_some() {
            return Ok(Err(format!("a document stands at `{path}`")));
        }
        if let Some(above) = self.document_above(&identity) {
            return Ok(Err(format!(
                "`{path}` lies beneath the document the plan puts at `{above}`"
            )));
        }
        if self
            .standing_below
            .get(&identity)
            .is_some_and(|&count| count > 0)
        {
            return Ok(Err(format!(
                "a folder stands at `{path}`, where the plan puts a document beneath it"
            )));
        }
        if let Some(made) = self.folder_spelled_otherwise(&identity) {
            return Ok(Err(format!(
                "`{path}` runs through the folder the plan puts at `{}`: a document is put at the spelling its folder already has, and a folder's change of case is not planned",
                made.display()
            )));
        }
        if spelling.as_str() != spelled_as_asked(&identity) {
            return Ok(Err(format!(
                "`{path}` is spelled `{spelling}` in the vault: a document is put at the spelling the vault lists, and a folder's change of case is not planned"
            )));
        }
        Ok(Ok(spelling))
    }

    /// The document standing so far at a folder above `identity`.
    fn document_above(&self, identity: &NormalizedPath) -> Option<&DocumentPath> {
        identity
            .as_path()
            .ancestors()
            .skip(1)
            .filter(|above| !above.as_os_str().is_empty())
            .filter_map(|above| self.view.normalizer().normalize(above).ok())
            .filter_map(|above| self.spelled.get(&above))
            .find(|spelling| self.targets[*spelling].after.is_some())
    }

    /// The spelling of a folder above `identity` that this plan has given
    /// another spelling than `identity` asks for.
    fn folder_spelled_otherwise(&self, identity: &NormalizedPath) -> Option<&PathBuf> {
        identity
            .as_path()
            .ancestors()
            .skip(1)
            .filter(|above| !above.as_os_str().is_empty())
            .find_map(|above| {
                let folder = self.view.normalizer().normalize(above).ok()?;
                self.folder_spelled
                    .get(&folder)
                    .filter(|made| made.as_path() != above)
            })
    }

    /// Set what the file at `spelling` holds so far, counting it beneath
    /// every folder above it while a document stands there.
    fn set_after(&mut self, spelling: &DocumentPath, after: Option<Arc<[u8]>>) {
        let target = self.target(spelling);
        let change = match (target.after.is_some(), after.is_some()) {
            (false, true) => Some(true),
            (true, false) => Some(false),
            _ => None,
        };
        target.after = after;
        let Some(arrives) = change else {
            return;
        };
        for above in Path::new(spelling.as_str()).ancestors().skip(1) {
            if above.as_os_str().is_empty() {
                break;
            }
            let Ok(folder) = self.view.normalizer().normalize(above) else {
                continue;
            };
            if arrives {
                self.folder_spelled
                    .entry(folder.clone())
                    .or_insert_with(|| above.to_owned());
            }
            let count = self.standing_below.entry(folder).or_default();
            if arrives {
                *count += 1;
            } else {
                *count -= 1;
            }
        }
    }

    /// Act on `kind`, or say why it cannot act, leaving the state as it was.
    fn apply(&mut self, kind: &OperationKind) -> Result<Result<(), Unresolved>, V::Error> {
        Ok(match kind {
            OperationKind::CreateDocument { path, content } => self.vacant(path)?.map(|spelling| {
                self.set_after(&spelling, Some(Arc::from(content.as_bytes())));
            }),
            OperationKind::StrReplace {
                path,
                old_str,
                new_str,
            } => match self.standing(path)? {
                Err(detail) => Err(detail),
                Ok(spelling) => {
                    let file = self.target(&spelling);
                    let bytes = file.after.as_ref().expect("a document stands");
                    replace_once(bytes, old_str, new_str).map(|replaced| {
                        file.after = Some(replaced);
                    })
                }
            },
            OperationKind::MoveDocument { from, to } => self.move_document(from, to)?,
            // What becomes of the links naming the document is its link
            // cascade's, which composes after every operation, and planning's
            // to judge (`super::cascade`): the removal is the same whatever
            // the delete says of them.
            OperationKind::DeleteDocument { path, .. } => self.standing(path)?.map(|spelling| {
                self.set_after(&spelling, None);
            }),
            // A wikilink rewrite touches nothing itself: what it writes is
            // its cascade, which composes after every operation, and which
            // wikilinks it retargets is planning's to judge
            // (`super::cascade`).
            OperationKind::RewriteWikilink { .. } => Ok(()),
            // An authored link rewrite composes with its holder's batch,
            // after every operation, where the plan leaves its document
            // ([`compose`]).
            OperationKind::RewriteLink { .. } => Ok(()),
            // Planning expands a folder move into the document moves it
            // makes before anything composes (`super::expand`), so only a
            // plan resolved without expansion meets one here, which the
            // applier refuses first as an unexpanded target.
            OperationKind::MoveFolder { from, .. } => Err(format!(
                "the folder move from `{from}` is planned only as the document moves it expands into, and was not expanded"
            )),
            // Planning expands a creation by rule into the `create_document`
            // its rule makes before anything composes (`super::rule`), so
            // only a plan resolved without expansion meets one here, which
            // the applier refuses first as an unexpanded rule.
            OperationKind::CreateByRule { .. } => Err(
                "a creation by rule is planned only as the `create_document` it expands into, and was not expanded"
                    .to_string(),
            ),
            OperationKind::SetFrontmatter { .. }
            | OperationKind::RemoveFrontmatter { .. }
            | OperationKind::PushFrontmatter { .. }
            | OperationKind::PopFrontmatter { .. }
            | OperationKind::ReplaceBody { .. }
            | OperationKind::ReplaceSection { .. }
            | OperationKind::AppendToSection { .. }
            | OperationKind::DeleteSection { .. }
            | OperationKind::InsertBeforeHeading { .. }
            | OperationKind::InsertAfterHeading { .. } => self.edit_in_place(kind)?,
        })
    }

    /// Edit the document a document-local `kind` names where it stands, as a
    /// pure function of what it holds so far (see [`super::edit`]).
    fn edit_in_place(&mut self, kind: &OperationKind) -> Result<Result<(), Unresolved>, V::Error> {
        let path = match edit::local_target(kind).expect("a document-local kind") {
            Ok(path) => path,
            Err(detail) => return Ok(Err(detail)),
        };
        if let Some(detail) = edit::refused_whatever_the_document(kind) {
            return Ok(Err(detail));
        }
        let spelling = match self.standing(path)? {
            Ok(spelling) => spelling,
            Err(detail) => return Ok(Err(detail)),
        };
        let file = self.target(&spelling);
        let bytes = file.after.as_ref().expect("a document stands");
        Ok(edit::edited(kind, bytes).map(|edited| {
            file.after = Some(edited);
        }))
    }

    /// Move the document at `from` to `to`.
    ///
    /// **A case-only rename is a move.** On a root that folds case a
    /// destination differing from its source only in case names the source
    /// itself (ADR 0032), so the destination is not an occupied name: the
    /// document is written at the new spelling and taken away at the old, two
    /// transitions the applier publishes as one respell. A destination whose
    /// spelling is the source's own is a move onto itself, which names no
    /// change.
    fn move_document(
        &mut self,
        from: &DocumentPath,
        to: &DocumentPath,
    ) -> Result<Result<(), Unresolved>, V::Error> {
        let source = match self.standing(from)? {
            Ok(source) => source,
            Err(detail) => return Ok(Err(detail)),
        };
        let identity = |path: &DocumentPath| {
            self.view
                .normalizer()
                .normalize(Path::new(path.as_str()))
                .ok()
        };
        let (from_identity, to_identity) = (identity(from), identity(to));
        let destination = if let Some(to_identity) = to_identity
            && Some(&to_identity) == from_identity.as_ref()
        {
            let respelled = spelled_as_asked(&to_identity);
            if respelled == source.as_str() {
                return Ok(Err(format!("`{from}` would be moved onto itself")));
            }
            if Path::new(&respelled).parent() != Path::new(source.as_str()).parent() {
                return Ok(Err(format!(
                    "`{to}` differs from `{source}` in the case of a folder, and a folder's change of case is not planned"
                )));
            }
            // The source already passed the index's grammar, and a change of
            // ASCII case alone never changes that grammar's verdict.
            let respelled = DocumentPath::new(&respelled).expect("a normalized document path");
            self.targets
                .entry(respelled.clone())
                .or_insert(ComposedTarget {
                    before: FileState::absent(),
                    after: None,
                });
            self.spelled.insert(to_identity, respelled.clone());
            respelled
        } else {
            match self.vacant(to)? {
                Ok(destination) => destination,
                Err(detail) => return Ok(Err(detail)),
            }
        };
        let moved = self.target(&source).after.clone();
        self.set_after(&source, None);
        self.set_after(&destination, moved);
        Ok(Ok(()))
    }

    /// The holder each rewrite of `cascade` names, at the spelling it stands
    /// at so far, or why one of them holds no document to rewrite.
    fn holders<'c>(
        &mut self,
        cascade: &'c [LinkRewrite],
    ) -> Result<Result<HeldRewrites<'c>, Unresolved>, V::Error> {
        let mut named = Vec::with_capacity(cascade.len());
        for rewrite in cascade {
            match self.standing(&rewrite.path)? {
                Ok(spelling) => named.push((spelling, rewrite)),
                Err(detail) => {
                    return Ok(Err(format!(
                        "its link cascade rewrites `{}`, where it cannot: {detail}",
                        rewrite.path
                    )));
                }
            }
        }
        Ok(Ok(named))
    }

    /// The spelling of the document the authored link rewrite `rewrite`
    /// names where it stands so far, or why it acts on none there: no
    /// document stands there, or none of its links of the rewrite's syntax
    /// is written with its `from`.
    fn matching(
        &mut self,
        rewrite: &LinkRewrite,
    ) -> Result<Result<DocumentPath, Unresolved>, V::Error> {
        let spelling = match self.standing(&rewrite.path)? {
            Ok(spelling) => spelling,
            Err(detail) => return Ok(Err(detail)),
        };
        let bytes = self
            .target(&spelling)
            .after
            .as_ref()
            .expect("a document stands");
        let matches = document_links(bytes).iter().any(|link| {
            wire_family(link.family) == rewrite.syntax && links::address(link) == rewrite.from
        });
        Ok(if matches {
            Ok(spelling)
        } else {
            let syntax = match rewrite.syntax {
                LinkFamily::Wikilink => "wikilink",
                LinkFamily::Markdown => "Markdown link",
                _ => "link of its syntax",
            };
            Err(format!(
                "no {syntax} in `{}` is written `{}`, so the rewrite changes nothing",
                rewrite.path, rewrite.from
            ))
        })
    }

    /// Respell, in the document standing at `spelling`, every link each of
    /// `rewrites` names, all at once, recording each matching link the text
    /// layer leaves as written under the address it is still written with,
    /// and each link no rewrite matches at an address one of them writes.
    fn rewrite(&mut self, spelling: &DocumentPath, rewrites: &[&LinkRewrite]) {
        let file = self.target(spelling);
        let bytes = file.after.as_ref().expect("a document stands");
        let kept = kept_at_written(bytes, rewrites);
        let (rewritten, skipped) = edit::rewritten(bytes, rewrites.iter().copied());
        file.after = Some(rewritten);
        self.skipped.extend(skipped.into_iter().map(|skip| Skipped {
            holder: spelling.clone(),
            syntax: match skip.link.family {
                norn_text::LinkFamily::Wikilink => LinkFamily::Wikilink,
                norn_text::LinkFamily::Markdown => LinkFamily::Markdown,
            },
            address: match &skip.link.protocol {
                Some(protocol) => format!("{protocol}://{}", skip.link.target),
                None => skip.link.target.clone(),
            },
            reason: skip.reason,
        }));
        self.kept
            .extend(kept.into_iter().map(|(syntax, address)| Kept {
                holder: spelling.clone(),
                syntax,
                address,
            }));
    }

    fn target(&mut self, spelling: &DocumentPath) -> &mut ComposedTarget {
        self.targets
            .get_mut(spelling)
            .expect("a placed file has a target")
    }
}

/// The syntax and address of each link `bytes` hold, as the change set reads
/// them, that no rewrite of `rewrites` matches and that one of them writes:
/// what a batch over `bytes` keeps under a key it also writes.
fn kept_at_written(bytes: &[u8], rewrites: &[&LinkRewrite]) -> Vec<(LinkFamily, String)> {
    let matches = |syntax: LinkFamily, address: &str| {
        rewrites
            .iter()
            .any(|rewrite| rewrite.syntax == syntax && rewrite.from == address)
    };
    let writes = |syntax: LinkFamily, address: &str| {
        rewrites
            .iter()
            .any(|rewrite| rewrite.syntax == syntax && rewrite.to == address)
    };
    let mut kept: Vec<(LinkFamily, String)> = Vec::new();
    for link in document_links(bytes) {
        let syntax = wire_family(link.family);
        let address = links::address(&link);
        if writes(syntax, &address)
            && !matches(syntax, &address)
            && !kept.contains(&(syntax, address.clone()))
        {
            kept.push((syntax, address));
        }
    }
    kept
}

/// `identity` at the spelling its operation asked for, normalized.
fn spelled_as_asked(identity: &NormalizedPath) -> String {
    document_path(identity.as_path())
        .map(|path| path.as_str().to_string())
        .unwrap_or_default()
}

/// Whether `kind` edits a document in place, changing the content the file
/// it names already holds: a `str_replace` and every document-local kind,
/// each where the document stands when it composes, and an authored link
/// rewrite, where the plan leaves the document it names. Such an edit names
/// no name it fills or empties and carries no content from another file.
pub(crate) fn edits_in_place(kind: &OperationKind) -> bool {
    match kind {
        OperationKind::StrReplace { .. } | OperationKind::RewriteLink { .. } => true,
        other => edit::local_target(other).is_some(),
    }
}

/// The files an operation touches: a move touches its source and its
/// destination, a frontmatter kind with a `where` target none until planning
/// expands it, a folder move, a wikilink rewrite and a creation by rule none —
/// each names its documents only once planning expands it — and every other
/// kind the one file it names.
pub(crate) fn touches(kind: &OperationKind) -> impl Iterator<Item = &DocumentPath> {
    let (first, second) = match kind {
        OperationKind::CreateDocument { path, .. }
        | OperationKind::StrReplace { path, .. }
        | OperationKind::DeleteDocument { path, .. }
        | OperationKind::RewriteLink { path, .. }
        | OperationKind::ReplaceBody { path, .. }
        | OperationKind::ReplaceSection { path, .. }
        | OperationKind::AppendToSection { path, .. }
        | OperationKind::DeleteSection { path, .. }
        | OperationKind::InsertBeforeHeading { path, .. }
        | OperationKind::InsertAfterHeading { path, .. } => (Some(path), None),
        OperationKind::SetFrontmatter { target, .. }
        | OperationKind::RemoveFrontmatter { target, .. }
        | OperationKind::PushFrontmatter { target, .. }
        | OperationKind::PopFrontmatter { target, .. } => (target.as_path(), None),
        OperationKind::MoveDocument { from, to } => (Some(from), Some(to)),
        OperationKind::MoveFolder { .. }
        | OperationKind::RewriteWikilink { .. }
        | OperationKind::CreateByRule { .. } => (None, None),
    };
    first.into_iter().chain(second)
}

/// The files `operation` touches: those its kind names ([`touches`]), then
/// each holder its link cascade rewrites. An operation and its cascade are
/// one operation, so every file either names stands or falls with it, and
/// every such file carries a transition.
pub(crate) fn touched(operation: &Operation) -> impl Iterator<Item = &DocumentPath> {
    touches(&operation.kind).chain(operation.cascade.iter().map(|rewrite| &rewrite.path))
}

/// `bytes` with the one occurrence of `old` replaced by `new`.
///
/// **Exactly one occurrence, counted at every offset.** An edit's anchor names
/// one place: text found nowhere, or at more than one offset — overlapping
/// offsets included — names none, and so does the empty text, which occurs at
/// every offset. The search is over bytes, so a document that is not UTF-8 is
/// edited where the anchor's bytes occur.
fn replace_once(bytes: &[u8], old: &str, new: &str) -> Result<Arc<[u8]>, Unresolved> {
    let needle = old.as_bytes();
    if needle.is_empty() {
        return Err("an edit's text is empty, so it names no one place".to_string());
    }
    let mut offsets = bytes
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(offset, _)| offset);
    let Some(at) = offsets.next() else {
        return Err(format!("the text `{old}` no longer occurs in the document"));
    };
    if offsets.next().is_some() {
        return Err(format!(
            "the text `{old}` occurs more than once in the document"
        ));
    }
    let mut replaced = Vec::with_capacity(bytes.len() - old.len() + new.len());
    replaced.extend_from_slice(&bytes[..at]);
    replaced.extend_from_slice(new.as_bytes());
    replaced.extend_from_slice(&bytes[at + old.len()..]);
    Ok(Arc::from(replaced))
}

/// The wire's content hash of `bytes`.
pub(crate) fn content_hash(bytes: &[u8]) -> ContentHash {
    wire_hash(norn_fs::ContentHash::of(bytes))
}

/// The state of a file holding `bytes`, whose hash is `hash`: quarantined
/// where the bytes do not decode as a vault document, by the derivation's
/// own rule ([`decodes`]). **Every file state planning or the applier reads
/// from bytes is built here**, so the two record and compare one flag.
pub(crate) fn holding(bytes: &[u8], hash: ContentHash) -> FileState {
    if decodes(bytes) {
        FileState::present(hash)
    } else {
        FileState::quarantined(hash)
    }
}

#[cfg(test)]
mod tests {
    use norn_wire::{AuthoredValue, WriteTarget};

    use super::super::view::memory::MemoryVault;
    use super::*;

    pub(crate) fn path(text: &str) -> DocumentPath {
        DocumentPath::new(text).expect("a legal document path")
    }

    fn after_text(composition: &Composition, at: &str) -> Option<String> {
        let target = composition.targets.get(&path(at)).expect("a target");
        target
            .after
            .as_ref()
            .map(|bytes| String::from_utf8(bytes.to_vec()).expect("utf-8"))
    }

    fn in_order(operations: &[Operation]) -> Vec<usize> {
        (0..operations.len()).collect()
    }

    #[test]
    fn a_create_composes_its_content_over_an_absent_file() {
        let vault = MemoryVault::default();
        let operations = [Operation::new(OperationKind::create_document(
            path("notes/new.md"),
            "hello",
        ))];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(
            composition.targets[&path("notes/new.md")].before,
            FileState::absent()
        );
        assert_eq!(
            after_text(&composition, "notes/new.md").as_deref(),
            Some("hello")
        );
    }

    fn rewrite(at: &str, from: &str, to: &str) -> norn_wire::LinkRewrite {
        norn_wire::LinkRewrite::new(path(at), norn_wire::LinkFamily::Wikilink, from, to)
    }

    /// **A cascade composes after every operation of the plan**, each
    /// rewrite on its holder's final bytes: an edit of a holder another
    /// operation also writes composes first, and a link the moved document
    /// holds is respelled at its destination.
    #[test]
    fn a_cascade_respells_its_holders_after_every_operation() {
        let vault = MemoryVault::with(&[("a.md", "self [[a]]\n"), ("h.md", "[[a]] and old\n")]);
        let operations = [
            Operation::new(OperationKind::move_document(path("a.md"), path("x/b.md")))
                .with_cascade(vec![rewrite("h.md", "a", "b"), rewrite("x/b.md", "a", "b")]),
            Operation::new(OperationKind::str_replace(path("h.md"), "old", "new")),
        ];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert!(composition.skipped.is_empty());
        assert_eq!(
            after_text(&composition, "h.md").as_deref(),
            Some("[[b]] and new\n")
        );
        assert_eq!(
            after_text(&composition, "x/b.md").as_deref(),
            Some("self [[b]]\n")
        );
        assert_eq!(after_text(&composition, "a.md"), None);
    }

    /// **A holder's rewrites compose as one batch, whatever operation carries
    /// each**: `[[a]]` respelled `b` is not respelled again by the rewrite
    /// meant for the `[[b]]` the holder already held, in either order of the
    /// two moves, and a moved document's own relative links — one's new
    /// spelling another's old one — each reach the file they named.
    #[test]
    fn a_holders_rewrites_compose_as_one_batch_whatever_the_order() {
        let vault = MemoryVault::with(&[
            ("a.md", "A\n"),
            ("p/b.md", "B\n"),
            ("h.md", "[[a]] [[b]]\n"),
            ("a/b/m.md", "[p](../N2.md) [q](../../N2.md)\n"),
        ]);
        let first = Operation::new(OperationKind::move_document(path("a.md"), path("r/b.md")))
            .with_cascade(vec![rewrite("h.md", "a", "b")]);
        let second = Operation::new(OperationKind::move_document(path("p/b.md"), path("q/z.md")))
            .with_cascade(vec![rewrite("h.md", "b", "z")]);
        let own = Operation::new(OperationKind::move_document(
            path("a/b/m.md"),
            path("z/m.md"),
        ))
        .with_cascade(vec![
            norn_wire::LinkRewrite::new(
                path("z/m.md"),
                norn_wire::LinkFamily::Markdown,
                "../../N2.md",
                "../N2.md",
            ),
            norn_wire::LinkRewrite::new(
                path("z/m.md"),
                norn_wire::LinkFamily::Markdown,
                "../N2.md",
                "../a/N2.md",
            ),
        ]);
        for operations in [
            [first.clone(), second.clone(), own.clone()],
            [second.clone(), first.clone(), own.clone()],
        ] {
            let composition =
                compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
            assert!(composition.unresolvable.is_empty());
            assert_eq!(
                after_text(&composition, "h.md").as_deref(),
                Some("[[b]] [[z]]\n")
            );
            assert_eq!(
                after_text(&composition, "z/m.md").as_deref(),
                Some("[p](../a/N2.md) [q](../N2.md)\n")
            );
        }
    }

    /// **A rewrite matching nothing composes its holder unchanged** and is no
    /// failure: the holder is a target whose after-state is its
    /// before-state.
    #[test]
    fn a_rewrite_matching_nothing_composes_its_holder_unchanged() {
        let vault = MemoryVault::with(&[("a.md", "A\n"), ("h.md", "[[other]]\n")]);
        let operations = [
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md")))
                .with_cascade(vec![rewrite("h.md", "a", "b")]),
        ];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        let holder = &composition.targets[&path("h.md")];
        assert_eq!(
            holder.before,
            FileState::present(content_hash(b"[[other]]\n"))
        );
        assert_eq!(
            after_text(&composition, "h.md").as_deref(),
            Some("[[other]]\n")
        );
    }

    /// **A link the rewriter leaves as written is reported with its reason**,
    /// keyed by the holder at the spelling the plan writes it, the syntax and
    /// the address it is still written with; its holder composes unchanged.
    #[test]
    fn a_skipped_link_reports_its_reason() {
        let vault = MemoryVault::with(&[("a.md", "A\n"), ("h.md", "[[a]]\n")]);
        let operations = [
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md")))
                .with_cascade(vec![rewrite("h.md", "a", "b]]c")]),
        ];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(after_text(&composition, "h.md").as_deref(), Some("[[a]]\n"));
        assert_eq!(
            composition.skipped,
            vec![Skipped {
                holder: path("h.md"),
                syntax: norn_wire::LinkFamily::Wikilink,
                address: "a".to_string(),
                reason: norn_text::RewriteSkip::Unrepresentable,
            }]
        );
    }

    /// **A cascade whose holder holds no document does not act**, and its
    /// operation is unresolvable whole: planning names only holders that
    /// stand, so such a cascade was not planned.
    #[test]
    fn a_cascade_naming_no_document_leaves_its_operation_unresolvable() {
        let vault = MemoryVault::with(&[("a.md", "A\n")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md")))
                .with_cascade(vec![rewrite("gone.md", "a", "b")]),
        );
        assert!(detail.contains("gone.md"), "{detail}");
    }

    #[test]
    fn a_str_replace_replaces_the_one_occurrence_of_its_text() {
        let vault = MemoryVault::with(&[("a.md", "status: draft\nbody\n")]);
        let operations = [Operation::new(OperationKind::str_replace(
            path("a.md"),
            "draft",
            "final",
        ))];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(
            composition.targets[&path("a.md")].before,
            FileState::present(content_hash(b"status: draft\nbody\n"))
        );
        assert_eq!(
            after_text(&composition, "a.md").as_deref(),
            Some("status: final\nbody\n")
        );
    }

    fn unresolvable_detail(vault: &MemoryVault, operation: Operation) -> String {
        let operations = [operation];
        let composition =
            compose(&operations, &in_order(&operations), vault).expect("an infallible view");
        let [
            Unresolvable {
                position: 0,
                detail,
            },
        ] = &composition.unresolvable[..]
        else {
            panic!("the one operation is unresolvable");
        };
        detail.clone()
    }

    #[test]
    fn a_str_replace_whose_text_is_gone_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "status: final\n")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final")),
        );
        assert!(detail.contains("no longer occurs"), "{detail}");
    }

    #[test]
    fn a_str_replace_whose_text_occurs_twice_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "draft and draft\n")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "draft", "final")),
        );
        assert!(detail.contains("more than once"), "{detail}");
    }

    #[test]
    fn overlapping_occurrences_are_more_than_one() {
        let vault = MemoryVault::with(&[("a.md", "aaa")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "aa", "b")),
        );
        assert!(detail.contains("more than once"), "{detail}");
    }

    #[test]
    fn an_empty_text_names_no_place_even_in_an_empty_document() {
        let vault = MemoryVault::with(&[("a.md", "")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "", "text")),
        );
        assert!(detail.contains("empty"), "{detail}");
    }

    #[test]
    fn a_str_replace_on_an_absent_document_does_not_resolve() {
        let vault = MemoryVault::default();
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::str_replace(path("a.md"), "x", "y")),
        );
        assert!(detail.contains("no document"), "{detail}");
    }

    #[test]
    fn a_create_over_a_standing_document_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "here")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::create_document(path("a.md"), "new")),
        );
        assert!(detail.contains("a document stands"), "{detail}");
    }

    #[test]
    fn a_delete_leaves_its_document_absent() {
        let vault = MemoryVault::with(&[("a.md", "gone")]);
        let operations = [Operation::new(OperationKind::delete_document(path("a.md")))];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(
            composition.targets[&path("a.md")].before,
            FileState::present(content_hash(b"gone"))
        );
        assert_eq!(after_text(&composition, "a.md"), None);
    }

    #[test]
    fn a_delete_of_an_absent_document_does_not_resolve() {
        let detail = unresolvable_detail(
            &MemoryVault::default(),
            Operation::new(OperationKind::delete_document(path("a.md"))),
        );
        assert!(detail.contains("no document"), "{detail}");
    }

    #[test]
    fn a_move_creates_its_destination_from_its_source_and_removes_the_source() {
        let vault = MemoryVault::with(&[("a.md", "moved")]);
        let operations = [Operation::new(OperationKind::move_document(
            path("a.md"),
            path("archive/a.md"),
        ))];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(after_text(&composition, "a.md"), None);
        assert_eq!(
            composition.targets[&path("archive/a.md")].before,
            FileState::absent()
        );
        assert_eq!(
            after_text(&composition, "archive/a.md").as_deref(),
            Some("moved")
        );
    }

    #[test]
    fn a_move_onto_a_standing_document_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "a"), ("b.md", "b")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
        );
        assert!(detail.contains("a document stands"), "{detail}");
    }

    #[test]
    fn a_move_onto_itself_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "a")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::move_document(path("a.md"), path("a.md"))),
        );
        assert!(detail.contains("onto itself"), "{detail}");
    }

    #[test]
    fn a_move_from_an_absent_document_does_not_resolve() {
        let detail = unresolvable_detail(
            &MemoryVault::default(),
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
        );
        assert!(detail.contains("no document"), "{detail}");
    }

    #[test]
    fn operations_on_one_file_compose_in_order() {
        let vault = MemoryVault::with(&[("a.md", "one two")]);
        let operations = [
            Operation::new(OperationKind::str_replace(path("a.md"), "one", "three")),
            Operation::new(OperationKind::move_document(path("a.md"), path("b.md"))),
            Operation::new(OperationKind::str_replace(
                path("b.md"),
                "three two",
                "four",
            )),
        ];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(after_text(&composition, "a.md"), None);
        assert_eq!(after_text(&composition, "b.md").as_deref(), Some("four"));
    }

    #[test]
    fn an_operation_that_does_not_resolve_leaves_the_state_it_met() {
        let vault = MemoryVault::with(&[("a.md", "one")]);
        let operations = [
            Operation::new(OperationKind::str_replace(path("a.md"), "absent", "x")),
            Operation::new(OperationKind::str_replace(path("a.md"), "one", "two")),
        ];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert_eq!(composition.unresolvable.len(), 1);
        assert_eq!(after_text(&composition, "a.md").as_deref(), Some("two"));
    }

    #[test]
    fn a_path_in_a_second_spelling_is_the_file_its_one_spelling_names() {
        let vault = MemoryVault::with(&[("a/b.md", "b")]).folding_case();
        for spelling in ["A/b.md", "a/B.md", "A/B.MD"] {
            let operations = [Operation::new(OperationKind::delete_document(path(
                spelling,
            )))];
            let composition =
                compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
            assert!(composition.unresolvable.is_empty(), "{spelling}");
            assert_eq!(
                composition.targets.keys().collect::<Vec<_>>(),
                vec![&path("a/b.md")],
                "{spelling}"
            );
        }
    }

    fn composed_text(vault: &MemoryVault, operation: Operation, at: &str) -> Option<String> {
        let operations = [operation];
        let composition =
            compose(&operations, &in_order(&operations), vault).expect("an infallible view");
        assert!(
            composition.unresolvable.is_empty(),
            "{:?}",
            composition.unresolvable
        );
        after_text(&composition, at)
    }

    fn at(text: &str) -> WriteTarget {
        WriteTarget::path(path(text))
    }

    /// **A `set_frontmatter` writes exactly the value sent into the one
    /// field**, leaving every other byte of the document as it was.
    #[test]
    fn a_set_frontmatter_writes_its_value_into_the_field() {
        let vault = MemoryVault::with(&[("a.md", "---\nstatus: draft\ntitle: A\n---\nbody\n")]);
        let composed = composed_text(
            &vault,
            Operation::new(OperationKind::set_frontmatter(
                at("a.md"),
                "status",
                AuthoredValue::string("done"),
            )),
            "a.md",
        );
        assert_eq!(
            composed.as_deref(),
            Some("---\nstatus: done\ntitle: A\n---\nbody\n")
        );
    }

    /// **A `remove_frontmatter` takes the field's whole entry away.**
    #[test]
    fn a_remove_frontmatter_takes_the_field_away() {
        let vault = MemoryVault::with(&[("a.md", "---\nstatus: draft\ntitle: A\n---\nbody\n")]);
        let composed = composed_text(
            &vault,
            Operation::new(OperationKind::remove_frontmatter(at("a.md"), "status")),
            "a.md",
        );
        assert_eq!(composed.as_deref(), Some("---\ntitle: A\n---\nbody\n"));
    }

    /// **A `push_frontmatter` appends to the list, a value it already holds
    /// included, and makes an absent field, or one written with no value, a
    /// list of the one value.**
    #[test]
    fn a_push_frontmatter_appends_and_makes_an_absent_field_a_list() {
        let vault = MemoryVault::with(&[
            ("a.md", "---\ntags:\n  - x\n---\n"),
            ("b.md", "---\ntitle: B\n---\n"),
        ]);
        let push = |at_path: &str| {
            Operation::new(OperationKind::push_frontmatter(
                at(at_path),
                "tags",
                AuthoredValue::string("x"),
            ))
        };
        assert_eq!(
            composed_text(&vault, push("a.md"), "a.md").as_deref(),
            Some("---\ntags:\n  - x\n  - x\n---\n")
        );
        let made = composed_text(&vault, push("b.md"), "b.md").expect("a document");
        let document = norn_text::Document::parse(&made);
        let tags = document
            .frontmatter()
            .and_then(norn_text::Value::as_map)
            .and_then(|map| map.get("tags"))
            .cloned();
        assert_eq!(
            tags,
            Some(norn_text::Value::Sequence(vec![norn_text::Value::from(
                "x"
            )]))
        );
        let stub = MemoryVault::with(&[("c.md", "---\ntags:\n---\n")]);
        let made = composed_text(&stub, push("c.md"), "c.md").expect("a document");
        let document = norn_text::Document::parse(&made);
        assert_eq!(
            document
                .frontmatter()
                .and_then(norn_text::Value::as_map)
                .and_then(|map| map.get("tags"))
                .cloned(),
            Some(norn_text::Value::Sequence(vec![norn_text::Value::from(
                "x"
            )])),
            "a field written with no value takes one element"
        );
    }

    /// **A `pop_frontmatter` removes every element equal to its value.**
    #[test]
    fn a_pop_frontmatter_removes_every_equal_element() {
        let vault = MemoryVault::with(&[("a.md", "---\ntags:\n  - x\n  - y\n  - x\n---\n")]);
        let composed = composed_text(
            &vault,
            Operation::new(OperationKind::pop_frontmatter(
                at("a.md"),
                "tags",
                AuthoredValue::string("x"),
            )),
            "a.md",
        );
        assert_eq!(composed.as_deref(), Some("---\ntags:\n  - y\n---\n"));
    }

    /// **A `replace_body` replaces everything after the frontmatter block and
    /// leaves the block byte-identical.**
    #[test]
    fn a_replace_body_keeps_the_block_byte_identical() {
        let vault = MemoryVault::with(&[("a.md", "---\ntitle:   'A'  # kept\n---\nold body\n")]);
        let composed = composed_text(
            &vault,
            Operation::new(OperationKind::replace_body(path("a.md"), "new body\n")),
            "a.md",
        );
        assert_eq!(
            composed.as_deref(),
            Some("---\ntitle:   'A'  # kept\n---\nnew body\n")
        );
    }

    /// **The section kinds address a section by its heading through the one
    /// shared resolver**: a replace keeps the heading line, a delete removes
    /// heading and body, an append lands at the section's end, and an insert
    /// goes directly before or after the heading line, adding no blank line.
    #[test]
    fn the_section_kinds_edit_the_section_their_heading_names() {
        let source = "# Top\n\n## Tasks\n\none\n\n## Notes\n\ntwo\n";
        let vault = MemoryVault::with(&[("a.md", source)]);
        let cases = [
            (
                OperationKind::replace_section(path("a.md"), "tasks", "fresh\n"),
                "# Top\n\n## Tasks\n\nfresh\n\n## Notes\n\ntwo\n",
            ),
            (
                OperationKind::delete_section(path("a.md"), "Tasks"),
                "# Top\n\n## Notes\n\ntwo\n",
            ),
            (
                OperationKind::append_to_section(path("a.md"), "Tasks", "added"),
                "# Top\n\n## Tasks\n\none\nadded\n\n## Notes\n\ntwo\n",
            ),
            (
                OperationKind::insert_before_heading(path("a.md"), "Notes", "above"),
                "# Top\n\n## Tasks\n\none\n\nabove\n## Notes\n\ntwo\n",
            ),
            (
                OperationKind::insert_after_heading(path("a.md"), "Notes", "below"),
                "# Top\n\n## Tasks\n\none\n\n## Notes\nbelow\n\ntwo\n",
            ),
        ];
        for (kind, expected) in cases {
            let name = kind.name();
            assert_eq!(
                composed_text(&vault, Operation::new(kind), "a.md").as_deref(),
                Some(expected),
                "{name}"
            );
        }
    }

    /// **A push onto a field holding a scalar or a map does not resolve**:
    /// turning it into a list is a set.
    #[test]
    fn a_push_onto_a_scalar_or_a_map_does_not_resolve() {
        let vault = MemoryVault::with(&[
            ("a.md", "---\ntags: one\n---\n"),
            ("b.md", "---\ntags:\n  k: v\n---\n"),
        ]);
        for at_path in ["a.md", "b.md"] {
            let detail = unresolvable_detail(
                &vault,
                Operation::new(OperationKind::push_frontmatter(
                    at(at_path),
                    "tags",
                    AuthoredValue::string("x"),
                )),
            );
            assert!(detail.contains("not a list"), "{at_path}: {detail}");
        }
    }

    /// **A pop of a value the list does not hold, or of an absent field, does
    /// not resolve**, rather than landing as a silent no-op.
    #[test]
    fn a_pop_of_a_value_not_held_or_an_absent_field_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "---\ntags:\n  - y\nempty:\n---\n")]);
        let pop = |field: &str| {
            Operation::new(OperationKind::pop_frontmatter(
                at("a.md"),
                field,
                AuthoredValue::string("x"),
            ))
        };
        let not_held = unresolvable_detail(&vault, pop("tags"));
        assert!(not_held.contains("holds no element"), "{not_held}");
        let written_empty = unresolvable_detail(&vault, pop("empty"));
        assert!(
            written_empty.contains("holds no element"),
            "{written_empty}"
        );
        let absent = unresolvable_detail(&vault, pop("missing"));
        assert!(absent.contains("not present"), "{absent}");
    }

    /// **A remove of a field the document does not carry does not resolve.**
    #[test]
    fn a_remove_of_an_absent_field_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "---\ntitle: A\n---\n"), ("b.md", "no block\n")]);
        for at_path in ["a.md", "b.md"] {
            let detail = unresolvable_detail(
                &vault,
                Operation::new(OperationKind::remove_frontmatter(at(at_path), "status")),
            );
            assert!(detail.contains("not present"), "{at_path}: {detail}");
        }
    }

    /// **A heading the folding cannot tell apart from another is ambiguous,
    /// and a heading no line reads as is not found**: neither resolves, and
    /// neither advises addressing an occurrence the wire cannot name.
    #[test]
    fn an_ambiguous_or_missing_heading_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "## Tasks\n\none\n\n### tasks\n\ntwo\n")]);
        let ambiguous = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::delete_section(path("a.md"), "Tasks")),
        );
        assert!(ambiguous.contains("2 headings"), "{ambiguous}");
        assert!(!ambiguous.contains("occurrence"), "{ambiguous}");
        let missing = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::replace_section(path("a.md"), "Notes", "x")),
        );
        assert!(missing.contains("no heading"), "{missing}");
    }

    /// **A heading inside a blockquote or a list item never matches as a
    /// section to edit.**
    #[test]
    fn a_heading_in_a_container_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "> ## Quoted\n> body\n")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::replace_section(path("a.md"), "Quoted", "x")),
        );
        assert!(detail.contains("blockquote or list item"), "{detail}");
    }

    /// **An append or insert with empty content does not resolve**, so it
    /// never lands as a change of nothing; a replace with empty content
    /// empties the section and resolves.
    #[test]
    fn an_append_or_insert_with_empty_content_does_not_resolve() {
        let vault = MemoryVault::with(&[("a.md", "## Tasks\n\none\n")]);
        for kind in [
            OperationKind::append_to_section(path("a.md"), "Tasks", ""),
            OperationKind::insert_before_heading(path("a.md"), "Tasks", ""),
            OperationKind::insert_after_heading(path("a.md"), "Tasks", ""),
        ] {
            let name = kind.name();
            let detail = unresolvable_detail(&vault, Operation::new(kind));
            assert!(detail.contains("empty content"), "{name}: {detail}");
        }
        assert_eq!(
            composed_text(
                &vault,
                Operation::new(OperationKind::replace_section(path("a.md"), "Tasks", "")),
                "a.md"
            )
            .as_deref(),
            Some("## Tasks\n\n")
        );
    }

    /// **A nested value composes like any other**: a map and a list of lists
    /// set in block style, a map pushed onto a list as one element and popped
    /// from it again.
    #[test]
    fn a_nested_value_composes_like_any_other() {
        let source = "---\ntags: []\nrows:\n  - k: 1\n---\n";
        let vault = MemoryVault::with(&[("a.md", source)]);
        let map = |value| {
            AuthoredValue::map([("k".to_string(), AuthoredValue::Integer(value))]).expect("a map")
        };
        let grid = AuthoredValue::list([AuthoredValue::list([AuthoredValue::string("a")])]);
        for (kind, written) in [
            (
                OperationKind::set_frontmatter(at("a.md"), "owner", map(2)),
                "---\ntags: []\nrows:\n  - k: 1\nowner:\n  k: 2\n---\n",
            ),
            (
                OperationKind::set_frontmatter(at("a.md"), "grid", grid),
                "---\ntags: []\nrows:\n  - k: 1\ngrid:\n  - - a\n---\n",
            ),
            (
                OperationKind::push_frontmatter(at("a.md"), "rows", map(2)),
                "---\ntags: []\nrows:\n  - k: 1\n  - k: 2\n---\n",
            ),
            (
                OperationKind::pop_frontmatter(at("a.md"), "rows", map(1)),
                "---\ntags: []\nrows: []\n---\n",
            ),
        ] {
            let name = kind.name();
            assert_eq!(
                composed_text(&vault, Operation::new(kind), "a.md").as_deref(),
                Some(written),
                "{name}"
            );
        }
    }

    /// **A refusal `norn-text` makes leaves the operation unresolved with its
    /// message**: a list rewrite that would drop a comment, and a
    /// frontmatter block that cannot be read.
    #[test]
    fn an_edit_norn_text_refuses_does_not_resolve_with_its_message() {
        let vault = MemoryVault::with(&[
            ("a.md", "---\ntags: [a]  # keep\n---\n"),
            ("b.md", "---\ntitle: [unclosed\n---\n"),
        ]);
        let commented = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::push_frontmatter(
                at("a.md"),
                "tags",
                AuthoredValue::string("b"),
            )),
        );
        assert!(commented.contains("comment"), "{commented}");
        let unreadable = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::set_frontmatter(
                at("b.md"),
                "status",
                AuthoredValue::Bool(true),
            )),
        );
        assert!(unreadable.contains("cannot be read"), "{unreadable}");
    }

    /// **A document that is not UTF-8 text takes no document-local edit.**
    #[test]
    fn a_document_that_is_not_text_does_not_resolve() {
        let vault = MemoryVault::with_bytes(&[("a.md", b"\xff\xfe body\n")]);
        let detail = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::replace_body(path("a.md"), "x")),
        );
        assert!(detail.contains("not UTF-8"), "{detail}");
    }

    /// **A `where` target reaching composition does not resolve, and an empty
    /// one says why it names nothing**: a resolved plan carries only path
    /// targets, and a conjunction of no predicates matches every document.
    #[test]
    fn a_where_target_does_not_resolve_and_an_empty_one_is_refused() {
        let vault = MemoryVault::with(&[("a.md", "---\nstatus: draft\n---\n")]);
        let predicate: norn_wire::Predicate =
            serde_json::from_str(r#"{"op":"eq","key":"status","value":"draft"}"#)
                .expect("a predicate");
        let matching = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::set_frontmatter(
                WriteTarget::matching([predicate]),
                "status",
                AuthoredValue::string("done"),
            )),
        );
        assert!(matching.contains("not yet expanded"), "{matching}");
        let empty = unresolvable_detail(
            &vault,
            Operation::new(OperationKind::remove_frontmatter(
                WriteTarget::matching(Vec::new()),
                "status",
            )),
        );
        assert!(empty.contains("at least one predicate"), "{empty}");
    }

    /// **A document-local edit of a document that is not there does not
    /// resolve.**
    #[test]
    fn a_document_local_edit_of_an_absent_document_does_not_resolve() {
        let detail = unresolvable_detail(
            &MemoryVault::default(),
            Operation::new(OperationKind::replace_body(path("a.md"), "x")),
        );
        assert!(detail.contains("no document"), "{detail}");
    }

    /// **Document-local edits compose in order over what the document holds
    /// so far**, a create in the same plan included.
    #[test]
    fn document_local_edits_compose_over_what_the_plan_left() {
        let operations = [
            Operation::new(OperationKind::create_document(path("a.md"), "## Log\n")),
            Operation::new(OperationKind::push_frontmatter(
                at("a.md"),
                "tags",
                AuthoredValue::string("new"),
            )),
            Operation::new(OperationKind::append_to_section(
                path("a.md"),
                "Log",
                "entry",
            )),
        ];
        let composition = compose(&operations, &in_order(&operations), &MemoryVault::default())
            .expect("an infallible view");
        assert!(
            composition.unresolvable.is_empty(),
            "{:?}",
            composition.unresolvable
        );
        let text = after_text(&composition, "a.md").expect("a document");
        assert!(text.contains("new"), "{text}");
        assert!(text.ends_with("## Log\nentry\n"), "{text}");
    }

    #[test]
    fn two_spellings_of_one_file_compose_as_one_target() {
        let vault = MemoryVault::with(&[("a/b.md", "one two")]).folding_case();
        let operations = [
            Operation::new(OperationKind::str_replace(path("a/b.md"), "one", "1")),
            Operation::new(OperationKind::str_replace(path("A/b.md"), "two", "2")),
        ];
        let composition =
            compose(&operations, &in_order(&operations), &vault).expect("an infallible view");
        assert!(composition.unresolvable.is_empty());
        assert_eq!(composition.targets.len(), 1);
        assert_eq!(after_text(&composition, "a/b.md").as_deref(), Some("1 2"));
    }
}
