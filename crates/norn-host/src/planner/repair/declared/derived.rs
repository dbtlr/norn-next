//! Derived fixes: the answers a built-in rule proves unique from the
//! document and the declaration alone, with no rule of the schema writing
//! them.
//!
//! **A derived fix is made at [`Confidence::Derived`], below any declared
//! one.** It is composed and judged as a declared fix is (`Document::admit`),
//! so a fix the applier would refuse, or that brings required fields in, is
//! skipped with the reason the judge gives. A fix whose level the request's
//! threshold does not admit is judged too, then skipped as
//! [`SkipReason::BelowThreshold`], carrying the operation lowering the
//! threshold writes for it: its document composed again at the derived
//! level, where the batch's routes leave it (`Reckoned::propose_lowered`),
//! and a fix that composition refuses skips with its reason.
//!
//! - **Wrap and unwrap** answer a `field/shape-mismatch`. A field declared a
//!   list holding a scalar is set to the one-item list of that scalar, its
//!   YAML type kept; a field declared single holding a one-item list is set
//!   to that item. A list of any other length under single has no derived
//!   fix. **A reshape the judge refuses follows one composition table**
//!   ([`reshape`]): the element fixes it exposes always compose with it as
//!   one addition, judged afresh whatever refused the plain reshape.
//!   Admitted, the composed value is the fix at the weakest confidence of
//!   its parts, with their notes. Refused, or not buildable because an
//!   exposed element's proposal skips, the first reason that applies is the
//!   shape finding's: `brings_in_required_fields`, then
//!   `judge_would_refuse` for any violation beyond the exposed elements'
//!   findings, then the exposed element's own skip with its reason and
//!   decision data — read from the plain reshape where nothing composed.
//!   Any exposed link type mismatch waits for the settled routes to decide
//!   it.
//! - **Unquoted wikilinks** on link fields answer exactly `[["s"]]`, YAML's
//!   reading of `[[s]]`, where the text layer reads one whole wikilink.
//!   Obsidian's frontmatter link is a quoted string: a non-list field's whole
//!   value of that shape becomes `"[[s]]"`, as does each element of that
//!   shape on a list field, in one write. A one-string list `["s"]`, such as
//!   the element of `ups: [[s]]`, and every other nested shape have no
//!   unquoted-wikilink fix.
//!   Resolution does not decide this answer; the written link's resolution
//!   is recorded as every repair-written link's is.
//! - **Boolean case** answers a `field/type-mismatch` on a `boolean` field: a
//!   string whose trimmed text lower-cases to exactly `true` or `false` is
//!   written as the YAML boolean, element by element for a list.
//! - **Case and white space** answer a `field/not-one-of` on a field whose
//!   values compare as text ([`Folds`]), every element the finding names a
//!   string — a YAML boolean or number is never re-typed. The one member of
//!   the combined closed set that folds equal to the value — Unicode lower
//!   case, the ends trimmed and each run of white space inside one space — is
//!   written as the schema writes it. Several members folding equal are a
//!   tie. A word join or split (`inprogress` for `in progress`) is not a
//!   fold. A fold never re-cases a wikilink, with no guard needed: a closed
//!   set holds no wikilink (the schema refuses one at load), and the fold
//!   keeps whether a text holds one (see [`fold`]), so a value that folds
//!   equal to a member holds none.
//!
//! **A type mismatch no derived fix answers is an excluded class** where the
//! field is declared `date`, `number` or `boolean`: a fix would reformat a
//! date or coerce a number or a yes/no spelling, which a repair never makes
//! ([`excluded`]). On a `link` field a bare value is made a link where it
//! resolves to one document ([`super::bare_link`]), and an unquoted wikilink
//! is written as the link it spells; any other is left with no declared fix.

use std::collections::{BTreeSet, HashMap};

use norn_config::schema::{FieldType, Rule, Shape, VaultSchema};
use norn_wire::{
    AuthoredValue, Confidence, FindingKind, FindingRow, SkipReason, SkippedCandidates,
    SkippedFinding, ValueCandidate,
};

use super::elements::{self, Offending, Proposed, Replacement};
use super::{Document, Fix, Fixing, State, cited, field_of, head, skip, value_candidates};
use crate::applier::Held;
use crate::planner::edit::edited;

