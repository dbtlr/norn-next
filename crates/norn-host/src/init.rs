//! The `init` verb: a starter schema for a registered vault that declares
//! none, planned and applied through the one planner and the one applier.
//!
//! **What init plans, and when.** A registration naming a `schema_source` —
//! inside the vault or out — reads its schema from a file init does not
//! write, so init answers that the schema lives elsewhere, naming the source,
//! and plans nothing. Otherwise the vault reads its schema from the default
//! `.norn/schema.yaml`, and init plans one `write_control_file` of the starter
//! there. A plan finding a schema already standing there would replace it,
//! which init never does: it answers that the vault is already set up and
//! writes nothing. Rewriting a schema that stands is `vault migrate`'s.
//!
//! **An apply writes what its preview planned, or nothing.** An init sent to
//! apply plans the starter as its preview does, then sends the resolved plan,
//! not its operations, to the applier: the plan's before-state is the absence
//! planning read, so a schema another writer creates in between refuses the
//! apply by create exclusivity — answered with a fresh plan, as any drift is —
//! rather than being replaced.
//!
//! **The vault is reloaded under what landed.** The watcher's control-file
//! facts are discarded by design, and attach and an explicit reload are the
//! two activation boundaries (`docs/architecture.md`). Init is the caller's
//! explicit act on the schema, so once its apply lands it takes the boundary
//! `vault reload` takes, in the same call, and answers once the vault serves
//! under the starter. The starter declares nothing, so no finding moves.
//!
//! **The starter is a pure function of the vault's observed field universe**
//! ([`starter_schema`]), read through the Layer 3 builders in process on one
//! snapshot ([`observed_fields`]): the describe builder's observed-field
//! facets, and for each key the count builder's tally of the documents
//! carrying it. It declares nothing beyond `version: 1`; every observed field
//! is a comment the user's agent turns into a declaration.

use std::fmt::Write as _;

use norn_store::{ContentModel, PageRefusal, Snapshot};
use norn_wire::{
    ApplyMode, ApplyReport, AuthoredPlan, ContainerKind, ControlFile, CountParams, DescribeParams,
    DocumentPath, ErrorEnvelope, Facet, FacetKind, FileState, Forecast, InitParams, InitReport,
    Operation, OperationKind, PlanDocument, Predicate, ResolvedPlan, VaultAddress, VaultName,
};

use crate::address::registered_name;
use crate::lifecycle::{EntryOps, Host, ReadSource, SnapshotSource};
use crate::read::Built;

/// One frontmatter key the vault's documents carry: how many carry it, and
/// every container its values sit in, in the vocabulary's order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObservedKey {
    pub(crate) key: String,
    pub(crate) documents: u64,
    pub(crate) containers: Vec<ContainerKind>,
}

/// The starter schema's opening: what it is, and the next step.
const HEADER: &str = "\
# The schema norn reads this vault's documents under.
#
# It declares nothing yet. Have your agent turn the commented hints below
# into declarations: each names a frontmatter field the vault's documents
# carry, how many documents carry it, and the shapes its values take. The
# `describe` verb reports what the vault declares and what its documents
# carry.
version: 1
";

/// The starter schema for a vault whose documents carry `observed`, in key
/// order.
///
/// **It declares nothing.** Beside `version: 1` every line is a comment, so it
/// reads as the declaration that declares nothing, and a vault reloaded
/// under it judges every document as it did. A vault with no documents gets
/// the header and the version alone.
///
/// **Deterministic**: a pure function of `observed`, so two previews of one
/// vault write the same bytes. Each key is written quoted, every character a
/// comment line cannot hold — a line break of any kind, any other control
/// character, a byte-order mark — escaped as `\uXXXX`, with `"` and `\`
/// escaped too, so no key ends its comment or reads as a declaration.
pub(crate) fn starter_schema(observed: &[ObservedKey]) -> String {
    let mut schema = String::from(HEADER);
    if observed.is_empty() {
        return schema;
    }
    schema.push_str("\n# Observed fields: the key, how many documents carry it, and its shapes.\n");
    for field in observed {
        let shapes: Vec<&str> = field
            .containers
            .iter()
            .map(|container| container.as_str())
            .collect();
        let documents = match field.documents {
            1 => "1 document".to_string(),
            count => format!("{count} documents"),
        };
        let _ = writeln!(
            schema,
            "# {}: {documents}; {}",
            quoted(&field.key),
            shapes.join(", ")
        );
    }
    schema
}

