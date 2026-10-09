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
//! **Re-planning never renumbers.** A resolved plan the applier refuses
//! answers a fresh plan of its operations, and those are the concrete
//! creates, never the rule, so the fresh plan cannot allocate again: a
//! caller that wants a new number re-sends its operations, which plan anew.
//! A resolved plan re-sent writes the path it was previewed with, which may
//! be a number freed after the preview — the resolved plan takes precedence
//! over what allocating now would give. Two writers landing identical bytes
//! at one name land one document: the second finds its create already
//! landed and is reported found, not wrote.
//!
//! **The rules are the schema the plan is judged under**: the entry's pinned
//! schema, the one its plan ground carries and the applier's schema check
//! judges a composed result by ([`Rules`]). A rule name the schema does not
//! declare, and a creation naming no rule where the schema declares no inbox,
//! do not resolve.
//!
//! **One clock reading per plan.** The clock is read the first time a
//! creation of the plan needs it — any creation by rule, whose target and
//! templates may read it, and any creation where a rule default reading
//! `{{now}}`, `{{date}}` or `{{time}}` is proposed for a field or compared
//! with a filled value, even one that then turns out to conflict — and never
//! for a plan whose creations need none, so a document created at a path
//! where no default reading a clock token is proposed or compared never
//! depends on the clock; every template and every default of the plan fills
//! from that one reading. A clock outside the
//! years `{{date}}` can write leaves every creation of the plan that needs
//! it unresolved, saying so.
//!
//! **What a caller supplies is judged against the rule.** Every variable the
//! rule declares must be supplied, and none it does not declare may be: a
//! variable the rule would not read is left unresolved rather than dropped.
//! The inbox declares none. A value that would break the target's path — one
//! a [`FillError`](norn_config::schema::FillError) names — leaves the
//! operation unresolved naming the token and the value, or the path it
//! filled to.
//!
//! **The document's text** ([`composed`]): the rule's frontmatter defaults,
//! each string scalar filled as a template and every type and order kept,
//! then the caller's fields laid over them — a field the defaults hold keeps
//! its place and takes the caller's value, and a new one follows in the
//! caller's order. A caller's value is typed and written as it is, never
//! filled as a template, converted one to one as a `set`'s value is. The body
//! is the caller's where it sends one, else the rule's body template filled,
//! else empty. The inbox has neither frontmatter defaults nor a body
//! template, so a capture is the caller's fields and body before the rule
//! defaults.
//!
//! **Then the rule defaults** ([`defaulted`]): the schema rules matching the
//! document fill each required field its caller and its creation rule left
//! out, to a fixpoint ([`VaultSchema::fill_rule_defaults`]), each set into
//! the composed document as a `set` writes a field. A key either sends is
//! theirs, null included: **omit a key to take its default; send null to ask
//! for no value**, which the write gate judges — a required field held null
//! refuses as missing — and no line the caller sent is rewritten. A document created at a
//! path takes the same chain, its own frontmatter its caller's values. A
//! disagreement leaves the creation unresolved as
//! [`UnresolvedReason::DefaultsConflict`], naming each field and every
//! candidate with its rules, and a default read from a capture bound several
//! ways as [`UnresolvedReason::AmbiguousCapture`]. The document is written
//! through `norn-text`'s one renderer, its frontmatter block with LF line
//! endings and its body exactly as sent — a body's own breaks, CRLF
//! included, and an unterminated last line are kept, so a CRLF body sits
//! under an LF block as sent. One holding no field is
//! its body alone, with no empty frontmatter block, unless the reader would
//! take the body's first line as opening a block (`norn-text`'s own fence
//! rule, a byte-order mark and any line break included): that body is set
//! under an empty block, so it reads back as no field and the body as sent,
//! never as fields the caller did not send. A document the
//! renderer refuses — a block past the bound the reader admits among them —
//! leaves the operation unresolved naming the refusal.
//!
//! **`{{seq}}` is the highest number already used, plus one** ([`Numbering`]),
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
//! from 1. A directory at a matching name counts as a file does: it
//! occupies the name. The folder is listed once per slot for a plan, and its
//! listing is folded into a running highest number as it streams, never
//! collected. Allocation is a reading, not a reservation: a name another
//! writer takes before the plan lands is a taken name the applier refuses,
//! as any create's is, and the caller plans again.
//!
//! **Known limits.** On a root that folds case, a rule whose target folder is
//! spelled in another case than the vault lists it never creates: its names
//! are counted, but the create is put at the target's spelling, which the
//! planner does not respell, and is left unresolved naming the spelling the
//! vault lists. A name in another Unicode normalization than the target's is
//! not counted — the one normalization point folds ASCII case only — so a
//! number it holds may be allocated again.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::ops::ControlFlow;
use std::path::Path;

use norn_config::schema::{
    CaseFold, CreationRule, LocalTimestamp, NotALocalTimestamp, RuleDefaultsRefusal, RuleWork,
    SeqSlot, Target, TemplateValues, VaultSchema,
};
use norn_text::{LineEnding, Mapping, opens_frontmatter, render_document};
use norn_wire::{
    ConflictingDefault, DefaultCandidate, DocumentPath, Operation, OperationKind, UnresolvedReason,
    ValueMap, Variables, WriteTarget,
};