/// The value a field declared `shape` holds once re-shaped, or `None` where
/// no re-shape is proved unique: a scalar under a list is the one item of a
/// list, and a list of one item under single is that item.
pub(super) fn reshaped(shape: Shape, value: &AuthoredValue) -> Option<AuthoredValue> {
    match (shape, value) {
        (Shape::List, scalar) if scalar.scalar_text().is_some() => {
            Some(AuthoredValue::List(vec![scalar.clone()]))
        }
        (Shape::Single, AuthoredValue::List(items)) => match items.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The candidates a derived fix skipped by the judge carries: the one value
/// it would have written, proposed by no named rule.
pub(super) fn candidate_of(value: &AuthoredValue) -> SkippedCandidates {
    value_candidates(std::iter::once(ValueCandidate::new(head(value))))
}

/// The wikilink spelled by exactly `[["s"]]`, YAML's reading of `[[s]]`,
/// where the text layer reads one wikilink spanning that whole spelling.
pub(super) fn unquoted_link(value: &AuthoredValue) -> Option<AuthoredValue> {
    let AuthoredValue::List(items) = value else {
        return None;
    };
    let [AuthoredValue::List(inner)] = items.as_slice() else {
        return None;
    };
    let [AuthoredValue::String(text)] = inner.as_slice() else {
        return None;
    };
    let written = format!("[[{text}]]");
    let links = norn_text::parse_wikilinks_in_text(&written);
    matches!(links.as_slice(), [only] if only.raw == written)
        .then_some(AuthoredValue::string(written))
}

/// The link spelled by a non-list link field's whole value `[["s"]]`.
pub(super) fn unquoted_field(
    schema: &VaultSchema,
    field: &str,
    value: &AuthoredValue,
) -> Option<AuthoredValue> {
    if schema.declared_type(field) != FieldType::Link
        || schema.field(field).and_then(|f| f.shape()) == Some(Shape::List)
    {
        return None;
    }
    unquoted_link(value)
}

/// A derived value written as one addition, judged before its threshold is read.
pub(super) fn fix_value(
    document: &Document<'_>,
    state: &State,
    row: &FindingRow,
    finding: &Held,
    field: &str,
    replacement: Replacement,
    vetoed: &dyn Fn(&State) -> Option<String>,
) -> Result<Fix, Box<SkippedFinding>> {
    let set = document.set(&state.at, field, replacement.member);
    let composed = document.admit(
        state,
        std::slice::from_ref(&set),
        Fixing {
            row,
            finding,
            candidates: &replacement.candidates,
        },
    )?;
    if let Some(note) = vetoed(&composed) {
        return Err(Box::new(
            skip(row, SkipReason::JudgeWouldRefuse)
                .with_candidates(replacement.candidates)
                .with_note(note),
        ));
    }
    document.within_threshold(replacement.level, row, &set)?;
    Ok(Fix {
        removed_field: None,
        operations: vec![set],
        cited: vec![cited(row, replacement.level, replacement.notes)],
        state: composed,
    })
}

/// A derived replacement of `member`, proposed by no named rule.
pub(super) fn replacement(member: AuthoredValue) -> Replacement {
    Replacement {
        candidates: candidate_of(&member),
        member,
        level: Confidence::Derived,
        notes: Vec::new(),
    }
}

/// What a shape fix reaches now, or leaves for the settled routes to answer.
pub(super) enum Reshaping {
    Fixed(Fix),
    NeedsLinks,
}

/// The shape fix of `row`: the plain reshape where the judge admits it, and
/// otherwise the composition table's answer — the reshape composed with
/// every exposed element fix and judged afresh, or, where that cannot be
/// built, the plain refusal for a reshape introducing anything beyond its
/// exposed elements' findings and the exposed element's own skip for one
/// introducing nothing else. A bare link answer waits for `links`.
pub(super) fn reshape(
    document: &Document<'_>,
    state: &State,
    row: &FindingRow,
    finding: &Held,
    field: &str,
    links: Option<&mut dyn FnMut(&Offending<'_>) -> Proposed>,
    vetoed: &dyn Fn(&State) -> Option<String>,
) -> Result<Reshaping, Box<SkippedFinding>> {
    let Some(shape) = document.schema().field(field).and_then(|f| f.shape()) else {
        return Err(Box::new(crate::planner::repair::skipped(row)));
    };
    let Some(Some(value)) = field_of(&state.bytes, field) else {
        return Err(Box::new(skip(row, SkipReason::JudgeWouldRefuse).with_note(
            format!("the document's `{field}` cannot be read to re-shape"),
        )));
    };
    if let Some(link) = unquoted_field(document.schema(), field, &value) {
        return fix_value(
            document,
            state,
            row,
            finding,
            field,
            replacement(link),
            vetoed,
        )
        .map(Reshaping::Fixed);
    }
    let Some(reshaped) = reshaped(shape, &value) else {
        return Err(Box::new(crate::planner::repair::skipped(row)));
    };
    let set = document.set(&state.at, field, reshaped.clone());
    let candidates = candidate_of(&reshaped);
    let bytes = edited(&set, &state.bytes).map_err(|detail| {
        Box::new(
            skip(row, SkipReason::JudgeWouldRefuse)
                .with_candidates(candidates.clone())
                .with_note(format!("the fix cannot be set into the document: {detail}")),
        )
    })?;
    let (exposed, introduced) = document.assessed(state, &state.at, bytes);
    let refused = match document.admitted(
        exposed.clone(),
        &introduced,
        Fixing {
            row,
            finding,
            candidates: &candidates,
        },
        "the fix leaves it still standing",
    ) {
        Ok(composed) => {
            if let Some(note) = vetoed(&composed) {
                return Err(Box::new(
                    skip(row, SkipReason::JudgeWouldRefuse)
                        .with_candidates(candidates)
                        .with_note(note),
                ));
            }
            document.within_threshold(Confidence::Derived, row, &set)?;
            return Ok(Reshaping::Fixed(Fix {
                removed_field: None,
                operations: vec![set],
                cited: vec![cited(row, Confidence::Derived, Vec::new())],
                state: composed,
            }));
        }
        Err(skip) => skip,
    };
    // The composed value is built and judged afresh whatever refused the
    // plain reshape. Where it cannot be built, the plain refusal stands for a
    // reshape introducing anything beyond its exposed elements' findings —
    // `brings_in_required_fields` before `judge_would_refuse`, as the judge
    // orders them — and otherwise the exposed element's own skip is the
    // shape finding's.
    let beyond_elements = introduced.iter().any(|violation| {
        !(violation.target.as_deref() == Some(field) && elements::is_exposed(violation.kind))
    });
    match elements::after_reshape(document, &exposed, row, field, &reshaped, links) {
        elements::Reshaped::Unanswered => Err(refused),
        elements::Reshaped::Skipped(_) if beyond_elements => Err(refused),
        elements::Reshaped::Skipped(skip) => Err(skip),
        elements::Reshaped::NeedsLinks => Ok(Reshaping::NeedsLinks),
        elements::Reshaped::Replaced(replacement) => {
            fix_value(document, state, row, finding, field, replacement, vetoed)
                .map(Reshaping::Fixed)
        }
    }
}

/// The boolean a string spells but for its case and padding: the string
/// `text` trimmed and lower-cased is exactly `true` or `false`.
fn boolean_of(text: &str) -> Option<bool> {
    match text.trim().to_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// What the boolean field `field` makes of the type-mismatched element
/// `offending`: the YAML boolean its text spells but for case and padding, or
/// an excluded class where no case fix answers it.
///
/// Every element such a finding names is a string: a boolean field reads a
/// string spelling exactly `true` or `false` as the boolean, so no finding
/// names one beside a YAML boolean, and no number or null spells a boolean
/// but for its case.
pub(super) fn boolean_case(document: &Document<'_>, offending: &Offending<'_>) -> Proposed {
    match boolean_of(offending.text) {
        Some(flag) => {
            let member = AuthoredValue::Bool(flag);
            Proposed::Replace(Replacement {
                candidates: candidate_of(&member),
                member,
                level: Confidence::Derived,
                notes: Vec::new(),
            })
        }
        None => Proposed::Skipped(Box::new(
            excluded(offending.row, document.schema())
                .unwrap_or_else(|| crate::planner::repair::skipped(offending.row)),
        )),
    }
}

/// Whether a `field/type-mismatch` on `field` is answered by a derived fix
/// where the element is a string spelling a boolean: the field is declared
/// `boolean`.
pub(super) fn is_boolean(schema: &VaultSchema, field: &str) -> bool {
    schema.declared_type(field) == FieldType::Boolean
}

/// The skip of the `field/type-mismatch` finding `row` as an excluded class,
/// its note naming the class, where its field is declared `date`, `number` or
/// `boolean`: the fix would be date reformatting or a coercion repair never
/// makes. `None` for any other finding.
pub(super) fn excluded(row: &FindingRow, schema: &VaultSchema) -> Option<SkippedFinding> {
    if row.kind != FindingKind::TypeMismatch {
        return None;
    }
    let field = row.target.as_deref()?;
    let (declared, class) = match schema.declared_type(field) {
        FieldType::Date => ("date", "date reformatting"),
        FieldType::Number => ("number", "number coercion"),
        FieldType::Boolean => ("boolean", "yes/no coercion"),
        FieldType::Text | FieldType::Tags | FieldType::Link => return None,
    };
    Some(skip(row, SkipReason::ExcludedClass).with_note(format!(
        "`{field}` is declared `{declared}`, and a fix would be {class}, a class of fix a \
         repair never makes"
    )))
}

/// What a value folds to when it is compared as text: lower-cased by Unicode,
/// the ends trimmed and each run of white space inside one space.
///
/// **The fold keeps whether a text holds a wikilink.** A wikilink is `[[`, one
/// or more characters none of which is `]`, then `]]`. The fold never adds,
/// moves or removes a `[` or a `]`; it maps each other character to
/// characters that are not `]`, and a white space run to a space, so the
/// characters between the brackets of a link stay at least one and stay free of
/// `]`, and the characters of a fold that read as a link come from the
/// characters that read as one. Two texts folding equal therefore both hold a
/// wikilink or neither does, and since a closed set holds none, a value that
/// folds equal to a member holds none.
fn fold(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the fold of an offending value finds among the combined closed set.
pub(super) enum Folded {
    /// No member folds equal.
    None,
    /// Exactly one member folds equal: the proof of a derived fix.
    One(String),
    /// Several members fold equal, in the set's order.
    Many(Vec<String>),
}

/// The members of the combined closed set of one field by what they fold to:
/// built once for the rules that close the field, and asked once per
/// offending element, so the cost follows the members plus the elements, not
/// their product.
pub(super) struct Folds {
    rules: BTreeSet<String>,
    members: HashMap<String, Vec<String>>,
}

impl Folds {
    /// The folds of the combined closed set of `field` under `rules`, which
    /// are empty where the field's values do not compare as text or no rule
    /// closes it.
    fn of(schema: &VaultSchema, field: &str, rules: &BTreeSet<String>) -> Self {
        let mut members: HashMap<String, Vec<String>> = HashMap::new();
        if schema.compares_as_text(field) {
            let closing: Vec<&Rule> = rules.iter().filter_map(|name| schema.rule(name)).collect();
            let combined = schema.combined(&closing);
            let set = combined
                .field(field)
                .and_then(|constraint| constraint.one_of());
            for member in set.into_iter().flat_map(|set| set.members()) {
                members
                    .entry(fold(member))
                    .or_default()
                    .push(member.to_string());
            }
        }
        Folds {
            rules: rules.clone(),
            members,
        }
    }

    /// The members `offending` folds equal to.
    pub(super) fn answer(&self, offending: &str) -> Folded {
        match self.members.get(&fold(offending)).map(Vec::as_slice) {
            None | Some([]) => Folded::None,
            Some([only]) => Folded::One(only.clone()),
            Some(several) => Folded::Many(several.to_vec()),
        }
    }
}

/// The folds of one field's closed set, rebuilt only where the rules closing
/// the field change between its elements.
#[derive(Default)]
pub(super) struct FoldCache(Option<Folds>);

impl FoldCache {
    /// The folds of `field` under `rules`.
    pub(super) fn of(
        &mut self,
        schema: &VaultSchema,
        field: &str,
        rules: &BTreeSet<String>,
    ) -> &Folds {
        if self.0.as_ref().is_none_or(|folds| &folds.rules != rules) {
            self.0 = Some(Folds::of(schema, field, rules));
        }
        self.0.as_ref().expect("the folds were just built")
    }
}

#[cfg(test)]
mod tests {
    use norn_wire::{
        AuthoredValue, Confidence, FindingKind, OperationKind, SkipReason, SkippedCandidates,
        SkippedFinding,
    };

    use super::fold;
    use crate::planner::repair::declared::bare_link::tests::{finding, planned_at};

    /// Whether the text layer reads a wikilink anywhere in `text`.
    fn holds_wikilink(text: &str) -> bool {
        !norn_text::parse_wikilinks_in_text(text).is_empty()
    }

    /// **The fold keeps whether a text holds a wikilink**, so a value
    /// folding equal to a member of a closed set, which holds none, holds
    /// none: the fold never re-cases a link, and needs no guard to say so.
    /// The cases are the ones a fold could disturb: white space inside and
    /// around a link, a line break between its brackets, upper case and an
    /// expanding lower case in its target, an embed, and brackets that read
    /// as no link.
    #[test]
    fn the_fold_keeps_whether_a_text_holds_a_wikilink() {
        let texts = [
            "[[a]]",
            "[[A]]",
            " [[ A  B ]] ",
            "[[a\nb]]",
            "[[a\u{a0}\u{2003}b]]",
            "[[İ]]",
            "![[A]]",
            "see\t[[A|X]]\n",
            "[[",
            "[[]]",
            "[[ ]]",
            "[ [a]]",
            "[[a] ]",
            "a]]b",
            "[x]",
            "[[a]",
            "[[a]]]",
            "[[[a]]",
        ];
        for text in texts {
            assert_eq!(
                holds_wikilink(&fold(text)),
                holds_wikilink(text),
                "{text:?} folds to {:?}",
                fold(text)
            );
        }
    }

    /// What the composition table expects of one wrap or unwrap.
    enum Outcome {
        /// The field is set to the value, the shape finding cited at the level.
        Fixed(AuthoredValue, Confidence),
        /// The shape finding is skipped for the reason, carrying the data.
        Skipped(SkipReason, Data),
    }

    /// The decision data a cell's skip carries.
    enum Data {
        /// The operation lowering the threshold would write, setting the field
        /// to the value.
        Proposed(AuthoredValue),
        /// The one value the refused change would have written.
        Candidate(AuthoredValue),
        /// The required fields the change would bring in, by name.
        RequiredFields(&'static [&'static str]),
        /// The tied members, in the order the skip carries them.
        Values(&'static [&'static str]),
        /// The documents an ambiguous link names, in any order.
        Documents(&'static [&'static str]),
        /// Words the note holds.
        Note(&'static str),
        /// Nothing beyond the reason.
        Nothing,
    }

    /// One cell of the composition table: the shape finding `1` of `a.md`'s
    /// `field`, judging `value`, planned at `threshold`.
    struct Cell {
        name: &'static str,
        schema: &'static str,
        documents: &'static [(&'static str, &'static str)],
        field: &'static str,
        value: &'static str,
        threshold: Confidence,
        outcome: Outcome,
    }

    /// What `skipped` departs from in `data`, if anything.
    fn departs(skipped: &SkippedFinding, data: &Data) -> Option<String> {
        let held = match data {
            Data::Proposed(value) => skipped.proposed.as_ref().is_some_and(|operation| {
                matches!(&operation.kind, OperationKind::SetFrontmatter { value: set, .. } if set == value)
            }),
            Data::Candidate(value) => {
                skipped.candidates.as_ref() == Some(&super::candidate_of(value))
            }
            Data::RequiredFields(names) => skipped.required_fields.as_ref().is_some_and(|head| {
                head.fields()
                    .iter()
                    .map(|field| field.field.as_str())
                    .eq(names.iter().copied())
            }),
            Data::Values(members) => match &skipped.candidates {
                Some(SkippedCandidates::Values { head, .. }) => head
                    .candidates()
                    .iter()
                    .map(|candidate| candidate.value.text())
                    .eq(members.iter().copied()),
                _ => false,
            },
            Data::Documents(paths) => match &skipped.candidates {
                Some(SkippedCandidates::Documents { head, .. }) => {
                    let mut named: Vec<&str> = head
                        .candidates()
                        .iter()
                        .map(|candidate| candidate.path.as_str())
                        .collect();
                    named.sort_unstable();
                    named == *paths
                }
                _ => false,
            },
            Data::Note(words) => skipped.note.as_deref().is_some_and(|note| note.contains(words)),
            Data::Nothing => {
                skipped.candidates.is_none()
                    && skipped.required_fields.is_none()
                    && skipped.proposed.is_none()
            }
        };
        (!held).then(|| format!("{skipped:?}"))
    }

    /// What planning `cell` departs from its outcome in, if anything.
    fn departure(cell: &Cell) -> Option<String> {
        let planned = planned_at(
            cell.schema,
            cell.documents,
            &[finding(
                1,
                FindingKind::ShapeMismatch,
                "a.md",
                cell.field,
                cell.value,
            )],
            cell.threshold,
        );
        match (&cell.outcome, planned.skipped.as_slice()) {
            (Outcome::Fixed(value, level), []) => {
                let set = planned
                    .operations
                    .iter()
                    .map(|operation| &operation.kind)
                    .collect::<Vec<_>>();
                let fixed = matches!(set.as_slice(), [OperationKind::SetFrontmatter { field, value: written, .. }] if field == cell.field && written == value);
                let cited = planned
                    .citations
                    .iter()
                    .flat_map(|citation| &citation.findings)
                    .map(|cited| (cited.finding, cited.confidence))
                    .collect::<Vec<_>>();
                (!fixed || cited != [(1, *level)])
                    .then(|| format!("{:?} citing {cited:?}", planned.operations))
            }
            (Outcome::Skipped(reason, data), [skipped]) if skipped.reason == *reason => {
                if planned.operations.is_empty() {
                    departs(skipped, data)
                } else {
                    Some(format!("{:?} beside the skip", planned.operations))
                }
            }
            _ => Some(format!(
                "{:?} skipping {:?}",
                planned.operations, planned.skipped
            )),
        }
    }

    const BOOLEAN_SINGLE: &str = "version: 1\nfields:\n  flag: {type: boolean, shape: single}\n";
    const BOOLEAN_LIST: &str = "version: 1\nfields:\n  flags: {type: boolean, shape: list}\n";
    const SYNONYM: &str = "version: 1\nfields:\n  status: {type: text, shape: list}\nrules:\n  tasks:\n    match: {path: '**'}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n";
    const LINKS: &str = "version: 1\nfields:\n  ups: {type: link, shape: list}\n";
    /// `status` closed to `todo` and `done`, `complete` a synonym of `done`,
    /// and at most four characters long, declared a list or single.
    const LIMITED_LIST: &str = "version: 1\nfields:\n  status: {type: text, shape: list}\nrules:\n  tasks:\n    match: {path: '**'}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n    max_length:\n      status: 4\n";
    const LIMITED_SINGLE: &str = "version: 1\nfields:\n  status: {type: text, shape: single}\nrules:\n  tasks:\n    match: {path: '**'}\n    one_of:\n      status: {values: [todo, done], synonyms: {complete: done}}\n    max_length:\n      status: 4\n";

    /// The rule closing `kind` to `done` and `todo`, `complete` a synonym of `done`.
    macro_rules! kinds {
        () => {
            "  kinds:\n    match: {path: '**'}\n    one_of:\n      kind: {values: [done, todo], synonyms: {complete: done}}\n"
        };
    }

    /// The rule closing `kind` to `Task` and `TASK`, which `task` folds equal to both.
    macro_rules! tied {
        () => {
            "  kinds:\n    match: {path: '**'}\n    one_of:\n      kind: {values: [Task, TASK]}\n"
        };
    }

    /// The composition table for a wrap or unwrap the judge refuses, each
    /// cell by its name: the composed value is built with the exposed
    /// elements' fixes and judged afresh whatever refused the plain reshape;
    /// admitted, it is the fix at the weaker level; refused or not buildable,
    /// the first reason that applies is reported — `brings_in_required_fields`,
    /// then `judge_would_refuse` for any other violation, then the exposed
    /// element's own skip with its decision data.
    fn table() -> Vec<Cell> {
        let done = || AuthoredValue::List(vec![AuthoredValue::string("done")]);
        vec![
            Cell {
                name: "an unwrap composes with the boolean case it exposes",
                schema: BOOLEAN_SINGLE,
                documents: &[("a.md", "---\nflag: ['True']\n---\n")],
                field: "flag",
                value: "[\"True\"]",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Fixed(AuthoredValue::Bool(true), Confidence::Derived),
            },
            Cell {
                name: "a wrap composes with the boolean case it exposes",
                schema: BOOLEAN_LIST,
                documents: &[("a.md", "---\nflags: 'True'\n---\n")],
                field: "flags",
                value: "True",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Fixed(
                    AuthoredValue::List(vec![AuthoredValue::Bool(true)]),
                    Confidence::Derived,
                ),
            },
            Cell {
                name: "a wrap composed with a declared synonym is the weaker, derived level",
                schema: SYNONYM,
                documents: &[("a.md", "---\nstatus: complete\n---\n")],
                field: "status",
                value: "complete",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Fixed(done(), Confidence::Derived),
            },
            Cell {
                name: "a wrap composes with the bare link it exposes",
                schema: LINKS,
                documents: &[("a.md", "---\nups: b\n---\n"), ("b.md", "# B\n")],
                field: "ups",
                value: "b",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Fixed(
                    AuthoredValue::List(vec![AuthoredValue::string("[[b]]")]),
                    Confidence::Derived,
                ),
            },
            Cell {
                name: "a plain reshape bringing in required fields composes where the composed value brings in none",
                schema: concat!(
                    "version: 1\nfields:\n  kind: {type: text, shape: list}\nrules:\n  legacy:\n    match: {frontmatter: {kind: complete}}\n    required:\n      status: {default: todo}\n",
                    kinds!()
                ),
                documents: &[("a.md", "---\nkind: complete\n---\n")],
                field: "kind",
                value: "complete",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Fixed(done(), Confidence::Derived),
            },
            Cell {
                name: "a plain reshape introducing another violation composes where the composed value introduces none",
                schema: concat!(
                    "version: 1\nfields:\n  kind: {type: text, shape: list}\nrules:\n  legacy:\n    match: {frontmatter: {kind: complete}}\n    forbidden:\n      draft:\n",
                    kinds!()
                ),
                documents: &[("a.md", "---\nkind: complete\ndraft: x\n---\n")],
                field: "kind",
                value: "complete",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Fixed(done(), Confidence::Derived),
            },
            Cell {
                name: "a composed fix below the threshold proposes the combined operation",
                schema: SYNONYM,
                documents: &[("a.md", "---\nstatus: complete\n---\n")],
                field: "status",
                value: "complete",
                threshold: Confidence::Declared,
                outcome: Outcome::Skipped(SkipReason::BelowThreshold, Data::Proposed(done())),
            },
            Cell {
                name: "a composed value bringing in required fields reports its own",
                schema: concat!(
                    "version: 1\nfields:\n  kind: {type: text, shape: list}\nrules:\n  finished:\n    match: {frontmatter: {kind: done}}\n    required:\n      status: {default: todo}\n",
                    kinds!()
                ),
                documents: &[("a.md", "---\nkind: complete\n---\n")],
                field: "kind",
                value: "complete",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(
                    SkipReason::BringsInRequiredFields,
                    Data::RequiredFields(&["status"]),
                ),
            },
            Cell {
                name: "a composed value bringing in required fields and another violation brings them in first",
                schema: concat!(
                    "version: 1\nfields:\n  kind: {type: text, shape: list}\nrules:\n  finished:\n    match: {frontmatter: {kind: done}}\n    required:\n      status: {default: todo}\n    forbidden:\n      draft:\n",
                    kinds!()
                ),
                documents: &[("a.md", "---\nkind: complete\ndraft: x\n---\n")],
                field: "kind",
                value: "complete",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(
                    SkipReason::BringsInRequiredFields,
                    Data::RequiredFields(&["status"]),
                ),
            },
            Cell {
                name: "an unbuildable reshape bringing in required fields keeps them over its element's tie",
                schema: concat!(
                    "version: 1\nfields:\n  kind: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {kind: task}}\n    required:\n      status: {default: todo}\n",
                    tied!()
                ),
                documents: &[("a.md", "---\nkind: task\n---\n")],
                field: "kind",
                value: "task",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(
                    SkipReason::BringsInRequiredFields,
                    Data::RequiredFields(&["status"]),
                ),
            },
            Cell {
                name: "an unbuildable reshape bringing in required fields and another violation brings them in first",
                schema: concat!(
                    "version: 1\nfields:\n  kind: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {kind: task}}\n    required:\n      status: {default: todo}\n    forbidden:\n      draft:\n",
                    tied!()
                ),
                documents: &[("a.md", "---\nkind: task\ndraft: x\n---\n")],
                field: "kind",
                value: "task",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(
                    SkipReason::BringsInRequiredFields,
                    Data::RequiredFields(&["status"]),
                ),
            },
            Cell {
                name: "a composed value introducing another violation is refused with the combined candidate",
                schema: "version: 1\nfields:\n  flags: {type: boolean, shape: list}\nrules:\n  flagged:\n    match: {frontmatter: {flags: true}}\n    forbidden:\n      draft:\n",
                documents: &[("a.md", "---\nflags: 'True'\ndraft: x\n---\n")],
                field: "flags",
                value: "True",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(
                    SkipReason::JudgeWouldRefuse,
                    Data::Candidate(AuthoredValue::List(vec![AuthoredValue::Bool(true)])),
                ),
            },
            Cell {
                name: "an unbuildable reshape introducing another violation is refused over its element's tie",
                schema: concat!(
                    "version: 1\nfields:\n  kind: {type: text, shape: list}\nrules:\n  tasks:\n    match: {frontmatter: {kind: task}}\n    forbidden:\n      draft:\n",
                    tied!()
                ),
                documents: &[("a.md", "---\nkind: task\ndraft: x\n---\n")],
                field: "kind",
                value: "task",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(
                    SkipReason::JudgeWouldRefuse,
                    Data::Candidate(AuthoredValue::List(vec![AuthoredValue::string("task")])),
                ),
            },
            Cell {
                name: "an exposed closed-set tie is the shape finding's, with its candidates",
                schema: "version: 1\nfields:\n  status: {type: text, shape: list}\nrules:\n  tasks:\n    match: {path: '**'}\n    one_of:\n      status: {values: [Todo, TODO, done]}\n",
                documents: &[("a.md", "---\nstatus: todo\n---\n")],
                field: "status",
                value: "todo",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(SkipReason::Tie, Data::Values(&["TODO", "Todo"])),
            },
            Cell {
                name: "an exposed ambiguous link is the shape finding's, with its documents",
                schema: LINKS,
                documents: &[
                    ("a.md", "---\nups: b\n---\n"),
                    ("x/b.md", "# X\n"),
                    ("y/b.md", "# Y\n"),
                ],
                field: "ups",
                value: "b",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(
                    SkipReason::AmbiguousLink,
                    Data::Documents(&["x/b.md", "y/b.md"]),
                ),
            },
            Cell {
                name: "an exposed link naming no document is the shape finding's no_declared_fix, with its note",
                schema: LINKS,
                documents: &[("a.md", "---\nups: ghost\n---\n")],
                field: "ups",
                value: "ghost",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(
                    SkipReason::NoDeclaredFix,
                    Data::Note("`[[ghost]]` would resolve to no document"),
                ),
            },
            Cell {
                name: "an exposed closed-set value with no fix is the shape finding's no_declared_fix",
                schema: "version: 1\nfields:\n  tags_of: {type: text, shape: list}\nrules:\n  tasks:\n    match: {path: '**'}\n    one_of:\n      tags_of: {values: [red, blue]}\n",
                documents: &[("a.md", "---\ntags_of: bogus\n---\n")],
                field: "tags_of",
                value: "bogus",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(SkipReason::NoDeclaredFix, Data::Nothing),
            },
            Cell {
                name: "an exposed number coercion is the shape finding's excluded class",
                schema: "version: 1\nfields:\n  priority: {type: number, shape: list}\n",
                documents: &[("a.md", "---\npriority: abc\n---\n")],
                field: "priority",
                value: "abc",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Skipped(SkipReason::ExcludedClass, Data::Note("number coercion")),
            },
            Cell {
                name: "a wrap composes with the synonym answering both findings of its exposed element",
                schema: LIMITED_LIST,
                documents: &[("a.md", "---\nstatus: complete\n---\n")],
                field: "status",
                value: "complete",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Fixed(done(), Confidence::Derived),
            },
            Cell {
                name: "an unwrap composes with the synonym answering both findings of its exposed element",
                schema: LIMITED_SINGLE,
                documents: &[("a.md", "---\nstatus: [complete]\n---\n")],
                field: "status",
                value: "[\"complete\"]",
                threshold: Confidence::DEFAULT_THRESHOLD,
                outcome: Outcome::Fixed(AuthoredValue::string("done"), Confidence::Derived),
            },
        ]
    }

    /// **A wrap or unwrap the judge refuses follows the composition table,
    /// cell by cell**: each cell that departs is named with what was planned.
    #[test]
    fn a_refused_wrap_or_unwrap_follows_the_composition_table() {
        let departed: Vec<String> = table()
            .iter()
            .filter_map(|cell| departure(cell).map(|planned| format!("{}: {planned}", cell.name)))
            .collect();
        assert!(
            departed.is_empty(),
            "cells departing:\n{}",
            departed.join("\n")
        );
    }
}
