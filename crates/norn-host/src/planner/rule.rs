//! Creation by rule: each `create_by_rule` turned, at planning, into the one
//! `create_document` its rule makes, with a concrete path and composed
//! content.
//!
//! **Expanded before anything is ordered, as a folder move is.** A
//! `create_by_rule` names no path until its rule fills one, so planning
//! expands it one for one, at its place in the plan, into an ordinary
//! `create_document` that keeps the operation's identifier, requirements,
//! conditions and footnote. What follows — ordering, composition, vacancy,
//! the applier's schema check and its exclusive create — plans and judges
//! that create as it would any. A resolved plan carries only the concrete
//! create, so every template value is fixed at planning, and a resolved plan
//! sent back writes exactly the path and the bytes it was previewed with,
//! however the clock or the vault has moved since.
//!
//! **The rules are the schema the plan is judged under**: the entry's pinned
//! schema, the one its plan ground carries and the applier's schema check
//! judges a composed result by ([`Rules`]). A rule name the schema does not
//! declare, and a creation naming no rule where the schema declares no inbox,
//! do not resolve.
//!
//! **One clock reading per plan.** The clock is read the first time an
//! operation of the plan creates by rule, and never for a plan that does not;
//! every template of the plan fills from that one reading. A clock outside
//! the years `{{date}}` can write leaves every creation by rule of the plan
//! unresolved, saying so.
//!
//! **What a caller supplies is judged against the rule.** Every variable the
//! rule declares must be supplied, and none it does not declare may be: a
//! variable the rule would not read is left unresolved rather than dropped.
//! The inbox declares none. A value that would break the target's path — one
//! a [`FillError`] names — leaves the operation unresolved naming the token
//! and the value, or the path it filled to.
//!
//! **The document's text** ([`composed`]): the rule's frontmatter defaults,
//! each string scalar filled as a template and every type and order kept,
//! then the caller's fields laid over them — a field the defaults hold keeps
//! its place and takes the caller's value, and a new one follows in the
//! caller's order. A caller's value is typed and written as it is, never
//! filled as a template, converted one to one as a `set`'s value is. The body
//! is the caller's where it sends one, else the rule's body template filled,
//! else empty. The inbox has neither defaults nor a body template, so a
//! capture is exactly the caller's fields and body. The document is written
//! through `norn-text`'s one renderer, with LF line endings; one holding no
//! field is its body alone, with no empty frontmatter block. A document the
//! renderer refuses — a block past the bound the reader admits among them —
//! leaves the operation unresolved naming the refusal.
//!
//! **`{{seq}}` is the highest number already used, plus one** ([`allocated`]),
//! counted per slot: the folder and the file name around the number a
//! target fills to, every other token filled. The numbers already used are
//! read from the names the folder lists — the same files the planner reads
//! a create's vacancy from, never the index — and from every name the plan
//! itself puts a document at: a create's path, a move's destination, and the
//! number an earlier creation of the same plan took, so two creations on one
//! slot take consecutive numbers in plan order. A name the plan removes is
//! still listed, so a number is never used twice, and a gap is never filled.
//! A name counts where it is the slot's text with one or more ASCII digits
//! between, compared under the root's case rule through the one
//! normalization point, so `task-007.md` counts as 7 and, on a root that
//! folds case, `TASK-8.md` counts for `task-`. A number past what the
//! allocator can count leaves the operation unresolved naming the file
//! rather than numbering below it. A folder that does not stand yet numbers
//! from 1. Allocation is a reading, not a reservation: a name another writer
//! takes before the plan lands is a taken name the applier refuses, as any
//! create's is, and the caller plans again.

use std::collections::BTreeMap;
use std::path::Path;

use norn_config::schema::{
    CreationRule, FillError, LocalTimestamp, NotALocalTimestamp, SeqSlot, Target, TemplateValues,
    VaultSchema,
};
use norn_text::{LineEnding, Mapping, render_document};
use norn_wire::{DocumentPath, Operation, OperationKind, UnresolvedReason, ValueMap, Variables};

use super::edit::text_value;
use super::view::VaultView;

/// What a plan's creations by rule are made by: the creation rules and the
/// inbox of the schema the plan is judged under, and the clock the plan
/// reads once.
pub(crate) struct Rules<'a> {
    /// The schema the entry's store pins.
    pub(crate) schema: &'a VaultSchema,
    /// The host's clock, read at most once for a plan.
    pub(crate) clock: &'a dyn Fn() -> Result<LocalTimestamp, NotALocalTimestamp>,
}