use super::edit::{edited, text_value};
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
    let mut numbering = None;
    let mut reading = None;
    let case = crate::stored_path_order(view.normalizer().case_sensitivity()).glob_case();
    let defaults = states_rule_defaults(rules.schema);
    for position in 0..operations.len() {
        match &operations[position].kind {
            OperationKind::CreateByRule {
                rule,
                variables,
                fields,
                body,
            } => {
                let at = *reading.get_or_insert_with(|| (rules.clock)());
                let numbering = numbering.get_or_insert_with(|| Numbering::of(operations));
                let asked = Asked {
                    rule: rule.as_deref(),
                    variables,
                    fields,
                    body: body.as_deref(),
                };
                match made(&asked, rules.schema, at, case, numbering, view)? {
                    Ok((path, content)) => {
                        numbering.arrivals.push(path.as_str().to_string());
                        operations[position].kind = OperationKind::create_document(path, content);
                    }
                    Err(reason) => {
                        left_out.insert(position, reason);
                    }
                }
            }
            OperationKind::CreateDocument { path, content } if defaults => {
                let mut clock = || *reading.get_or_insert_with(|| (rules.clock)());
                match defaulted(path, content, rules.schema, &mut clock, case) {
                    Ok(Some(filled)) => {
                        operations[position].kind =
                            OperationKind::create_document(path.clone(), filled);
                    }
                    Ok(None) => {}
                    Err(reason) => {
                        left_out.insert(position, reason);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether some schema rule states a default, which a document created at a
/// path may take.
fn states_rule_defaults(schema: &VaultSchema) -> bool {
    schema
        .rules()
        .any(|rule| rule.required().any(|(_, default)| default.is_some()))
}

/// The text `content` — a document a caller creates at `path` — takes once
/// the rule defaults fill each required field its own frontmatter leaves out
/// ([`VaultSchema::fill_rule_defaults`]), `clock` read only where a default
/// reading it is proposed for a field or compared with a filled value;
/// `None` where none fills; or why it takes none. A key
/// the frontmatter holds null stands. What the fixpoint paid is tallied on
/// the logical rule counters, refused or not.
///
/// **The caller's frontmatter is its values, and every byte it sent stays.**
/// Each filled field is set into the document through the one composition a
/// `set` writes a field by ([`edited`]): at the end of the block it carries,
/// or in a new block where it carries none, its body and the block's own
/// spelling kept, and a composition that would drop a comment refused as a
/// `set` refuses it. A document whose fields cannot be read — bytes no
/// document decodes from, a block nothing reads, a block whose top level is
/// no map — fills nothing, and the write gate judges what was sent.
fn defaulted(
    path: &DocumentPath,
    content: &str,
    schema: &VaultSchema,
    clock: &mut dyn FnMut() -> Result<LocalTimestamp, NotALocalTimestamp>,
    case: CaseFold,
) -> Result<Option<String>, UnresolvedReason> {
    let Some(fields) = crate::derivation::written_fields(content.as_bytes()) else {
        return Ok(None);
    };
    let mut work = RuleWork::default();
    let filled = schema.fill_rule_defaults(&fields, path.as_str(), clock, case, &mut work);
    crate::evidence::count_rule_work(work);
    let filled = filled.map_err(refused_defaults)?;
    if filled.is_empty() {
        return Ok(None);
    }
    let mut bytes: std::sync::Arc<[u8]> = std::sync::Arc::from(content.as_bytes());
    for (field, value) in filled {
        let set = OperationKind::set_frontmatter(WriteTarget::path(path.clone()), field, value);
        bytes = edited(&set, &bytes).map_err(|detail| {
            UnresolvedReason::no_longer_resolves(format!(
                "a rule default cannot be set into the document: {detail}"
            ))
        })?;
    }
    Ok(Some(
        String::from_utf8(bytes.to_vec()).expect("a composed document is UTF-8 text"),
    ))
}

/// The unresolved reason a defaults refusal is answered as.
fn refused_defaults(refusal: RuleDefaultsRefusal) -> UnresolvedReason {
    match refusal {
        RuleDefaultsRefusal::Conflict { fields } => UnresolvedReason::defaults_conflict(
            fields
                .iter()
                .map(|conflict| {
                    ConflictingDefault::new(
                        conflict.field(),
                        conflict
                            .candidates()
                            .iter()
                            .map(|candidate| {
                                DefaultCandidate::new(
                                    candidate.value().clone(),
                                    candidate.rules().iter().cloned(),
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
        ),
        RuleDefaultsRefusal::AmbiguousCapture {
            rule,
            field,
            bindings,
        } => {
            let [first, second] = *bindings;
            let named = |captures: norn_wire::Captures| {
                captures
                    .iter()
                    .map(|(name, segment)| (name.to_string(), segment.to_string()))
                    .collect()
            };
            UnresolvedReason::ambiguous_capture(rule, field, [named(first), named(second)])
        }
        RuleDefaultsRefusal::NoClockReading(unread) => {
            UnresolvedReason::no_longer_resolves(format!(
                "the host's clock cannot be read as a local time the rule defaults can fill: \
                 {unread}"
            ))
        }
    }
}

/// What a plan's numbers are allocated past, gathered the first time one of
/// its operations creates by rule, and never for a plan that does not.
struct Numbering {
    /// Every name the plan puts a document at, each a number a slot may
    /// already hold; each creation by rule adds the one it takes.
    arrivals: Vec<String>,
    /// The highest number each slot's folder lists, or why none can be
    /// counted, by the slot's folder, prefix and suffix: a folder is listed
    /// once for a plan, however many of its creations number in that slot.
    listed: BTreeMap<(String, String, String), Result<u64, String>>,
}

impl Numbering {
    /// The numbering of a plan of `operations`, its slots not yet listed.
    fn of(operations: &[Operation]) -> Self {
        let arrivals = operations
            .iter()
            .filter_map(|operation| match &operation.kind {
                OperationKind::CreateDocument { path, .. } => Some(path.as_str().to_string()),
                OperationKind::MoveDocument { to, .. } => Some(to.as_str().to_string()),
                _ => None,
            })
            .collect();
        Numbering {
            arrivals,
            listed: BTreeMap::new(),
        }
    }

    /// The number the next document in `slot` takes: one past the highest
    /// any name in the slot's folder, or any of the plan's arrivals, already
    /// holds there; or why there is none, in words.
    fn allocated<V: VaultView>(
        &mut self,
        slot: &SeqSlot,
        view: &V,
    ) -> Result<Result<u64, String>, V::Error> {
        let key = (
            slot.folder().to_string(),
            slot.prefix().to_string(),
            slot.suffix().to_string(),
        );
        let listed = match self.listed.get(&key) {
            Some(listed) => listed.clone(),
            None => {
                let listed = listed_highest(slot, view)?;
                self.listed.insert(key, listed.clone());
                listed
            }
        };
        let mut highest = match listed {
            Ok(highest) => highest,
            Err(detail) => return Ok(Err(detail)),
        };
        for path in &self.arrivals {
            match numbered(slot, path, view) {
                None => {}
                Some(Some(seq)) => highest = highest.max(seq),
                Some(None) => return Ok(Err(past_the_largest(path))),
            }
        }
        Ok(highest.checked_add(1).ok_or_else(|| {
            format!(
                "a document is already numbered {highest} in `{}`, the largest number a document can take",
                slot.path(highest)
            )
        }))
    }
}

/// The highest number any name `slot`'s folder lists holds there — 0 where
/// none does, or the folder does not stand — or why it cannot be counted,
/// in words.
///
/// **The listing is folded as it streams**, never collected, so counting a
/// folder holds one name however wide it is. A name that is not UTF-8 is no
/// slot's text. A directory at a matching name counts like a file: it
/// occupies the name, so the number it spells is used — up to a name past
/// the largest number, which leaves the creation unresolved naming it.
fn listed_highest<V: VaultView>(slot: &SeqSlot, view: &V) -> Result<Result<u64, String>, V::Error> {
    let mut highest = 0;
    let mut past = None;
    let mut visit = |name: &OsStr| {
        let Some(name) = name.to_str() else {
            return ControlFlow::Continue(());
        };
        let path = at_folder(slot.folder(), name);
        match numbered(slot, &path, view) {
            None => ControlFlow::Continue(()),
            Some(Some(seq)) => {
                highest = highest.max(seq);
                ControlFlow::Continue(())
            }
            Some(None) => {
                past = Some(past_the_largest(&path));
                ControlFlow::Break(())
            }
        }
    };
    if slot.folder().is_empty() {
        view.visit_root_names(&mut visit)?;
    } else if let Ok(folder) = view.normalizer().normalize(Path::new(slot.folder())) {
        // A slot's folder is a filled target's, which schema read and the
        // fill hold to a path inside the vault, so it always normalizes.
        view.visit_folder_names(&folder, &mut visit)?;
    }
    Ok(past.map_or(Ok(highest), Err))
}

/// Why `path` cannot be counted, in words.
fn past_the_largest(path: &str) -> String {
    format!("`{path}` is numbered past the largest number a document can take")
}

/// What one `create_by_rule` asks for.
struct Asked<'a> {
    rule: Option<&'a str>,
    variables: &'a Variables,
    fields: &'a ValueMap,
    body: Option<&'a str>,
}

/// The path and text of the document `asked` makes under `schema`, read at
/// `at`, numbered by `numbering` past what `view` lists, the rule defaults
/// filled beneath the caller's values and its creation rule's
/// ([`defaulted`]), its rules' path globs comparing letters as `case` says;
/// or why it makes none.
fn made<V: VaultView>(
    asked: &Asked<'_>,
    schema: &VaultSchema,
    at: Result<LocalTimestamp, NotALocalTimestamp>,
    case: CaseFold,
    numbering: &mut Numbering,
    view: &V,
) -> Result<Result<(DocumentPath, String), UnresolvedReason>, V::Error> {
    let (path, content) = match composed_by_rule(asked, schema, at, numbering, view)? {
        Ok(made) => made,
        Err(detail) => return Ok(Err(UnresolvedReason::no_longer_resolves(detail))),
    };
    Ok(match defaulted(&path, &content, schema, &mut || at, case) {
        Ok(Some(filled)) => Ok((path, filled)),
        Ok(None) => Ok((path, content)),
        Err(reason) => Err(reason),
    })
}

/// The path and text of the document `asked` makes under `schema` before
/// any rule default fills it — the caller's values over its creation rule's
/// defaults — read at `at`, numbered by `numbering` past what `view` lists;
/// or why it makes none, in words.
fn composed_by_rule<V: VaultView>(
    asked: &Asked<'_>,
    schema: &VaultSchema,
    at: Result<LocalTimestamp, NotALocalTimestamp>,
    numbering: &mut Numbering,
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
    let cannot = |detail: String| format!("{named} cannot make a document: {detail}");
    let values = match target.seq_slot(&values) {
        Err(error) => return Ok(Err(cannot(error.to_string()))),
        Ok(None) => values,
        Ok(Some(slot)) => match numbering.allocated(&slot, view)? {
            Ok(seq) => values.with_seq(seq),
            Err(detail) => return Ok(Err(format!("{named} cannot number a document: {detail}"))),
        },
    };
    let path = match filled_path(target, &values) {
        Ok(path) => path,
        Err(detail) => return Ok(Err(cannot(detail))),
    };
    Ok(composed(rule, asked, &values)
        .map(|content| (path, content))
        .map_err(cannot))
}

/// The document path `target` fills to under `values`, or why it fills to
/// none, in words.
fn filled_path(target: &Target, values: &TemplateValues) -> Result<DocumentPath, String> {
    target.fill(values).map_err(|error| error.to_string())
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
    // A body laid down alone that the reader would take as opening a block
    // would read back as fields nobody sent, or as a block it refuses; under
    // an empty block it is body, as sent.
    if frontmatter.is_empty() && !opens_frontmatter(&body) {
        return Ok(body);
    }
    render_document(&frontmatter, &body, LineEnding::Lf)
        .map_err(|error| format!("its document cannot be written: {error}"))
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

    /// **The number is cut where the slot's text puts it, digits around it
    /// included**: a slot whose prefix and suffix end and start in digits
    /// counts only the digits between them, and a name with anything else
    /// there, in another case, or in a deeper folder counts for nothing.
    #[test]
    fn the_number_is_cut_between_the_slots_own_text() {
        let digits_around = schema(b"version: 1\ninbox:\n  target: \"tasks/T7{{seq}}9.md\"\n");
        let vault = MemoryVault::with(&[
            ("tasks/T700079.md", "7\n"),
            ("tasks/T799x9.md", "not a number\n"),
            ("tasks/t7999.md", "another case\n"),
            ("tasks/deep/T7999.md", "another folder\n"),
        ]);
        let resolution = planned_reading(
            &vault,
            &digits_around,
            vec![by_rule(None, &[], ValueMap::default(), Some("body\n"))],
            Ok(reading()),
            &Cell::new(0),
        );
        assert_eq!(landed_at(&resolution), "tasks/T789.md");
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

    /// **A slot's folder is listed once for a plan**, however many of its
    /// creations number in that slot, and each still numbers past the last.
    #[test]
    fn a_slots_folder_is_listed_once_per_plan() {
        let vault = MemoryVault::with(&[("tasks/NORN-1.md", "1\n")]);
        let resolution = planned(&vault, vec![task(), task(), task()]);
        let paths: Vec<String> = kinds(&resolution)
            .iter()
            .map(|kind| match kind {
                OperationKind::CreateDocument { path, .. } => path.as_str().to_string(),
                other => panic!("a create: {other:?}"),
            })
            .collect();
        assert_eq!(
            paths,
            ["tasks/NORN-2.md", "tasks/NORN-3.md", "tasks/NORN-4.md"]
        );
        assert_eq!(*vault.listings.borrow(), [("tasks".to_string(), 1)].into());
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

    /// **A create earlier in the plan holds its number by itself**: with
    /// the folder empty, the rule numbers past the name the plan creates.
    #[test]
    fn a_create_earlier_in_the_plan_holds_its_number() {
        let creating = Operation::new(create("tasks/NORN-4.md", "4\n"));
        let resolution = planned(&MemoryVault::default(), vec![creating, task()]);
        let OperationKind::CreateDocument { path, .. } = &kinds(&resolution)[1] else {
            panic!("the creation by rule expands into a create");
        };
        assert_eq!(path.as_str(), "tasks/NORN-5.md");
    }

    /// **A name the plan fills in another folder holds no number of the
    /// slot**, though its file name is the slot's.
    #[test]
    fn a_name_the_plan_fills_in_another_folder_holds_no_number() {
        let vault = MemoryVault::with(&[("tasks/x.md", "x\n")]);
        let moving = Operation::new(OperationKind::move_document(
            DocumentPath::new("tasks/x.md").expect("a path"),
            DocumentPath::new("done/NORN-9.md").expect("a path"),
        ));
        let resolution = planned(&vault, vec![moving, task()]);
        let OperationKind::CreateDocument { path, .. } = &kinds(&resolution)[1] else {
            panic!("the creation by rule expands into a create");
        };
        assert_eq!(path.as_str(), "tasks/NORN-1.md");
    }

    /// **The highest number counts wherever the listing puts it**: the
    /// memory vault lists names in byte order, so `NORN-9.md` is listed after
    /// `NORN-10.md`, and the last name listed is not the highest.
    #[test]
    fn the_highest_number_counts_wherever_it_is_listed() {
        let vault = MemoryVault::with(&[("tasks/NORN-9.md", "9\n"), ("tasks/NORN-10.md", "10\n")]);
        assert_eq!(
            landed_at(&planned(&vault, vec![task()])),
            "tasks/NORN-11.md"
        );
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

    /// **A known limit: on a root that folds case, a target folder spelled
    /// in another case than the vault lists never creates.** Its names are
    /// counted, but the create is put at the target's spelling, which the
    /// planner does not respell, so it is left unresolved naming the spelling
    /// the vault lists.
    #[test]
    fn a_target_folder_in_another_case_never_creates_on_a_folding_root() {
        let cased = schema(b"version: 1\ninbox:\n  target: \"Tasks/{{seq}}.md\"\n");
        let vault = MemoryVault::with(&[("tasks/1.md", "1\n")]).folding_case();
        let resolution = planned_reading(
            &vault,
            &cased,
            vec![by_rule(None, &[], ValueMap::default(), Some("x\n"))],
            Ok(reading()),
            &Cell::new(0),
        );
        let detail = left_in_words(&resolution);
        assert!(
            detail.contains("`Tasks/2.md` is spelled `tasks/2.md` in the vault"),
            "{detail}"
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

    /// The words `operation` is left unresolved with, planned alone over an
    /// empty vault under `SCHEMA`; nothing is written.
    fn refused_alone(operation: Operation) -> String {
        let resolution = planned(&MemoryVault::default(), vec![operation.clone()]);
        assert!(resolution.plan.transitions.is_empty());
        assert_eq!(resolution.unresolved[0].operation, operation);
        left_in_words(&resolution)
    }

    /// **A rule the schema does not declare is unresolved, naming it.**
    #[test]
    fn an_unknown_rule_is_unresolved_naming_it() {
        let detail = refused_alone(by_rule(Some("meeting"), &[], ValueMap::default(), None));
        assert!(detail.contains("no creation rule `meeting`"), "{detail}");
    }

    /// **A creation naming no rule where the schema declares no inbox is
    /// unresolved, saying no inbox is declared.**
    #[test]
    fn a_capture_with_no_inbox_declared_is_unresolved_saying_so() {
        let no_inbox = schema(b"version: 1\n");
        let capture = by_rule(None, &[], ValueMap::default(), Some("Call Sam.\n"));
        let resolution = planned_reading(
            &MemoryVault::default(),
            &no_inbox,
            vec![capture],
            Ok(reading()),
            &Cell::new(0),
        );
        let detail = left_in_words(&resolution);
        assert!(detail.contains("no inbox is declared"), "{detail}");
    }

    /// **A variable the rule declares and the caller does not supply leaves
    /// the creation unresolved, naming the variable**, even one no template
    /// of the rule reads.
    #[test]
    fn a_missing_variable_is_unresolved_naming_it() {
        let detail = refused_alone(by_rule(
            Some("task"),
            &[("project", "NORN")],
            ValueMap::default(),
            Some("Body.\n"),
        ));
        assert!(detail.contains("`title`"), "{detail}");
        assert!(detail.contains("no value is supplied"), "{detail}");
    }

    /// **A variable the rule does not declare is unresolved, naming it**,
    /// rather than dropped; the inbox declares none.
    #[test]
    fn an_undeclared_variable_is_unresolved_naming_it() {
        let detail = refused_alone(by_rule(
            Some("task"),
            &[("project", "NORN"), ("title", "T"), ("owner", "drew")],
            ValueMap::default(),
            None,
        ));
        assert!(detail.contains("declares no variable `owner`"), "{detail}");
        let detail = refused_alone(by_rule(
            None,
            &[("owner", "drew")],
            ValueMap::default(),
            None,
        ));
        assert!(
            detail.contains("the inbox declares no variable `owner`"),
            "{detail}"
        );
    }

    /// **A value that would break the target's path is unresolved, naming
    /// the token and the value.**
    #[test]
    fn an_unsafe_value_is_unresolved_naming_the_token() {
        let detail = refused_alone(by_rule(
            Some("task"),
            &[("project", "../up"), ("title", "T")],
            ValueMap::default(),
            None,
        ));
        assert!(
            detail.contains("{{var.project}}") && detail.contains("../up"),
            "{detail}"
        );
    }

    /// A schema whose rules fill whole path segments from their variables, so
    /// what their targets say about a place is decided only once filled.
    const SEGMENT_SCHEMA: &[u8] = b"version: 1
creatable:
  note:
    target: \"notes/{{var.name}}.md\"
    variables: [name]
  filed:
    target: \"{{var.dir}}/{{var.file}}/x.md\"
    variables: [dir, file]
";

    /// `operation` planned alone over an empty vault under
    /// [`SEGMENT_SCHEMA`], which writes nothing: the operation left
    /// unresolved, and its words.
    fn refused_by_segments(operation: Operation) -> (Operation, String) {
        let resolution = planned_reading(
            &MemoryVault::default(),
            &schema(SEGMENT_SCHEMA),
            vec![operation],
            Ok(reading()),
            &Cell::new(0),
        );
        assert!(resolution.plan.transitions.is_empty());
        (
            resolution.unresolved[0].operation.clone(),
            left_in_words(&resolution),
        )
    }

    /// **The strict document-path grammar judges the path a target fills to,
    /// not the target as written**: `notes/{{var.name}}.md` is a legal
    /// target, and a value carrying a control character — which no value
    /// rule refuses on its own — fills it to a path the grammar refuses. The
    /// creation is unresolved naming that filled path and the grammar's
    /// reason.
    #[test]
    fn a_filled_path_the_document_grammar_refuses_is_unresolved_naming_it() {
        let filled = "notes/a\u{7}b.md";
        assert_eq!(
            norn_wire::PathProblem::of_document(filled),
            Some(norn_wire::PathProblem::ControlCharacter)
        );
        let operation = by_rule(
            Some("note"),
            &[("name", "a\u{7}b")],
            ValueMap::default(),
            None,
        );
        let (left, detail) = refused_by_segments(operation.clone());
        assert_eq!(left, operation, "the rule fills to no path to create at");
        assert!(detail.contains(&format!("{filled:?}")), "{detail}");
        assert!(
            detail.contains(norn_wire::PathProblem::ControlCharacter.message()),
            "{detail}"
        );
    }

    /// **A target filled to a control file's place is refused by the gate a
    /// document operation meets there**: `{{var.dir}}/{{var.file}}/x.md`
    /// names no control file as written, and filled with `.norn` and a
    /// control file's name it lies beneath that file. What is left unresolved
    /// is the concrete create the rule expanded into, at the filled path, in
    /// the words a create written there by hand meets.
    #[test]
    fn a_filled_path_beneath_a_control_file_is_unresolved_by_the_control_file_gate() {
        for (file, role) in [("schema.yaml", "schema"), ("config.toml", "config")] {
            let filled = format!(".norn/{file}/x.md");
            let (left, detail) = refused_by_segments(by_rule(
                Some("filed"),
                &[("dir", ".norn"), ("file", file)],
                ValueMap::default(),
                None,
            ));
            assert_eq!(left.kind, create(&filled, ""));
            let (_, by_hand) = refused_by_segments(Operation::new(create(&filled, "")));
            assert_eq!(detail, by_hand);
            assert!(
                detail.contains(&format!(
                    "`.norn/{file}/x.md` lies beneath the vault {role}'s path"
                )),
                "{detail}"
            );
        }
    }

    /// **A document the renderer refuses is unresolved, naming the
    /// refusal**: frontmatter past the bound the reader admits would read as
    /// none.
    #[test]
    fn an_oversized_composition_is_unresolved_naming_the_bound() {
        let huge = "x".repeat(norn_text::FRONTMATTER_MAX_BYTES);
        let detail = refused_alone(by_rule(
            Some("task"),
            &[("project", "NORN"), ("title", "T")],
            fields(vec![("notes", AuthoredValue::string(huge))]),
            None,
        ));
        assert!(detail.contains("cannot be written"), "{detail}");
        assert!(
            detail.contains(&norn_text::FRONTMATTER_MAX_BYTES.to_string()),
            "{detail}"
        );
    }

    /// **The clock is read once for a plan, and only for a plan whose
    /// creations may fill from it**: every creation of the plan fills from
    /// that one reading, and a document created at a path under a schema
    /// stating no rule default reads none.
    #[test]
    fn the_clock_is_read_once_per_plan_and_only_when_a_creation_may_fill_from_it() {
        let reads = Cell::new(0);
        let resolution = planned_reading(
            &MemoryVault::default(),
            &schema(SCHEMA),
            vec![
                task(),
                task(),
                by_rule(None, &[], ValueMap::default(), None),
            ],
            Ok(reading()),
            &reads,
        );
        assert_eq!(kinds(&resolution).len(), 3);
        assert_eq!(reads.get(), 1);

        let reads = Cell::new(0);
        planned_reading(
            &MemoryVault::default(),
            &schema(SCHEMA),
            vec![Operation::new(create("a.md", "a\n"))],
            Ok(reading()),
            &reads,
        );
        assert_eq!(reads.get(), 0);
    }

    /// **A clock no template can write leaves every creation by rule
    /// unresolved, naming the clock**, and the plan's other operations
    /// resolve.
    #[test]
    fn a_clock_outside_the_calendar_leaves_each_creation_unresolved() {
        let resolution = planned_reading(
            &MemoryVault::default(),
            &schema(SCHEMA),
            vec![task(), Operation::new(create("a.md", "a\n")), task()],
            Err(NotALocalTimestamp),
            &Cell::new(0),
        );
        assert_eq!(
            resolution.plan.operations,
            vec![Operation::new(create("a.md", "a\n"))]
        );
        assert_eq!(resolution.unresolved.len(), 2);
        for left in &resolution.unresolved {
            let UnresolvedReason::NoLongerResolves { detail, .. } = &left.reason else {
                panic!("left out for {:?}", left.reason);
            };
            assert!(detail.contains("clock"), "{detail}");
        }
    }

    /// **A capture under a schema stating no rule default is exactly the
    /// caller's fields and body**, in the inbox, numbered; one with no field
    /// is its body alone, with no empty frontmatter block.
    #[test]
    fn a_capture_under_no_rule_default_is_exactly_the_callers_fields_and_body() {
        let vault = MemoryVault::with(&[("inbox/2026-10-01-1.md", "earlier\n")]);
        let resolution = planned(
            &vault,
            vec![
                by_rule(
                    None,
                    &[],
                    fields(vec![("source", AuthoredValue::string("phone"))]),
                    Some("Call Sam.\n"),
                ),
                by_rule(None, &[], ValueMap::default(), Some("Buy milk.\n")),
            ],
        );
        assert_eq!(
            kinds(&resolution),
            vec![
                create(
                    "inbox/2026-10-01-2.md",
                    "---\nsource: phone\n---\nCall Sam.\n"
                ),
                create("inbox/2026-10-01-3.md", "Buy milk.\n"),
            ]
        );
    }

    /// The one create of `resolution`: its path and content.
    fn the_create(resolution: &Resolution) -> (String, String) {
        match &kinds(resolution)[..] {
            [OperationKind::CreateDocument { path, content }] => {
                (path.as_str().to_string(), content.clone())
            }
            other => panic!("one create is planned: {other:?}"),
        }
    }

    /// **A body the reader would take as opening a frontmatter block is set
    /// under an empty one where no field is sent**, so it reads back as no
    /// field and the body exactly as sent — whichever break ends its fence,
    /// behind a byte-order mark too — and a fence too large for the reader to
    /// admit is body content, not a block refused; a body opening no block is
    /// written alone.
    #[test]
    fn a_body_opening_a_fence_with_no_field_is_set_under_an_empty_block() {
        let oversized = format!(
            "---\nnotes: {}\n---\nBody.\n",
            "x".repeat(norn_text::FRONTMATTER_MAX_BYTES)
        );
        for body in [
            "---\nstatus: forged\n---\nreal\n",
            "---\r\nstatus: forged\r\n---\r\nreal\r\n",
            "\u{feff}---\nstatus: forged\n---\nreal\n",
            "---\nnever closed\n",
            &oversized,
        ] {
            let capture = by_rule(None, &[], ValueMap::default(), Some(body));
            let (_, content) = the_create(&planned(&MemoryVault::default(), vec![capture]));
            assert_eq!(content, format!("---\n---\n{body}"));
            let read = norn_text::Document::parse(&content);
            // An empty block reads as null: no field.
            assert_eq!(
                read.frontmatter(),
                Some(&norn_text::Value::Null),
                "{body:?}"
            );
            assert!(read.diagnostics().is_empty(), "{:?}", read.diagnostics());
            assert_eq!(read.body(), body);
        }
        for body in ["--- not a fence\n", "Call Sam.\n---\n", ""] {
            let capture = by_rule(None, &[], ValueMap::default(), Some(body));
            let (_, content) = the_create(&planned(&MemoryVault::default(), vec![capture]));
            assert_eq!(content, body);
        }
    }

    /// **A rule's body template opening a fence is set under an empty block
    /// too**, where the rule holds no default and the caller sends no field.
    #[test]
    fn a_body_template_opening_a_fence_is_set_under_an_empty_block() {
        let fenced = schema(
            b"version: 1\ncreatable:\n  note:\n    target: \"notes/{{seq}}.md\"\n    body: \"---\\nstatus: forged\\n---\\n\"\n",
        );
        let resolution = planned_reading(
            &MemoryVault::default(),
            &fenced,
            vec![by_rule(Some("note"), &[], ValueMap::default(), None)],
            Ok(reading()),
            &Cell::new(0),
        );
        assert_eq!(
            kinds(&resolution),
            vec![create("notes/1.md", "---\n---\n---\nstatus: forged\n---\n")]
        );
    }

    /// **A caller's body is written exactly as sent, under fields too**: an
    /// unterminated last line stays unterminated, and CRLF breaks stay CRLF
    /// under the block's LF lines.
    #[test]
    fn a_callers_body_is_written_exactly_as_sent_under_fields() {
        for body in ["Call Sam.", "Line one.\r\nLine two.\r\n"] {
            let capture = by_rule(
                None,
                &[],
                fields(vec![("source", AuthoredValue::string("phone"))]),
                Some(body),
            );
            let (_, content) = the_create(&planned(&MemoryVault::default(), vec![capture]));
            assert_eq!(content, format!("---\nsource: phone\n---\n{body}"));
        }
    }

    /// **A caller's body replaces the rule's body template**, and a caller's
    /// value is written as it is, never filled as a template.
    #[test]
    fn a_callers_body_and_values_are_written_as_sent() {
        let resolution = planned(
            &MemoryVault::default(),
            vec![by_rule(
                Some("task"),
                &[("project", "NORN"), ("title", "T")],
                fields(vec![("note", AuthoredValue::string("{{now}}"))]),
                Some("Mine.\n"),
            )],
        );
        assert_eq!(
            kinds(&resolution),
            vec![create(
                "tasks/NORN-1.md",
                "---\nstatus: todo\ncreated: 2026-10-01T09:30:15+02:00\nrank: 3\nnote: '{{now}}'\n---\nMine.\n"
            )]
        );
    }

    /// A task conditioned on `guard.md`'s `status` reading `unlocked`.
    fn guarded_task() -> Operation {
        task().with_conditions(vec![norn_wire::AuthorCondition::expected_value(
            DocumentPath::new("guard.md").expect("a path"),
            "status",
            norn_wire::ExpectedField::present(AuthoredValue::string("unlocked")),
        )])
    }

    /// **The expanded create keeps the creation's conditions**: where they
    /// hold it carries them, and where one fails it is left unresolved and
    /// nothing is written.
    #[test]
    fn the_expanded_create_keeps_its_conditions() {
        let holding = MemoryVault::with(&[("guard.md", "---\nstatus: unlocked\n---\n")]);
        let resolution = planned(&holding, vec![guarded_task()]);
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(
            resolution.plan.operations[0].conditions,
            guarded_task().conditions
        );

        let failing = MemoryVault::with(&[("guard.md", "---\nstatus: locked\n---\n")]);
        let resolution = planned(&failing, vec![guarded_task()]);
        assert!(resolution.plan.transitions.is_empty());
        let [left] = resolution.unresolved.as_slice() else {
            panic!("the creation is unresolved: {:?}", resolution.unresolved);
        };
        assert_eq!(left.operation.conditions, guarded_task().conditions);
    }

    /// **The expanded create keeps what the creation carries beyond its
    /// kind**: an operation requiring it by identifier requires the create.
    #[test]
    fn the_expanded_create_keeps_its_identifier_and_requirements() {
        let made = norn_wire::OperationId::new("make-task").expect("an identifier");
        let first = norn_wire::OperationId::new("first").expect("an identifier");
        let creating = Operation::new(create("a.md", "a\n")).with_id(first.clone());
        let by_task = task()
            .with_id(made.clone())
            .with_requires(vec![first.clone()]);
        let editing = Operation::new(OperationKind::str_replace(
            DocumentPath::new("tasks/NORN-1.md").expect("a path"),
            "Ship it",
            "Shipped",
        ))
        .with_requires(vec![made.clone()]);
        let resolution = planned(&MemoryVault::default(), vec![editing, by_task, creating]);
        let operations = &resolution.plan.operations;
        assert!(
            resolution.unresolved.is_empty(),
            "{:?}",
            resolution.unresolved
        );
        assert_eq!(operations.len(), 3);
        assert_eq!(operations[1].id.as_ref(), Some(&made));
        assert_eq!(operations[1].requires, vec![first]);
        assert!(matches!(
            &operations[1].kind,
            OperationKind::CreateDocument { path, .. } if path.as_str() == "tasks/NORN-1.md"
        ));
        assert_eq!(
            resolution.plan.transitions,
            vec![
                norn_wire::Transition::new(
                    DocumentPath::new("a.md").expect("a path"),
                    norn_wire::FileState::absent(),
                    norn_wire::FileState::present(super::super::compose::content_hash(b"a\n")),
                ),
                norn_wire::Transition::new(
                    DocumentPath::new("tasks/NORN-1.md").expect("a path"),
                    norn_wire::FileState::absent(),
                    norn_wire::FileState::present(super::super::compose::content_hash(
                        b"---\nstatus: todo\ncreated: 2026-10-01T09:30:15+02:00\nrank: 3\n---\n# Shipped\n"
                    )),
                ),
            ]
        );
    }

    /// A schema whose creation rule, inbox and schema rules each state a
    /// layer of a created document's values: the `task` rule defaults
    /// `kind: task` and `priority: low`; a rule on `kind: task` requires
    /// `priority` (default `high`), `owner` (default `nobody`) and `created`
    /// (default the clock); a vault-wide rule requires `kind` (default
    /// `note`); and a rule on `kind: note` requires `area` (default
    /// `general`).
    const LAYERED: &[u8] = b"version: 1
creatable:
  task:
    target: \"tasks/{{seq}}.md\"
    frontmatter_defaults:
      kind: task
      priority: low
inbox:
  target: \"inbox/{{seq}}.md\"
rules:
  tasks: { match: { frontmatter: { kind: task } }, required: { priority: { default: high }, owner: { default: nobody }, created: { default: '{{now}}' } } }
  every: { required: { kind: { default: note } } }
  notes: { match: { frontmatter: { kind: note } }, required: { area: { default: general } } }
";

    /// The one create `operations` plan to under [`LAYERED`]: its path and
    /// content.
    fn layered(operations: Vec<Operation>) -> (String, String) {
        the_create(&planned_reading(
            &MemoryVault::default(),
            &schema(LAYERED),
            operations,
            Ok(reading()),
            &Cell::new(0),
        ))
    }

    fn task_with(entries: Vec<(&str, AuthoredValue)>) -> Operation {
        by_rule(Some("task"), &[], fields(entries), None)
    }

    fn capture_with(entries: Vec<(&str, AuthoredValue)>) -> Operation {
        by_rule(None, &[], fields(entries), Some("Body.\n"))
    }

    /// **`new --as`: the caller's value stands over the creation rule's
    /// default and the rule default.** `priority: urgent` sent by the caller
    /// is written though the creation rule defaults `low` and a rule `high`.
    #[test]
    fn new_by_rule_takes_the_callers_value_over_every_default() {
        let (_, content) = layered(vec![task_with(vec![(
            "priority",
            AuthoredValue::string("urgent"),
        )])]);
        assert!(content.contains("priority: urgent\n"), "{content}");
        assert!(!content.contains("priority: low"), "{content}");
    }

    /// **`new --as`: the creation rule's default stands over the rule
    /// default**, and the rule defaults fill each required field the caller
    /// and the creation rule left missing, after them, from the one clock
    /// reading.
    #[test]
    fn new_by_rule_takes_the_creation_default_over_a_rule_default_and_rule_defaults_last() {
        let (path, content) = layered(vec![task_with(vec![])]);
        assert_eq!(path, "tasks/1.md");
        assert_eq!(
            content,
            "---\nkind: task\npriority: low\ncreated: 2026-10-01T09:30:15+02:00\nowner: nobody\n---\n"
        );
    }

    /// **Inbox capture: the caller's value stands over a rule default, and
    /// the rule defaults fill each required field it left missing.**
    #[test]
    fn a_capture_takes_the_callers_value_over_a_rule_default_and_rule_defaults_last() {
        let (path, content) = layered(vec![capture_with(vec![
            ("kind", AuthoredValue::string("task")),
            ("owner", AuthoredValue::string("me")),
        ])]);
        assert_eq!(path, "inbox/1.md");
        assert_eq!(
            content,
            "---\nkind: task\nowner: me\ncreated: 2026-10-01T09:30:15+02:00\npriority: high\n---\nBody.\n"
        );
    }

    /// **The fixpoint fills a default a filled default brings into scope**:
    /// a capture sending no field takes `kind: note` from the vault-wide
    /// rule, which brings in the rule on `kind: note` and its `area`.
    #[test]
    fn a_filled_default_brings_in_a_rule_whose_default_fills_too() {
        let (_, content) = layered(vec![capture_with(vec![])]);
        assert_eq!(content, "---\nkind: note\narea: general\n---\nBody.\n");
    }

    /// **`new` at a bare path: its own frontmatter is the caller's values,
    /// and each rule default is set into it as a `set` writes a field** — at
    /// the end of the block it carries, or in a new block where it carries
    /// none — every byte it sent otherwise kept: its body, and its block's
    /// own spelling.
    #[test]
    fn new_at_a_path_takes_rule_defaults_beneath_its_own_frontmatter() {
        let (_, fronted) = layered(vec![Operation::new(create(
            "notes/x.md",
            "---\nkind: task   # mine\nowner:   me\n---\nBody,\r\nkept.",
        ))]);
        assert_eq!(
            fronted,
            "---\nkind: task   # mine\nowner:   me\ncreated: 2026-10-01T09:30:15+02:00\npriority: high\n---\nBody,\r\nkept."
        );
        let (_, bare) = layered(vec![Operation::new(create("notes/y.md", "Body.\n"))]);
        assert_eq!(bare, "---\nkind: note\narea: general\n---\nBody.\n");
    }

    /// **A creation's defaults fixpoint reaches the logical rule counters**
    /// of the job planning it: a capture sending no field fills `kind` in
    /// the first round and `area` in the second, the third filling nothing,
    /// each round and the re-check evaluating the schema's three rules.
    #[test]
    fn a_creations_defaults_fixpoint_reaches_the_logical_rule_counters() {
        let evidence = std::sync::Arc::new(crate::evidence::JobEvidence::default());
        {
            let _job = evidence.attributing();
            layered(vec![capture_with(vec![])]);
        }
        let work = evidence.read().rule_work;
        assert_eq!(
            (
                work.defaults_rounds,
                work.defaults_filled,
                work.rules_evaluated
            ),
            (3, 2, 12),
            "{work:?}"
        );
    }

    /// **`new` at a bare path whose frontmatter does not read fills
    /// nothing**: the document is created exactly as sent, for the write gate
    /// to judge.
    #[test]
    fn new_at_a_path_whose_frontmatter_does_not_read_fills_nothing() {
        let sent = "---\nkind: [unclosed\n---\nBody.\n";
        let (_, content) = layered(vec![Operation::new(create("notes/x.md", sent))]);
        assert_eq!(content, sent);
    }

    /// **A caller block a `set` cannot edit refuses the create only where a
    /// default would fill**: a default is set into the caller's block by the
    /// one composition a `set` writes a field by, so a flow-style block
    /// missing `area` is left unresolved naming why, while one missing
    /// nothing is created exactly as sent.
    #[test]
    fn a_caller_block_a_set_cannot_edit_refuses_only_where_a_default_would_fill() {
        let resolution = planned_reading(
            &MemoryVault::default(),
            &schema(LAYERED),
            vec![Operation::new(create(
                "notes/x.md",
                "---\n{kind: note}\n---\nBody.\n",
            ))],
            Ok(reading()),
            &Cell::new(0),
        );
        let [left] = resolution.unresolved.as_slice() else {
            panic!("one operation left: {:?}", resolution.unresolved);
        };
        let UnresolvedReason::NoLongerResolves { detail, .. } = &left.reason else {
            panic!("a create that no longer resolves: {:?}", left.reason);
        };
        assert!(
            detail.starts_with("a rule default cannot be set into the document"),
            "{detail}"
        );
        let sent = "---\n{kind: note, area: home}\n---\nBody.\n";
        let (_, content) = layered(vec![Operation::new(create("notes/y.md", sent))]);
        assert_eq!(content, sent);
    }

    /// **A capture whose body opens a fence takes its defaults in the empty
    /// block the body is set under**: the body stays body, after the block
    /// the defaults fill.
    #[test]
    fn a_capture_body_opening_a_fence_takes_its_defaults_in_the_block_above_it() {
        let resolution = planned_reading(
            &MemoryVault::default(),
            &schema(LAYERED),
            vec![by_rule(
                None,
                &[],
                fields(vec![]),
                Some("---\nkind: task\n---\nBody\n"),
            )],
            Ok(reading()),
            &Cell::new(0),
        );
        assert_eq!(
            the_create(&resolution).1,
            "---\nkind: note\narea: general\n---\n---\nkind: task\n---\nBody\n"
        );
    }

    /// A schema whose vault-wide rule defaults `kind: task` and
    /// `status: todo` while a rule on `kind: task` defaults `status: done`,
    /// and whose two `area` rules default `area` apart in one round.
    const CONFLICTED: &[u8] = b"version: 1
creatable:
  task:
    target: \"tasks/{{seq}}.md\"
inbox:
  target: \"inbox/{{seq}}.md\"
rules:
  wide: { required: { kind: { default: task }, status: { default: todo } } }
  tasks: { match: { frontmatter: { kind: task } }, required: { status: { default: done } } }
  west: { match: { frontmatter: { side: both } }, required: { area: { default: west } } }
  east: { match: { frontmatter: { side: both } }, required: { area: { default: east } } }
";

    /// The one reason `operation` is left unresolved for under
    /// [`CONFLICTED`].
    fn conflicted(operation: Operation) -> UnresolvedReason {
        let resolution = planned_reading(
            &MemoryVault::default(),
            &schema(CONFLICTED),
            vec![operation],
            Ok(reading()),
            &Cell::new(0),
        );
        assert!(
            resolution.plan.operations.is_empty(),
            "{:?}",
            resolution.plan
        );
        let [left] = resolution.unresolved.as_slice() else {
            panic!("one operation left: {:?}", resolution.unresolved);
        };
        left.reason.clone()
    }

    fn conflict(field: &str, candidates: &[(&str, &[&str])]) -> UnresolvedReason {
        UnresolvedReason::defaults_conflict(vec![norn_wire::ConflictingDefault::new(
            field,
            candidates
                .iter()
                .map(|(value, rules)| {
                    norn_wire::DefaultCandidate::new(
                        AuthoredValue::string(*value),
                        rules.iter().map(|rule| (*rule).to_string()),
                    )
                })
                .collect(),
        )])
    }

    /// **The late conflict refuses naming the field and both candidates**,
    /// for `new --as`, inbox capture and `new` at a bare path alike: the
    /// vault-wide rule fills `kind: task` and `status: todo`, which brings in
    /// the rule on `kind: task` defaulting `status: done`.
    #[test]
    fn a_default_a_later_round_disagrees_with_refuses_naming_both_candidates() {
        let late = conflict("status", &[("todo", &["wide"]), ("done", &["tasks"])]);
        assert_eq!(
            conflicted(by_rule(Some("task"), &[], fields(vec![]), None)),
            late
        );
        assert_eq!(conflicted(capture_with(vec![])), late);
        assert_eq!(
            conflicted(Operation::new(create("notes/x.md", "Body.\n"))),
            late
        );
    }

    /// **Defaults disagreeing in one round refuse alike**, naming the field
    /// and each candidate with the rule proposing it.
    #[test]
    fn defaults_disagreeing_in_one_round_refuse_naming_each_candidate() {
        let fields_sent = vec![
            ("kind", AuthoredValue::string("note")),
            ("status", AuthoredValue::string("open")),
            ("side", AuthoredValue::string("both")),
        ];
        assert_eq!(
            conflicted(capture_with(fields_sent)),
            conflict("area", &[("east", &["east"]), ("west", &["west"])])
        );
    }

    /// **A default read from a capture its rule's `match.path` binds several
    /// ways refuses**, naming the rule, the field and two of the bindings,
    /// which bind `area` to the first and to the second segment of the path.
    #[test]
    fn a_default_read_from_a_capture_bound_several_ways_refuses() {
        let resolution = planned_reading(
            &MemoryVault::default(),
            &schema(
                b"version: 1
rules:
  areas: { match: { path: '**/<area>/**' }, required: { area: { default: '{{path.area}}' } } }
",
            ),
            vec![Operation::new(create("a/b/c.md", "Body.\n"))],
            Ok(reading()),
            &Cell::new(0),
        );
        let [left] = resolution.unresolved.as_slice() else {
            panic!("one operation left: {:?}", resolution.unresolved);
        };
        let UnresolvedReason::AmbiguousCapture {
            rule,
            field,
            bindings,
            ..
        } = &left.reason
        else {
            panic!("an ambiguous capture: {:?}", left.reason);
        };
        assert_eq!((rule.as_str(), field.as_str()), ("areas", "area"));
        // The walk stops at the second binding it finds: the capture taking
        // the first segment, then the one taking the second.
        let area = |segment: &str| BTreeMap::from([("area".to_string(), segment.to_string())]);
        assert_eq!(bindings, &[area("a"), area("b")]);
    }

    /// **Every default of a plan fills from one clock reading**: two
    /// documents created at paths in one plan take one `created`, read once,
    /// and a plan whose creations take no default reads no clock.
    #[test]
    fn every_default_of_a_plan_fills_from_one_clock_reading() {
        let reads = Cell::new(0);
        let resolution = planned_reading(
            &MemoryVault::default(),
            &schema(LAYERED),
            vec![
                Operation::new(create("a.md", "---\nkind: task\n---\n")),
                Operation::new(create("b.md", "---\nkind: task\n---\n")),
                task_with(vec![]),
            ],
            Ok(reading()),
            &reads,
        );
        assert_eq!(reads.get(), 1);
        for kind in kinds(&resolution) {
            let OperationKind::CreateDocument { content, .. } = kind else {
                panic!("a create: {kind:?}");
            };
            assert!(
                content.contains("created: 2026-10-01T09:30:15+02:00\n"),
                "{content}"
            );
        }
        let reads = Cell::new(0);
        planned_reading(
            &MemoryVault::default(),
            &schema(LAYERED),
            vec![Operation::new(create(
                "a.md",
                "---\nkind: note\narea: home\n---\n",
            ))],
            Ok(reading()),
            &reads,
        );
        assert_eq!(
            reads.get(),
            0,
            "a creation proposing no default that reads the clock reads none"
        );
    }

    /// **A creation reads the clock only for a default reading it that is
    /// proposed or compared**: under an unreadable clock, a document created
    /// at a path whose defaults propose nothing, or only values reading no
    /// clock token, resolves without reading it; one a `{{now}}` default is
    /// proposed for is left unresolved naming the clock, read once.
    #[test]
    fn a_bare_create_proposing_no_clock_default_never_reads_the_clock() {
        let stamped = schema(
            b"version: 1
rules:
  every: { required: { kind: { default: note } } }
  logs: { match: { frontmatter: { kind: log } }, required: { created: { default: '{{now}}' } } }
",
        );
        for (sent, written) in [
            ("---\nkind: mine\n---\nB\n", "---\nkind: mine\n---\nB\n"),
            ("B\n", "---\nkind: note\n---\nB\n"),
        ] {
            let reads = Cell::new(0);
            let resolution = planned_reading(
                &MemoryVault::default(),
                &stamped,
                vec![Operation::new(create("x.md", sent))],
                Err(NotALocalTimestamp),
                &reads,
            );
            assert_eq!(the_create(&resolution).1, written, "{sent:?}");
            assert_eq!(reads.get(), 0, "{sent:?}");
        }
        let reads = Cell::new(0);
        let resolution = planned_reading(
            &MemoryVault::default(),
            &stamped,
            vec![Operation::new(create("x.md", "---\nkind: log\n---\nB\n"))],
            Err(NotALocalTimestamp),
            &reads,
        );
        let [left] = resolution.unresolved.as_slice() else {
            panic!("one operation left: {:?}", resolution.unresolved);
        };
        assert!(
            format!("{:?}", left.reason).contains("clock"),
            "{:?}",
            left.reason
        );
        assert_eq!(reads.get(), 1);
    }
}
