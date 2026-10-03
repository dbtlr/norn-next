//! The `init` verb: a starter schema for a registered vault that declares
//! none, planned and applied through the one planner and the one applier.
//!
//! **What init plans, and when.** A registration naming a `schema_source` —
//! inside the vault or out — reads its schema from a file init does not
//! write, so init answers that the schema lives elsewhere, naming the source,
//! and plans nothing. Otherwise the vault reads its schema from the default
//! `.norn/schema.yaml`, and init plans one `write_control_file` of the starter
//! there. A schema already standing there would be replaced, which init never
//! does: it answers that the vault is already set up and writes nothing.
//! Rewriting a schema that stands is `vault migrate`'s. Both answers are
//! reached before the vault's field universe is read or the starter built, so
//! neither rests on the starter building and a re-run pays no describe or
//! count.
//!
//! **An apply plans the starter afresh and sends its own resolved plan.** An
//! init sent to apply takes no preview from its caller: within the one call it
//! plans the starter as a preview does, then sends that resolved plan, not its
//! operations, to the applier. The plan's before-state is the absence planning
//! read, so a schema another writer creates in between refuses the apply by
//! create exclusivity, and init answers the vault already set up — never the
//! refusal's fresh plan, which would replace that schema. A writer that left
//! exactly the starter's bytes leaves the create landed, reported found, as
//! any target already at its after-state is.
//!
//! **The vault is reloaded under what landed.** The watcher's control-file
//! facts are discarded by design, and attach and an explicit reload are the
//! two activation boundaries (`docs/architecture.md`). Init is the caller's
//! explicit act on the schema, so once its apply lands it takes the boundary
//! `vault reload` takes, in the same call, and answers once the vault serves
//! under the starter. The starter declares nothing, so the findings after it
//! are those the vault holds under the empty declaration. Where a served
//! schema was deleted while attached, init's reload is what activates that
//! deletion, and the deleted schema's findings go.
//!
//! **The starter is a pure function of the vault's observed field universe**
//! ([`starter_schema`]), read through the Layer 3 builders in process on one
//! snapshot ([`observed_fields`]): the describe builder's observed-field
//! facets, and for each key the count builder's tally of the documents
//! carrying it. It declares nothing beyond `version: 1`; every observed field
//! is a comment the user's agent turns into a declaration. Two previews of an
//! unchanged vault write the same bytes because the starter is a function of
//! the keys alone, not because they read one snapshot: each reads its own.
//!
//! **What init costs.** A vault already set up costs one read of its schema.
//! Otherwise a preview, and the planning an apply runs, holds one snapshot
//! across every page of the describe builder's observed-field facets and one
//! count per observed key — a pass that grows with the keys the documents
//! carry, the snapshot held throughout — and then plans and judges the one
//! write. An apply that lands then pays the reload's re-pin: the starter's
//! fingerprint is not the empty schema's, so the schema-keyed rows are
//! discarded and the whole vault is walked again, though the starter declares
//! nothing.

use std::fmt::Write as _;
use std::path::Path;

use norn_store::{ContentModel, PageRefusal, Snapshot};
use norn_wire::{
    ApplyMode, ApplyReport, AuthoredPlan, ContainerKind, ControlFile, CountParams, DescribeParams,
    DocumentPath, ErrorDetail, ErrorEnvelope, Facet, FacetKind, FileState, Forecast, InitParams,
    InitReport, Operation, OperationKind, PlanDocument, Predicate, ResolvedPlan, VaultAddress,
    VaultName,
};