/// `key` as a double-quoted string a comment line holds whole.
fn quoted(key: &str) -> String {
    let mut quoted = String::with_capacity(key.len() + 2);
    quoted.push('"');
    for character in key.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            character
                if character.is_control()
                    || matches!(character, '\u{2028}' | '\u{2029}' | '\u{feff}') =>
            {
                let _ = write!(quoted, "\\u{:04x}", u32::from(character));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// Every frontmatter key `vault`'s documents carry on `snapshot`, in key
/// order, each with the documents carrying it and its containers.
///
/// **Read through the Layer 3 builders, in process**: every page of the
/// describe builder's observed-field facets, and for each key one count of
/// the documents a `has` predicate on it matches — the answers a `describe`
/// and a `count` at that instant give, never a query of init's own. It holds
/// one entry per key, which the starter writes whole.
pub(crate) fn observed_fields(
    vault: &VaultAddress,
    snapshot: &Snapshot,
    declared: &ContentModel,
) -> Result<Vec<ObservedKey>, PageRefusal> {
    let mut observed = Vec::new();
    let mut request = DescribeParams::new(vault.clone()).with_facets([FacetKind::ObservedField]);
    loop {
        let described = snapshot.describe(&request, declared)?;
        for facet in described.facets {
            let Facet::ObservedField {
                key, containers, ..
            } = facet
            else {
                continue;
            };
            let counted = snapshot.count(
                &CountParams::new(vault.clone()).with_predicates([Predicate::has(key.clone())]),
                declared,
            )?;
            let documents = counted.tallies.iter().map(|tally| tally.count).sum();
            observed.push(ObservedKey {
                key,
                documents,
                containers,
            });
        }
        match described.next {
            Some(after) => request = request.with_after(after),
            None => return Ok(observed),
        }
    }
}

/// What planning the starter came to: its preview — the resolved plan and its
/// forecast — or a schema already standing where it would be written.
enum Starter {
    Planned(Box<ResolvedPlan>, Forecast),
    AlreadySetUp(DocumentPath),
}

impl<O> Host<O>
where
    O: EntryOps,
    <O::Attachment as SnapshotSource>::Reader: ReadSource<Snapshot = Snapshot>,
{
    /// Answer an `init`: preview or apply the starter schema of the
    /// registered vault `params` names, or say why nothing is planned — the
    /// vault reads its schema from a registered source, or a schema already
    /// stands at the default path.
    ///
    /// An apply lands the plan its preview would answer, sent as a resolved
    /// plan so a schema created since refuses it, and then reloads the vault
    /// under the schema it wrote, as `vault reload` does; a reload that
    /// refuses answers with its refusal, the schema on disk and reported as
    /// a reload pending, and a host gone before the reload ran answers what
    /// landed, since the next attach reads it.
    pub fn init(&self, params: &InitParams) -> Result<InitReport, ErrorEnvelope> {
        let name = registered_name(&params.vault)?.clone();
        if let Some(source) = self
            .registrations()
            .into_iter()
            .find(|registration| registration.name == name)
            .and_then(|registration| registration.schema_source)
        {
            return Ok(InitReport::schema_elsewhere(source));
        }
        let (plan, forecast) = match self.plan_starter(&name, &params.vault)? {
            Starter::AlreadySetUp(schema) => return Ok(InitReport::already_set_up(schema)),
            Starter::Planned(plan, forecast) => (*plan, forecast),
        };
        match params.mode {
            ApplyMode::Preview => Ok(InitReport::scaffolded(ApplyReport::previewed(
                plan, forecast,
            ))),
            ApplyMode::Apply => {
                let applied = self
                    .admit_apply(&name, PlanDocument::resolved(plan))?
                    .wait()?;
                // A host gone before its reload ran answers what landed: the
                // next attach reads the schema the apply wrote.
                if let Err(refusal) = self.reload(&name)
                    && let Ok(refused) = refusal.answer(&name)
                {
                    return Err(refused);
                }
                Ok(InitReport::scaffolded(applied.report))
            }
        }
    }

    /// Plan the starter schema of `name`, addressed as `vault`: its observed
    /// fields read on one hold, then one `write_control_file` previewed
    /// through the one `apply` seam, as an apply would plan and judge it.
    fn plan_starter(
        &self,
        name: &VaultName,
        vault: &VaultAddress,
    ) -> Result<Starter, ErrorEnvelope> {
        let observed = self
            .answer_read(vault, |_, snapshot, declared| {
                Ok(Built {
                    unsatisfied: Vec::new(),
                    advisories: Vec::new(),
                    report: observed_fields(vault, snapshot, declared)?,
                    work: (),
                })
            })?
            .answer
            .report;
        let authored = AuthoredPlan::new(
            vault.clone(),
            vec![Operation::new(OperationKind::write_control_file(
                ControlFile::Schema,
                starter_schema(&observed),
            ))],
        );
        let ApplyReport::Previewed { plan, forecast, .. } = self
            .preview(name, PlanDocument::operations(authored))?
            .report
        else {
            unreachable!("a preview answers a previewed plan or a refusal");
        };
        // A schema standing where the starter would be written makes the
        // write a replacement, which init never plans.
        if let [transition] = plan.transitions.as_slice()
            && transition.before != FileState::absent()
        {
            return Ok(Starter::AlreadySetUp(transition.path.clone()));
        }
        Ok(Starter::Planned(Box::new(plan), forecast))
    }
}

#[cfg(test)]
mod tests {
    use norn_config::schema::VaultSchema;

    use super::*;

    fn key(key: &str, documents: u64, containers: &[ContainerKind]) -> ObservedKey {
        ObservedKey {
            key: key.to_string(),
            documents,
            containers: containers.to_vec(),
        }
    }

    /// **A vault with no documents gets the header and the version alone**,
    /// which read as the declaration that declares nothing.
    #[test]
    fn a_starter_for_a_vault_with_no_documents_is_its_header_and_version_alone() {
        let starter = starter_schema(&[]);
        assert_eq!(starter, HEADER);
        assert!(starter.ends_with("version: 1\n"));
        assert_eq!(
            VaultSchema::parse(starter.as_bytes()).expect("the starter parses"),
            VaultSchema::default()
        );
    }

    /// **Each observed field is one comment line, in key order, naming how
    /// many documents carry it and every shape its values take**, and the
    /// starter still declares nothing.
    #[test]
    fn a_starter_lists_each_observed_field_with_its_count_and_shapes() {
        let starter = starter_schema(&[
            key("status", 3, &[ContainerKind::Scalar]),
            key("tags", 1, &[ContainerKind::Scalar, ContainerKind::Sequence]),
        ]);
        assert_eq!(
            starter,
            format!(
                "{HEADER}\n# Observed fields: the key, how many documents carry it, and its shapes.\n\
                 # \"status\": 3 documents; scalar\n\
                 # \"tags\": 1 document; scalar, sequence\n"
            )
        );
        assert_eq!(
            VaultSchema::parse(starter.as_bytes()).expect("the starter parses"),
            VaultSchema::default()
        );
    }

    /// **A key a comment line cannot hold whole is escaped**: a line break of
    /// every kind YAML reads as one, a tab, a quote and a backslash stay
    /// inside the key's one comment line, so the starter still has exactly
    /// one line per key and declares nothing.
    #[test]
    fn a_key_a_comment_cannot_hold_whole_is_escaped_and_declares_nothing() {
        let keys = [
            "a\nversion: 2",
            "b\r\nfields:",
            "c\u{85}d",
            "e\u{2028}f\u{2029}g",
            "h\ti",
            "\u{feff}j",
            "k\"l\\m",
        ];
        let observed: Vec<ObservedKey> = keys
            .iter()
            .map(|text| key(text, 1, &[ContainerKind::Map]))
            .collect();
        let starter = starter_schema(&observed);
        assert_eq!(
            VaultSchema::parse(starter.as_bytes()).expect("the starter parses"),
            VaultSchema::default()
        );
        let listed: Vec<&str> = starter
            .lines()
            .skip_while(|line| !line.starts_with("# Observed fields"))
            .skip(1)
            .collect();
        assert_eq!(
            listed,
            [
                r#"# "a\u000aversion: 2": 1 document; map"#,
                r#"# "b\u000d\u000afields:": 1 document; map"#,
                r#"# "c\u0085d": 1 document; map"#,
                r#"# "e\u2028f\u2029g": 1 document; map"#,
                r#"# "h\u0009i": 1 document; map"#,
                r#"# "\ufeffj": 1 document; map"#,
                r#"# "k\"l\\m": 1 document; map"#,
            ]
        );
        assert!(
            starter
                .lines()
                .all(|line| line.is_empty() || line.starts_with('#') || line == "version: 1")
        );
    }
}
