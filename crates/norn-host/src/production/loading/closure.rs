//! Bounded whole-Get closure, shared by ordinary loading and retained experiments.
use super::super::*;
use super::facts::{Depth, load_fact};
use norn_store::loading::Catalogue;

const CLOSURE_BOUND: usize = 256;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::production) enum Preparation {
    Ready,
    Declined,
    SourceChanged,
}

#[allow(clippy::too_many_arguments)]
pub(in crate::production) fn prepare(
    store: &mut Store,
    catalogue: &Catalogue,
    source: &norn_fs::Vault,
    root: &Path,
    exclusions: &[PathBuf],
    policy: ProductionPolicy,
    target: &str,
    mut loaded: impl FnMut(&str) -> Result<(), JobFailure>,
) -> Result<Preparation, JobFailure> {
    // This capability covers whole-record default gets only. Other request
    // shapes must wait; the ordinary external API is unchanged.
    if target.contains('#') {
        return Ok(Preparation::Declined);
    }
    let Some(demanded) = catalogue
        .demand_names(target, CLOSURE_BOUND)
        .map_err(store_effect)?
    else {
        return Ok(Preparation::Declined);
    };
    // Missing targets wait for ordinary readiness; the catalogue cannot certify absence.
    if demanded.is_empty() {
        return Ok(Preparation::Declined);
    }
    let demanded: BTreeSet<_> = demanded.into_iter().collect();
    let mut remaining = demanded.clone();
    let mut observed = BTreeMap::new();
    let mut account = Account::default();
    while let Some(name) = remaining.pop_first() {
        if observed.contains_key(&name) {
            continue;
        }
        if observed.len() >= CLOSURE_BOUND {
            return Ok(Preparation::Declined);
        }
        let fact = source.fact(Path::new(&name)).map_err(effect)?;
        if matches!(fact, norn_fs::WalkFact::Unreadable(_)) {
            return Ok(Preparation::Declined);
        }
        if !matches!(fact, norn_fs::WalkFact::File(_)) {
            return Ok(Preparation::SourceChanged);
        }
        let hash = {
            let mut pending =
                Pending::new(store, policy.changeset_size, root, exclusions, &mut account)?;
            let hash = load_fact(&mut pending, fact, Depth::Rich, true)?;
            pending.flush()?;
            hash
        };
        if is_markdown(Path::new(&name)) && hash.is_none() {
            let broken = document_path(Path::new(&name))
                .ok()
                .map(|path| store.begin_request().stored_document(&path))
                .transpose()
                .map_err(store_effect)?
                .flatten()
                .is_some_and(|row| row.content_hash.is_none());
            return Ok(if broken {
                Preparation::Declined
            } else {
                Preparation::SourceChanged
            });
        }
        loaded(&name)?;
        observed.insert(name.clone(), hash);
        let path = document_path(Path::new(&name))
            .map_err(|_| JobFailure::Environmental("catalogue path became invalid".into()))?;
        // Complete the naming context for ambiguity heads and finding candidates.
        // This does not follow neighbors' outgoing links.
        for spelling in path.suffix_spellings() {
            let Some(names) = catalogue
                .demand_names(&spelling, CLOSURE_BOUND)
                .map_err(store_effect)?
            else {
                return Ok(Preparation::Declined);
            };
            remaining.extend(
                names
                    .into_iter()
                    .filter(|name| !observed.contains_key(name)),
            );
            if remaining.len() + observed.len() > CLOSURE_BOUND {
                return Ok(Preparation::Declined);
            }
        }
        if demanded.contains(&name)
            && let Some(facts) = store
                .begin_request()
                .stored_facts(&path)
                .map_err(store_effect)?
        {
            for link in &facts.links {
                let Some(names) = catalogue
                    .demand_link_names(&link.fact, &path, CLOSURE_BOUND)
                    .map_err(store_effect)?
                else {
                    return Ok(Preparation::Declined);
                };
                remaining.extend(
                    names
                        .into_iter()
                        .filter(|name| !observed.contains_key(name)),
                );
                if remaining.len() + observed.len() > CLOSURE_BOUND {
                    return Ok(Preparation::Declined);
                }
            }
        }
    }
    // Re-observe real bytes after closure construction, including invalid UTF-8
    // candidates. A changed or removed member declines this construction.
    for (name, hash) in observed {
        let file = match source.fact(Path::new(&name)).map_err(effect)? {
            norn_fs::WalkFact::File(file) => file,
            norn_fs::WalkFact::Unreadable(_) => return Ok(Preparation::Declined),
            norn_fs::WalkFact::Skipped(_) => return Ok(Preparation::SourceChanged),
        };
        if let Some(hash) = hash {
            let read = match file.read_optional().map_err(effect)? {
                norn_fs::Opened::Read(read) => read,
                norn_fs::Opened::Nothing => return Ok(Preparation::SourceChanged),
                norn_fs::Opened::Unreadable { .. } | norn_fs::Opened::UnreadableFolder { .. } => {
                    return Ok(Preparation::Declined);
                }
            };
            if read.content_hash().to_string() != hash {
                return Ok(Preparation::SourceChanged);
            }
        }
    }
    Ok(Preparation::Ready)
}
