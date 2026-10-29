//! Construction boundaries that admit read shapes.
//!
//! NORN-468 PR C (host early admission) is the consuming layer. The host still
//! uses disposable loading sources today, so this table is not yet on its path.
//! Future wire variants fall back to Ready; they never gain early admission.

use norn_wire::{
    Anchor, CollectionSelector, Column, FindParams, GetParams, Predicate, SortKey, Verb,
};

/// The end of the construction step that admits a shape, or canonical Ready.
/// Full text is admitted only after the closing transaction, never mid-batch.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Boundary {
    /// Fields and stored content are complete.
    Content,
    /// Document structure is complete.
    Structure,
    /// Relation judgments and findings are complete.
    Relations,
    /// The full-text closing transaction has committed.
    FullTextComplete,
    /// This shape has no early capability.
    Ready,
}

/// Earliest complete predicate facts. Future predicates stay canonical-only.
pub fn predicate(value: &Predicate) -> Boundary {
    match value {
        Predicate::Eq { .. }
        | Predicate::NotEq { .. }
        | Predicate::In { .. }
        | Predicate::Has { .. }
        | Predicate::Missing { .. }
        | Predicate::Before { .. }
        | Predicate::After { .. }
        | Predicate::Path { .. } => Boundary::Content,
        Predicate::Tag { .. } | Predicate::LinksTo { .. } | Predicate::Resolves { .. } => {
            Boundary::Structure
        }
        Predicate::HasFinding { .. } => Boundary::Relations,
        Predicate::Matches { .. } => Boundary::FullTextComplete,
        _ => Boundary::Ready,
    }
}

/// Earliest complete projection, withholding relation parts until Relations.
pub fn column(value: &Column) -> Boundary {
    match value {
        Column::Path { .. } | Column::Field { .. } | Column::Fields { .. } => Boundary::Content,
        Column::Body { .. }
        | Column::Links { .. }
        | Column::Headings { .. }
        | Column::Blocks { .. }
        | Column::Tags { .. }
        | Column::Findings { .. } => Boundary::Structure,
        _ => Boundary::Ready,
    }
}

/// Earliest complete ordering facts.
pub fn sort_key(value: &SortKey) -> Boundary {
    match value {
        SortKey::Path { .. } | SortKey::Field { .. } => Boundary::Content,
        _ => Boundary::Ready,
    }
}

/// A Find needs every predicate, projection and ordering fact it names.
pub fn find(params: &FindParams) -> Boundary {
    params
        .predicates
        .iter()
        .map(predicate)
        .chain(params.columns.iter().map(column))
        .chain(params.sort.iter().map(|sort| sort_key(&sort.key)))
        .fold(Boundary::Content, std::cmp::max)
}

/// Vault-wide Get admission. PR C may admit a target earlier once all its
/// candidates have content and the resolved document has committed structure.
/// An undeclared Field projection still requires vault-wide Structure.
pub fn get(params: &GetParams) -> Boundary {
    if params
        .columns
        .iter()
        .any(|value| column(value) == Boundary::Ready)
    {
        return Boundary::Ready;
    }
    match params.collection {
        Some(
            CollectionSelector::Links
            | CollectionSelector::Headings
            | CollectionSelector::Blocks
            | CollectionSelector::Tags
            | CollectionSelector::Findings,
        ) => Boundary::Structure,
        None => match params.target.anchor() {
            None | Some(Anchor::Heading { .. } | Anchor::Block { .. }) => Boundary::Structure,
            _ => Boundary::Ready,
        },
        _ => Boundary::Ready,
    }
}

/// Earliest verb capability; Get and Find also require shape classification.
/// Plans, writes and all other verbs have no early capability.
pub fn verb(value: Verb) -> Boundary {
    match value {
        Verb::Find => Boundary::Content,
        Verb::Get => Boundary::Structure,
        Verb::Validate => Boundary::Relations,
        Verb::Search => Boundary::FullTextComplete,
        _ => Boundary::Ready,
    }
}