use crate::address::registered_name;
use crate::apply::{PlanGround, unreadable};
use crate::lifecycle::{EntryOps, Host, ReadSource, SnapshotSource};
use crate::planner::control::control_path;
use crate::planner::view::{Entry, TreeView, VaultView};
use crate::read::{BuildRefused, Built};

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
/// comment line cannot hold ([`comment_holds`]) — a line break of any kind, a
/// tab, a byte-order mark, and every character outside YAML's printable set —
/// escaped as `\uXXXX`, with `"` and `\` escaped too, so no key ends its
/// comment, reads as a declaration or makes the starter unreadable.
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
            character if !comment_holds(character) => {
                let _ = write!(quoted, "\\u{:04x}", u32::from(character));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// Whether a comment line holds `character` as it is: one of `x20`–`x7E`,
/// `xA0`–`xD7FF`, `xE000`–`xFFFD` or `x10000`–`x10FFFF`, other than U+2028,
/// U+2029 and the byte-order mark.
///
/// That is YAML's printable set — `x9`, `xA`, `xD`, `x20`–`x7E`, `x85`,
/// `xA0`–`xD7FF`, `xE000`–`xFFFD`, `x10000`–`x10FFFF` — less its line breaks
/// (line feed, carriage return, next line, and U+2028 and U+2029, which a
/// YAML 1.1 reader breaks a line at), the tab and the byte-order mark. A
/// character outside the set — a control character, or the noncharacters
/// U+FFFE and U+FFFF — is refused by a YAML reader even inside a comment.
fn comment_holds(character: char) -> bool {
    matches!(
        character,
        '\u{20}'..='\u{7e}' | '\u{a0}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..
    ) && !matches!(character, '\u{2028}' | '\u{2029}' | '\u{feff}')
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

/// The schema another writer made after init previewed its starter, where
/// `refused` is the starter's apply refused for it: the fresh plan the
/// refusal carries would replace a file standing at the starter's path.
///
/// **Init answers that as the vault already set up, never with the fresh
/// plan**, which a caller sending it on would replace the other writer's
/// schema with.
fn schema_written_since(refused: &ErrorEnvelope) -> Option<DocumentPath> {
    let ErrorDetail::PlanRefused { plan, .. } = refused.detail() else {
        return None;
    };
    let schema = control_path(ControlFile::Schema);
    plan.transitions
        .iter()
        .find(|transition| transition.path == *schema && transition.before != FileState::absent())
        .map(|transition| transition.path.clone())
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
    /// An apply plans the starter afresh, as a preview does, and sends that
    /// resolved plan, so a schema created since refuses it and is answered
    /// already set up rather than replaced; then it reloads the vault
    /// under the schema it wrote, as `vault reload` does. A reload that
    /// refuses is answered beside the landing, the schema on disk and
    /// reported as a reload pending, and a host gone before the reload ran
    /// answers the landing alone, since the next attach reads it.
    ///
    /// **Answered synchronously.** The other write verbs answer a
    /// [`PendingApply`](crate::PendingApply) a caller can stop waiting on;
    /// init waits inside the call for its apply and its reload, and answers
    /// once both have. A vault reading its schema from the default path whose
    /// standing schema cannot be read is untrusted, so init answers the
    /// entry's untrusted refusal, as a read of it does, not `already_set_up`.
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
                let applied = match self
                    .admit_apply(&name, PlanDocument::resolved(plan))?
                    .wait()
                {
                    Ok(applied) => applied,
                    Err(refused) => {
                        return match schema_written_since(&refused) {
                            Some(schema) => Ok(InitReport::already_set_up(schema)),
                            None => Err(refused),
                        };
                    }
                };
                // The schema landed whatever the reload answers, so a refused
                // reload is answered beside the landing; a host gone before
                // its reload ran answers the landing alone, since the next
                // attach reads the schema the apply wrote.
                if let Err(refusal) = self.reload(&name)
                    && let Ok(refused) = refusal.answer(&name)
                {
                    return Ok(InitReport::scaffolded_reload_refused(
                        applied.report,
                        refused,
                    ));
                }
                Ok(InitReport::scaffolded(applied.report))
            }
        }
    }

    /// Plan the starter schema of `name`, addressed as `vault`: on one hold,
    /// whether a schema already stands where the starter would be written,
    /// and only where none does its observed fields; then one
    /// `write_control_file` previewed through the one `apply` seam, as an
    /// apply would plan and judge it.
    ///
    /// **Whether a schema stands is asked first**, so a vault already set up
    /// is answered without a describe or a count, whatever keys its documents
    /// carry, and that answer never rests on the starter building.
    fn plan_starter(
        &self,
        name: &VaultName,
        vault: &VaultAddress,
    ) -> Result<Starter, ErrorEnvelope> {
        let observed = self
            .answer_read_on_hold(vault, |name, hold| {
                let report = match schema_standing(name, hold.plan_ground())
                    .map_err(BuildRefused::Answered)?
                {
                    Some(schema) => Err(schema),
                    None => Ok(observed_fields(
                        vault,
                        hold.snapshot(),
                        hold.content_model(),
                    )?),
                };
                Ok(Built {
                    unsatisfied: Vec::new(),
                    advisories: Vec::new(),
                    report,
                    work: (),
                })
            })?
            .answer
            .report;
        let observed = match observed {
            Ok(observed) => observed,
            Err(schema) => return Ok(Starter::AlreadySetUp(schema)),
        };
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
        // A schema written there since it was asked makes the write a
        // replacement, which init never plans.
        if let [transition] = plan.transitions.as_slice()
            && transition.before != FileState::absent()
        {
            return Ok(Starter::AlreadySetUp(transition.path.clone()));
        }
        Ok(Starter::Planned(Box::new(plan), forecast))
    }
}