/// Expand every `create_by_rule` of `operations` in place into the
/// `create_document` its rule makes, or leave it out of the plan in
/// `left_out`, naming why.
pub(crate) fn expand<V: VaultView>(
    operations: &mut [Operation],
    left_out: &mut BTreeMap<usize, UnresolvedReason>,
    rules: &Rules<'_>,
    view: &V,
) -> Result<(), V::Error> {
    // Every name the plan puts a document at, each a number a slot may
    // already hold; each creation by rule adds the one it takes.
    let mut arrivals: Vec<String> = operations
        .iter()
        .filter_map(|operation| match &operation.kind {
            OperationKind::CreateDocument { path, .. } => Some(path.as_str().to_string()),
            OperationKind::MoveDocument { to, .. } => Some(to.as_str().to_string()),
            _ => None,
        })
        .collect();
    let mut reading = None;
    for (position, operation) in operations.iter_mut().enumerate() {
        let OperationKind::CreateByRule {
            rule,
            variables,
            fields,
            body,
        } = &operation.kind
        else {
            continue;
        };
        let at = *reading.get_or_insert_with(|| (rules.clock)());
        let asked = Asked {
            rule: rule.as_deref(),
            variables,
            fields,
            body: body.as_deref(),
        };
        match made(&asked, rules.schema, at, &arrivals, view)? {
            Ok((path, content)) => {
                arrivals.push(path.as_str().to_string());
                operation.kind = OperationKind::create_document(path, content);
            }
            Err(detail) => {
                left_out.insert(position, UnresolvedReason::no_longer_resolves(detail));
            }
        }
    }
    Ok(())
}

/// What one `create_by_rule` asks for.
struct Asked<'a> {
    rule: Option<&'a str>,
    variables: &'a Variables,
    fields: &'a ValueMap,
    body: Option<&'a str>,
}

/// The path and text of the document `asked` makes under `schema`, read at
/// `at`, numbered past `arrivals` and what `view` lists; or why it makes
/// none, in words.
fn made<V: VaultView>(
    asked: &Asked<'_>,
    schema: &VaultSchema,
    at: Result<LocalTimestamp, NotALocalTimestamp>,
    arrivals: &[String],
    view: &V,
) -> Result<Result<(DocumentPath, String), String>, V::Error> {
    let (target, rule, named) = match asked.rule {
        Some(name) => match schema.creation_rule(name) {
            Some(rule) => (
                rule.target(),
                Some(rule),
                format!("the creation rule `{name}`"),
            ),
            None => {
                return Ok(Err(format!(
                    "the vault's schema declares no creation rule `{name}`"
                )));
            }
        },
        None => match schema.inbox() {
            Some(inbox) => (inbox.target(), None, "the inbox".to_string()),
            None => {
                return Ok(Err(
                    "no inbox is declared in the vault's schema, so a document created by no \
                     rule has nowhere to land"
                        .to_string(),
                ));
            }
        },
    };
    let declared = rule.map_or(&[][..], CreationRule::variables);
    let supplied = asked.variables.entries();
    if let Some((name, _)) = supplied.iter().find(|(name, _)| !declared.contains(name)) {
        return Ok(Err(format!("{named} declares no variable `{name}`")));
    }
    if let Some(name) = declared
        .iter()
        .find(|name| !supplied.iter().any(|(supplied, _)| supplied == *name))
    {
        return Ok(Err(format!(
            "{named} declares the variable `{name}`, and no value is supplied for it"
        )));
    }
    let Ok(at) = at else {
        return Ok(Err(format!(
            "the host's clock cannot be read as a local time {named} can fill: {NotALocalTimestamp}"
        )));
    };
    let values = TemplateValues::new(supplied.iter().cloned().collect(), at);
    let refused = |error: FillError| format!("{named} cannot make a document: {error}");
    let values = match target.seq_slot(&values) {
        Err(error) => return Ok(Err(refused(error))),
        Ok(None) => values,
        Ok(Some(slot)) => match allocated(&slot, arrivals, view)? {
            Ok(seq) => values.with_seq(seq),
            Err(detail) => return Ok(Err(format!("{named} cannot number a document: {detail}"))),
        },
    };
    let path = match filled_path(target, &values) {
        Ok(path) => path,
        Err(detail) => return Ok(Err(refused_path(&named, &detail))),
    };
    Ok(composed(rule, asked, &values)
        .map(|content| (path, content))
        .map_err(|detail| format!("{named} cannot make a document: {detail}")))
}