#[cfg(test)]
#[test]
fn every_shape_has_one_step() {
    use norn_wire::{ResolutionTarget, VaultAddress, VaultName};
    for (json, expected) in [
        (r#"{"op":"eq","key":"k","value":"v"}"#, Boundary::Content),
        (
            r#"{"op":"not_eq","key":"k","value":"v"}"#,
            Boundary::Content,
        ),
        (r#"{"op":"in","key":"k","values":["v"]}"#, Boundary::Content),
        (r#"{"op":"has","key":"k"}"#, Boundary::Content),
        (r#"{"op":"missing","key":"k"}"#, Boundary::Content),
        (
            r#"{"op":"before","key":"k","value":"v"}"#,
            Boundary::Content,
        ),
        (r#"{"op":"after","key":"k","value":"v"}"#, Boundary::Content),
        (r#"{"op":"path","glob":"**"}"#, Boundary::Content),
        (r#"{"op":"tag","name":"x"}"#, Boundary::Structure),
        (r#"{"op":"links_to","target":"note"}"#, Boundary::Structure),
        (r#"{"op":"resolves","target":"note"}"#, Boundary::Structure),
        (
            r#"{"op":"has_finding","kind":"link/broken"}"#,
            Boundary::Relations,
        ),
        (
            r#"{"op":"matches","query":"word"}"#,
            Boundary::FullTextComplete,
        ),
    ] {
        assert_eq!(
            predicate(&serde_json::from_str(json).unwrap()),
            expected,
            "{json}"
        );
    }
    for (value, expected) in [
        (Column::path(), Boundary::Content),
        (Column::field("k"), Boundary::Content),
        (Column::fields(), Boundary::Content),
        (Column::body(), Boundary::Structure),
        (Column::links(), Boundary::Structure),
        (Column::headings(), Boundary::Structure),
        (Column::blocks(), Boundary::Structure),
        (Column::tags(), Boundary::Structure),
        (Column::findings(), Boundary::Structure),
    ] {
        assert_eq!(column(&value), expected);
    }
    for value in [SortKey::path(), SortKey::field("k")] {
        assert_eq!(sort_key(&value), Boundary::Content);
    }
    let verbs = [
        (Verb::Find, Boundary::Content),
        (Verb::Get, Boundary::Structure),
        (Verb::Validate, Boundary::Relations),
        (Verb::Search, Boundary::FullTextComplete),
        (Verb::Count, Boundary::Ready),
        (Verb::Describe, Boundary::Ready),
        (Verb::Apply, Boundary::Ready),
        (Verb::Set, Boundary::Ready),
        (Verb::Edit, Boundary::Ready),
        (Verb::New, Boundary::Ready),
        (Verb::Move, Boundary::Ready),
        (Verb::Delete, Boundary::Ready),
        (Verb::RewriteWikilink, Boundary::Ready),
        (Verb::Repair, Boundary::Ready),
        (Verb::Init, Boundary::Ready),
        (Verb::VaultRegister, Boundary::Ready),
        (Verb::VaultUnregister, Boundary::Ready),
        (Verb::VaultList, Boundary::Ready),
        (Verb::VaultSet, Boundary::Ready),
        (Verb::VaultResolve, Boundary::Ready),
        (Verb::VaultStatus, Boundary::Ready),
        (Verb::VaultReload, Boundary::Ready),
        (Verb::VaultMigrate, Boundary::Ready),
        (Verb::DoctorRegistry, Boundary::Ready),
    ];
    assert_eq!(verbs.len(), Verb::ALL.len());
    for (value, expected) in verbs {
        assert_eq!(verb(value), expected, "{value:?}");
    }
    let vault = VaultAddress::name(VaultName::new("test").unwrap());
    for target in ["note", "note#heading", "note#^block"] {
        let params = GetParams::new(vault.clone(), ResolutionTarget::new(target).unwrap());
        assert_eq!(get(&params), Boundary::Structure);
        for selector in [
            CollectionSelector::Links,
            CollectionSelector::Headings,
            CollectionSelector::Blocks,
            CollectionSelector::Tags,
            CollectionSelector::Findings,
        ] {
            let page = params.clone().with_collection(selector);
            assert_eq!(get(&page), Boundary::Structure);
            let continued = page.with_after(norn_wire::Cursor::new(
                norn_wire::Snapshot::new("epoch", 1, None, None),
                norn_wire::CursorKey::ordinal(selector, 1),
            ));
            assert_eq!(get(&continued), Boundary::Structure);
        }
    }
    let params = FindParams::new(vault).with_predicates([Predicate::matches("word")]);
    assert_eq!(find(&params), Boundary::FullTextComplete);
}