/// Where a schema stands at the default path the vault `name` reads it at, or
/// `None` where nothing that reads as one does: read as the planner reads a
/// control target ([`VaultView::control_entry`]), on `ground`, the ground the
/// entry's coverage stands on as the read's establishing hold read it.
///
/// Something there that is no file — a folder, a link — is no schema
/// standing, and is left to the starter's own planning, which refuses to
/// write there naming it.
fn schema_standing(
    name: &VaultName,
    ground: Option<&PlanGround>,
) -> Result<Option<DocumentPath>, ErrorEnvelope> {
    // An entry recording no ground is left to the preview, which answers
    // that as the host defect it is.
    let Some(ground) = ground else {
        return Ok(None);
    };
    ground.standing(name)?;
    let view = TreeView::open(&ground.root, &ground.exclusions)
        .map_err(|error| unreadable(name, error))?;
    let schema = control_path(ControlFile::Schema);
    let Ok(identity) = view.normalizer().normalize(Path::new(schema.as_str())) else {
        return Ok(None);
    };
    Ok(
        match view
            .control_entry(&identity)
            .map_err(|error| unreadable(name, error))?
        {
            Entry::Document { at, .. } => Some(at),
            Entry::Absent { .. } | Entry::Folder | Entry::Blocked { .. } => None,
        },
    )
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

    /// **Every character outside YAML's printable set is escaped**, whichever
    /// excluded class it falls in — the C0 controls but tab, line feed and
    /// carriage return; delete and the C1 controls but next line; and the
    /// noncharacters U+FFFE and U+FFFF, which a YAML reader refuses even
    /// inside a comment — so a key holding any of them still leaves a starter
    /// that parses and declares nothing, the character written as `\uXXXX`;
    /// and the printable set's bounds are written as they are.
    #[test]
    fn every_character_outside_yaml_s_printable_set_is_escaped() {
        let excluded = [
            (0x00..=0x08),
            (0x0b..=0x0c),
            (0x0e..=0x1f),
            (0x7f..=0x84),
            (0x86..=0x9f),
            (0xfffe..=0xffff),
        ];
        for class in excluded {
            for code in class {
                let character = char::from_u32(code).expect("a scalar value");
                let starter = starter_schema(&[key(&format!("a{character}b"), 1, &[])]);
                assert_eq!(
                    VaultSchema::parse(starter.as_bytes()).map_err(|error| error.to_string()),
                    Ok(VaultSchema::default()),
                    "U+{code:04X}"
                );
                assert!(
                    starter.ends_with(&format!("# \"a\\u{code:04x}b\": 1 document; \n")),
                    "U+{code:04X}:\n{starter}"
                );
            }
        }
        for printable in [
            '\u{a0}',
            '\u{d7ff}',
            '\u{e000}',
            '\u{fffd}',
            '\u{10000}',
            '\u{10ffff}',
        ] {
            let starter = starter_schema(&[key(&format!("a{printable}b"), 1, &[])]);
            assert!(
                starter.ends_with(&format!("# \"a{printable}b\": 1 document; \n")),
                "{printable:?}:\n{starter}"
            );
            VaultSchema::parse(starter.as_bytes()).expect("the starter parses");
        }
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