/// The document path `target` fills to under `values`, or why it fills to
/// none, in words.
fn filled_path(target: &Target, values: &TemplateValues) -> Result<DocumentPath, String> {
    let filled = target.fill(values).map_err(|error| error.to_string())?;
    DocumentPath::new(filled.as_str()).map_err(|refusal| format!("{filled:?} {refusal}"))
}

fn refused_path(named: &str, detail: &str) -> String {
    format!("{named} cannot make a document: {detail}")
}

/// The text of the document `rule` — the inbox where it is `None` — makes for
/// `asked`, filled under `values`, or why it makes none, in words.
fn composed(
    rule: Option<&CreationRule>,
    asked: &Asked<'_>,
    values: &TemplateValues,
) -> Result<String, String> {
    let mut frontmatter = Mapping::new();
    if let Some(rule) = rule {
        let defaults = rule
            .fill_frontmatter_defaults(values)
            .map_err(|error| error.to_string())?;
        for (key, value) in defaults.entries() {
            frontmatter.insert(key.clone(), text_value(value));
        }
    }
    // A caller's field the defaults hold takes its value where the default
    // stands; one they do not hold follows them.
    for (key, value) in asked.fields.entries() {
        frontmatter.insert(key.clone(), text_value(value));
    }
    let body = match (asked.body, rule.and_then(CreationRule::body)) {
        (Some(body), _) => body.to_string(),
        (None, Some(template)) => template.fill(values).map_err(|error| error.to_string())?,
        (None, None) => String::new(),
    };
    if frontmatter.is_empty() {
        return Ok(body);
    }
    render_document(&frontmatter, &body, LineEnding::Lf)
        .map_err(|error| format!("its document cannot be written: {error}"))
}

/// The number the next document in `slot` takes: one past the highest any
/// name in the slot's folder, or any of `arrivals`, already holds there; or
/// why there is none, in words.
fn allocated<V: VaultView>(
    slot: &SeqSlot,
    arrivals: &[String],
    view: &V,
) -> Result<Result<u64, String>, V::Error> {
    let normalizer = view.normalizer();
    // A slot's folder is a filled target's, which schema read and the fill
    // hold to a path inside the vault, so it always normalizes.
    let listed = if slot.folder().is_empty() {
        view.root_names()?
    } else {
        match normalizer.normalize(Path::new(slot.folder())) {
            Ok(folder) => view.folder_names(&folder)?,
            Err(_) => Vec::new(),
        }
    };
    let standing = listed
        .iter()
        .filter_map(|name| name.to_str())
        .map(|name| at_folder(slot.folder(), name));
    let mut highest = 0;
    for path in standing.chain(arrivals.iter().cloned()) {
        match numbered(slot, &path, view) {
            None => {}
            Some(Some(seq)) => highest = highest.max(seq),
            Some(None) => {
                return Ok(Err(format!(
                    "`{path}` is numbered past the largest number a document can take"
                )));
            }
        }
    }
    Ok(highest.checked_add(1).ok_or_else(|| {
        format!(
            "a document is already numbered {highest} in `{}`, the largest number a document can take",
            slot.path(highest)
        )
    }))
}

/// `name` in `folder`, as a vault-relative spelling.
fn at_folder(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// The number `path` holds in `slot`: `None` where it is not the slot's text
/// with ASCII digits between, `Some(None)` where its digits are past what a
/// `u64` counts.
///
/// **Compared under the root's case rule, through the one normalization
/// point**: the digits are cut out where the slot's text puts them, and the
/// path counts where it names the same file as the slot spelled with those
/// digits. The rule folds ASCII case only, which keeps every byte where it
/// stands, so the cut is the one the root reads. Were a match ever missed,
/// the number it held would be allocated again and meet that file at
/// planning as an occupied name, never overwriting it.
fn numbered<V: VaultView>(slot: &SeqSlot, path: &str, view: &V) -> Option<Option<u64>> {
    let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
    let end = name.len().checked_sub(slot.suffix().len())?;
    let digits = name.get(slot.prefix().len()..end)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let normalizer = view.normalizer();
    let spelled = at_folder(
        slot.folder(),
        &format!("{}{digits}{}", slot.prefix(), slot.suffix()),
    );
    let same = normalizer.normalize(Path::new(path)).ok()?
        == normalizer.normalize(Path::new(&spelled)).ok()?;
    same.then(|| digits.parse().ok())
}

#[cfg(test)]
pub(crate) mod testing {
    use std::sync::LazyLock;

    use norn_config::schema::{LocalTimestamp, NotALocalTimestamp, VaultSchema};

    use super::Rules;

    /// A schema declaring nothing.
    static NO_SCHEMA: LazyLock<VaultSchema> = LazyLock::new(VaultSchema::default);

    /// A clock no case planning without a creation by rule reads.
    fn unread() -> Result<LocalTimestamp, NotALocalTimestamp> {
        panic!("a plan creating nothing by rule read the clock")
    }

    /// The rules of a vault declaring none, for a case that creates nothing
    /// by rule: its clock fails the case if read.
    pub(crate) fn no_rules() -> Rules<'static> {
        Rules {
            schema: &NO_SCHEMA,
            clock: &unread,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use norn_wire::{
        AuthoredPlan, AuthoredValue, DocumentPath, OperationKind, Predicate, RootIdentity,
        ValueMap, Variables, VaultAddress, VaultName,
    };

    use super::super::expand::{Matched, Matcher, resolve_expanding};
    use super::super::links::testing::EmptyStore;
    use super::super::resolve::Resolution;
    use super::super::view::memory::MemoryVault;
    use super::*;

    /// A schema declaring a numbered rule, a rule numbering nothing, and the
    /// inbox.
    const SCHEMA: &[u8] = b"version: 1
creatable:
  task:
    target: \"tasks/{{var.project}}-{{seq}}.md\"
    variables: [project, title]
    frontmatter_defaults:
      status: todo
      created: \"{{now}}\"
      rank: 3
    body: \"# {{var.title}}\\n\"
  daily:
    target: \"daily/{{date}}.md\"
inbox:
  target: \"inbox/{{date}}-{{seq}}.md\"
";

    fn schema(bytes: &[u8]) -> VaultSchema {
        VaultSchema::parse(bytes).expect("a schema declaring creation rules")
    }

    /// The one reading every case's clock gives: 1 October 2026, 09:30:15,
    /// two hours east of UTC.
    fn reading() -> LocalTimestamp {
        LocalTimestamp::new(2026, 10, 1, 9, 30, 15, 120).expect("a reading")
    }

    /// A matcher no case here asks: no operation carries a `where` target.
    struct NoWhere;

    impl Matcher for NoWhere {
        type Error = std::convert::Infallible;

        fn matching(&self, _: &[Predicate]) -> Result<Matched, Self::Error> {
            panic!("a plan with no `where` target was matched")
        }
    }

    /// `operations` planned over `vault` under `schema`, the clock giving
    /// `at` and counting how often it is read in `reads`.
    fn planned_reading(
        vault: &MemoryVault,
        schema: &VaultSchema,
        operations: Vec<Operation>,
        at: Result<LocalTimestamp, NotALocalTimestamp>,
        reads: &Cell<usize>,
    ) -> Resolution {
        let clock = || {
            reads.set(reads.get() + 1);
            at
        };
        let rules = Rules {
            schema,
            clock: &clock,
        };
        let links = (EmptyStore::new(), std::marker::PhantomData);
        let name = VaultName::new("notes").expect("a vault name");
        match resolve_expanding(
            AuthoredPlan::new(VaultAddress::name(name), operations),
            RootIdentity::from_device_and_inode(1, 2),
            vault,
            &NoWhere,
            &links,
            &rules,
        ) {
            Ok(resolution) => resolution,
            Err(failure) => panic!("the plan is planned: {failure:?}"),
        }
    }

    fn planned(vault: &MemoryVault, operations: Vec<Operation>) -> Resolution {
        planned_reading(
            vault,
            &schema(SCHEMA),
            operations,
            Ok(reading()),
            &Cell::new(0),
        )
    }

    fn variables(entries: &[(&str, &str)]) -> Variables {
        Variables::new(
            entries
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
        )
        .expect("each variable once")
    }

    fn fields(entries: Vec<(&str, AuthoredValue)>) -> ValueMap {
        ValueMap::new(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value)),
        )
        .expect("each field once")
    }

    fn by_rule(
        rule: Option<&str>,
        supplied: &[(&str, &str)],
        fields: ValueMap,
        body: Option<&str>,
    ) -> Operation {
        Operation::new(OperationKind::create_by_rule(
            rule.map(str::to_string),
            variables(supplied),
            fields,
            body.map(str::to_string),
        ))
    }

    /// A task for the project `NORN` titled `Ship it`, sending nothing else.
    fn task() -> Operation {
        by_rule(
            Some("task"),
            &[("project", "NORN"), ("title", "Ship it")],
            ValueMap::default(),
            None,
        )
    }

    fn create(at: &str, content: &str) -> OperationKind {
        OperationKind::create_document(DocumentPath::new(at).expect("a document path"), content)
    }

    /// The operations a resolution carries, by kind.
    fn kinds(resolution: &Resolution) -> Vec<OperationKind> {
        assert!(
            resolution.unresolved.is_empty(),
            "every operation resolves: {:?}",
            resolution.unresolved
        );
        resolution
            .plan
            .operations
            .iter()
            .map(|operation| operation.kind.clone())
            .collect()
    }

    /// **A creation by rule expands into the one `create_document` its rule
    /// makes**: at the target filled from the caller's variables, numbered,
    /// holding the rule's defaults filled from the one clock reading with the
    /// caller's fields laid over them — an overriding field keeping the
    /// default's place, a new one following — and the rule's body filled.
    #[test]
    fn a_creation_by_rule_expands_into_the_document_its_rule_makes() {
        let operation = by_rule(
            Some("task"),
            &[("project", "NORN"), ("title", "Ship it")],
            fields(vec![
                ("status", AuthoredValue::string("doing")),
                ("owner", AuthoredValue::string("drew")),
            ]),
            None,
        );
        let resolution = planned(&MemoryVault::default(), vec![operation]);

        assert_eq!(
            kinds(&resolution),
            vec![create(
                "tasks/NORN-1.md",
                "---\nstatus: doing\ncreated: 2026-10-01T09:30:15+02:00\nrank: 3\nowner: drew\n---\n# Ship it\n"
            )]
        );
    }

    /// The path the one creation of `resolution` lands at.
    fn landed_at(resolution: &Resolution) -> String {
        match &kinds(resolution)[..] {
            [OperationKind::CreateDocument { path, .. }] => path.as_str().to_string(),
            other => panic!("one create is planned: {other:?}"),
        }
    }

    /// **`{{seq}}` is one past the highest number the slot's folder already
    /// holds**, gaps never filled.
    #[test]
    fn seq_is_one_past_the_highest_number_held() {
        let vault = MemoryVault::with(&[("tasks/NORN-1.md", "1\n"), ("tasks/NORN-3.md", "3\n")]);
        assert_eq!(landed_at(&planned(&vault, vec![task()])), "tasks/NORN-4.md");
    }

    /// **A number written with leading zeros counts as its value.**
    #[test]
    fn a_zero_padded_number_counts_as_its_value() {
        let vault = MemoryVault::with(&[("tasks/NORN-007.md", "7\n")]);
        assert_eq!(landed_at(&planned(&vault, vec![task()])), "tasks/NORN-8.md");
    }

    /// **A folder that does not stand yet numbers from 1.**
    #[test]
    fn a_missing_folder_numbers_from_one() {
        let vault = MemoryVault::with(&[("other/NORN-5.md", "5\n")]);
        assert_eq!(landed_at(&planned(&vault, vec![task()])), "tasks/NORN-1.md");
    }

    /// **Only a name that is the slot's text with digits between counts**:
    /// another prefix, a name with no digits there, another suffix, and a
    /// deeper folder hold no number of the slot.
    #[test]
    fn only_the_slots_own_names_count() {
        let vault = MemoryVault::with(&[
            ("tasks/NORN-2.md", "2\n"),
            ("tasks/OPS-9.md", "9\n"),
            ("tasks/NORN-x.md", "x\n"),
            ("tasks/NORN-.md", "-\n"),
            ("tasks/NORN-9.md.bak", "bak\n"),
            ("tasks/NORN-9x.md", "9x\n"),
            ("tasks/deep/NORN-9.md", "deep\n"),
            ("tasks/XNORN-9.md", "x9\n"),
        ]);
        assert_eq!(landed_at(&planned(&vault, vec![task()])), "tasks/NORN-3.md");
    }

    /// **Two creations on one slot in one plan take consecutive numbers**,
    /// in plan order.
    #[test]
    fn two_creations_on_one_slot_take_consecutive_numbers() {
        let vault = MemoryVault::with(&[("tasks/NORN-1.md", "1\n")]);
        let resolution = planned(&vault, vec![task(), task()]);
        let paths: Vec<String> = kinds(&resolution)
            .iter()
            .map(|kind| match kind {
                OperationKind::CreateDocument { path, .. } => path.as_str().to_string(),
                other => panic!("a create: {other:?}"),
            })
            .collect();
        assert_eq!(paths, ["tasks/NORN-2.md", "tasks/NORN-3.md"]);
    }

    /// **A number the plan itself frees is never taken again**: a document
    /// the plan deletes, or moves away, still holds its number.
    #[test]
    fn a_number_the_plan_frees_is_not_taken_again() {
        let vault = MemoryVault::with(&[("tasks/NORN-5.md", "5\n"), ("tasks/NORN-6.md", "6\n")]);
        let removing = Operation::new(OperationKind::delete_document(
            DocumentPath::new("tasks/NORN-6.md").expect("a path"),
        ));
        let moving = Operation::new(OperationKind::move_document(
            DocumentPath::new("tasks/NORN-5.md").expect("a path"),
            DocumentPath::new("done/NORN-5.md").expect("a path"),
        ));
        let resolution = planned(&vault, vec![removing, moving, task()]);
        assert_eq!(
            kinds(&resolution)[2],
            create(
                "tasks/NORN-7.md",
                "---\nstatus: todo\ncreated: 2026-10-01T09:30:15+02:00\nrank: 3\n---\n# Ship it\n"
            )
        );
    }

    /// **A name the plan puts a document at holds its number too**: a
    /// create or a move into the slot, wherever it stands in the plan.
    #[test]
    fn a_name_the_plan_fills_holds_its_number() {
        let vault = MemoryVault::with(&[("done/x.md", "x\n")]);
        let creating = Operation::new(create("tasks/NORN-4.md", "4\n"));
        let moving = Operation::new(OperationKind::move_document(
            DocumentPath::new("done/x.md").expect("a path"),
            DocumentPath::new("tasks/NORN-6.md").expect("a path"),
        ));
        let resolution = planned(&vault, vec![task(), creating, moving]);
        let OperationKind::CreateDocument { path, .. } = &kinds(&resolution)[0] else {
            panic!("the creation by rule expands into a create");
        };
        assert_eq!(path.as_str(), "tasks/NORN-7.md");
    }

    /// **On a root that folds case a name counts in any case**, as the root
    /// reads it; on one that tells case apart it does not.
    #[test]
    fn a_name_counts_under_the_roots_case_rule() {
        let files = [("tasks/norn-8.md", "8\n")];
        let folding = MemoryVault::with(&files).folding_case();
        assert_eq!(
            landed_at(&planned(&folding, vec![task()])),
            "tasks/NORN-9.md"
        );
        let telling = MemoryVault::with(&files);
        assert_eq!(
            landed_at(&planned(&telling, vec![task()])),
            "tasks/NORN-1.md"
        );
    }

    /// The one operation of `resolution` left unresolved, and its words.
    fn left_in_words(resolution: &Resolution) -> String {
        match &resolution.unresolved[..] {
            [unresolved] => match &unresolved.reason {
                UnresolvedReason::NoLongerResolves { detail, .. } => detail.clone(),
                other => panic!("no longer resolves: {other:?}"),
            },
            other => panic!("one operation is unresolved: {other:?}"),
        }
    }

    /// **A number past what the allocator counts is not numbered below**:
    /// the creation is left unresolved naming the file, and so is one whose
    /// next number would pass the largest.
    #[test]
    fn a_number_past_the_largest_is_unresolved_naming_the_file() {
        for held in [
            "tasks/NORN-99999999999999999999999.md",
            "tasks/NORN-18446744073709551615.md",
        ] {
            let resolution = planned(&MemoryVault::with(&[(held, "huge\n")]), vec![task()]);
            let detail = left_in_words(&resolution);
            assert!(detail.contains(held), "{held}: {detail}");
            assert!(resolution.plan.transitions.is_empty(), "{held}");
        }
    }

    /// **A slot at the vault root counts the root's own names.**
    #[test]
    fn a_slot_at_the_root_counts_the_roots_names() {
        let at_root = schema(b"version: 1\ninbox:\n  target: \"{{date}}-{{seq}}.md\"\n");
        let vault =
            MemoryVault::with(&[("2026-10-01-2.md", "2\n"), ("deep/2026-10-01-7.md", "7\n")]);
        let resolution = planned_reading(
            &vault,
            &at_root,
            vec![by_rule(None, &[], ValueMap::default(), Some("Call Sam.\n"))],
            Ok(reading()),
            &Cell::new(0),
        );
        assert_eq!(
            kinds(&resolution),
            vec![create("2026-10-01-3.md", "Call Sam.\n")]
        );
    }
}
