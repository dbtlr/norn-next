//! Compute deterministic field and schema observations on a repair's read hold.

use std::collections::{BTreeMap, BTreeSet};

use norn_config::schema::{ForbiddenFix, Rule, RulesConflict, VaultSchema};
use norn_store::{ContentModel, PageRefusal, RepairFieldTally, Snapshot};
use norn_wire::{
    AdvisoryValueType, ContainerKind, HygieneConflict, Predicate, RepairAdvisories, RepairAdvisory,
    SchemaHygiene, SpellingUse,
};

use crate::evidence::RepairAdvisoryCost;

/// Bound the number of advisory items carried by one repair.
const ADVISORY_ITEMS: usize = 64;
/// Bound the serialized JSON bytes of the advisory items, including YAML.
const ADVISORY_BYTES: usize = 32 * 1024;

/// Count every item while retaining the bounded prefix of one catalogue kind.
#[derive(Default)]
struct Collected {
    items: Vec<RepairAdvisory>,
    total: u64,
    bytes: usize,
}

impl Collected {
    /// Whether the next item may extend this kind's bounded prefix.
    fn taking(&self) -> bool {
        self.total == self.items.len() as u64
            && self.items.len() < ADVISORY_ITEMS
            && self.bytes + 2 < ADVISORY_BYTES
    }

    fn add(&mut self, item: RepairAdvisory) {
        self.total += 1;
        if self.total != self.items.len() as u64 + 1 {
            return;
        }
        let next = self.bytes + item_bytes(&item) + usize::from(!self.items.is_empty());
        if self.items.len() < ADVISORY_ITEMS && next + 2 <= ADVISORY_BYTES {
            self.bytes = next;
            self.items.push(item);
        }
    }
}

/// Keep field kinds in catalogue order and spellings for the near-duplicate pass.
#[derive(Default)]
struct FieldKinds {
    types: Collected,
    shapes: Collected,
    uniform: Collected,
    spellings: BTreeMap<String, Vec<SpellingUse>>,
}

/// Compute the catalogue without storing it or proposing an operation.
pub(crate) fn computed(
    snapshot: &Snapshot,
    schema: &VaultSchema,
    declared: &ContentModel,
) -> Result<(RepairAdvisories, RepairAdvisoryCost), PageRefusal> {
    let mut cost = RepairAdvisoryCost {
        passes: 1,
        ..RepairAdvisoryCost::default()
    };
    let fields = field_kinds(snapshot, schema, &mut cost)?;
    let duplicates = near_duplicates(snapshot, schema, fields.spellings, &mut cost)?;
    let (unmatched, unused) = rule_hygiene(snapshot, schema, declared, &mut cost)?;
    let combinations = empty_combinations(snapshot, schema, &mut cost);
    Ok((
        truncated([
            fields.types,
            fields.shapes,
            fields.uniform,
            duplicates,
            unmatched,
            unused,
            combinations,
        ]),
        cost,
    ))
}

/// Read each key once to observe its field kinds and count its spellings.
fn field_kinds(
    snapshot: &Snapshot,
    schema: &VaultSchema,
    cost: &mut RepairAdvisoryCost,
) -> Result<FieldKinds, PageRefusal> {
    let mut fields = FieldKinds::default();
    let mut after: Option<String> = None;
    loop {
        let read = snapshot.repair_key_after(after.as_deref())?;
        cost.absorb(read.work);
        let Some(key) = read.key else {
            break;
        };
        let tally = snapshot.repair_field_tally(&key)?;
        cost.absorb(tally.work);
        cost.keys += 1;
        cost.value_rows += tally.value_rows;
        observe_field(&key, tally, schema, &mut fields);
        after = Some(key);
    }
    Ok(fields)
}

/// Build complete field advisories from the tally's types, shapes and sample.
fn observe_field(
    key: &str,
    tally: RepairFieldTally,
    schema: &VaultSchema,
    fields: &mut FieldKinds,
) {
    if let Some(item) = uniform_declaration(key, &tally, schema) {
        fields.uniform.add(item);
    }
    if tally.types.len() > 1 {
        fields.types.add(RepairAdvisory::mixed_types(
            key,
            tally.types,
            tally.sample.clone(),
        ));
    }
    if tally.shapes.len() > 1 {
        fields.shapes.add(RepairAdvisory::mixed_shapes(
            key,
            tally.shapes,
            tally.sample,
        ));
    }
    fields
        .spellings
        .entry(folded(key))
        .or_default()
        .push(SpellingUse::new(key, tally.documents));
}

/// Declare an undeclared uniform field in its observed type and common shape.
fn uniform_declaration(
    key: &str,
    tally: &RepairFieldTally,
    schema: &VaultSchema,
) -> Option<RepairAdvisory> {
    if schema.field(key).is_some() || tally.types.len() != 1 {
        return None;
    }
    let kind = tally.types[0].value_type;
    let declaration = declaration(kind)?;
    let shape = if tally.shapes.len() == 1 {
        match tally.shapes[0].shape {
            ContainerKind::Scalar => ", shape: single",
            ContainerKind::Sequence => ", shape: list",
            _ => "",
        }
    } else {
        ""
    };
    let yaml = format!(
        "fields:\n  ? {}\n  : {{type: {declaration}{shape}}}\n",
        quoted(key)
    );
    Some(RepairAdvisory::undeclared_uniform(
        key,
        kind,
        tally.documents,
        tally.sample.clone(),
        yaml,
    ))
}

/// Count the exact document union of each near-duplicate group in key order.
fn near_duplicates(
    snapshot: &Snapshot,
    schema: &VaultSchema,
    spellings: BTreeMap<String, Vec<SpellingUse>>,
    cost: &mut RepairAdvisoryCost,
) -> Result<Collected, PageRefusal> {
    let mut groups: Vec<_> = spellings
        .into_values()
        .filter(|group| group.len() > 1)
        .collect();
    groups.sort_by(|left, right| left[0].key.cmp(&right[0].key));
    let rule_keys = rule_read_keys(schema);
    let mut duplicates = Collected::default();
    let mut next_rule = 1;
    for group in groups {
        if !duplicates.taking() {
            duplicates.total += 1;
            continue;
        }
        let keys: Vec<_> = group.iter().map(|spelling| spelling.key.clone()).collect();
        let tally = snapshot.repair_spelling_tally(&keys)?;
        cost.absorb(tally.work);
        let yaml = rename_rules(&group, schema, &rule_keys, &mut next_rule);
        duplicates.add(RepairAdvisory::near_duplicate_fields(
            group,
            tally.sample,
            yaml,
        ));
    }
    Ok(duplicates)
}

/// The spellings the config names for use, and those its rules forbid or fix,
/// collected once per pass.
///
/// A named spelling is never renamed away or forbidden by the advisory yaml. A
/// forbidden key is never a rename target and does not count as named for target
/// choice. One the schema forbids with its own fix (`remove` or `rename_to`) is
/// left to that fix: a second, different fix on it ties and repair skips both. One
/// forbidden with no fix and not named is renamed like an unnamed spelling, since
/// the rename is then the only fix repair has for it.
#[derive(Default)]
struct RuleSpellings<'s> {
    /// Rule keys, rename targets, creation-default keys and the tag carrier.
    named: BTreeSet<String>,
    /// Keys some rule forbids, whatever its fix.
    forbidden: BTreeSet<&'s str>,
    /// Keys some rule forbids with a declared fix.
    fixed: BTreeSet<&'s str>,
}

/// Collect the spellings the config names for use and those its rules forbid or fix.
fn rule_read_keys(schema: &VaultSchema) -> RuleSpellings<'_> {
    let mut keys = RuleSpellings::default();
    for rule in schema.rules() {
        keys.named.extend(
            rule.selector()
                .frontmatter()
                .map(|(key, _)| key.to_string()),
        );
        keys.named
            .extend(rule.required().map(|(key, _)| key.to_string()));
        for (key, fix) in rule.forbidden() {
            keys.forbidden.insert(key);
            match fix {
                ForbiddenFix::Unfixed => {}
                ForbiddenFix::Remove => {
                    keys.fixed.insert(key);
                }
                ForbiddenFix::RenameTo(target) => {
                    keys.fixed.insert(key);
                    keys.named.insert(target.to_string());
                }
            }
        }
        keys.named
            .extend(rule.one_of().map(|(key, _)| key.to_string()));
        keys.named
            .extend(rule.max_length().map(|(key, _)| key.to_string()));
    }
    for rule in schema.creation_rules() {
        keys.named.extend(
            rule.frontmatter_defaults()
                .into_entries()
                .into_iter()
                .map(|(key, _)| key),
        );
    }
    keys.named.insert(norn_store::TAG_CARRIER.to_string());
    keys
}

/// Render selectorless renames without forbidding a schema-named spelling or
/// renaming onto a forbidden one.
fn rename_rules(
    group: &[SpellingUse],
    schema: &VaultSchema,
    rule_keys: &RuleSpellings<'_>,
    next_rule: &mut u64,
) -> String {
    let (mut named, other): (Vec<_>, Vec<_>) = group.iter().partition(|spelling| {
        schema.field(&spelling.key).is_some() || rule_keys.named.contains(spelling.key.as_str())
    });
    let target = |spelling: &&SpellingUse| !rule_keys.forbidden.contains(spelling.key.as_str());
    named.retain(target);
    let winner = match named.as_slice() {
        [only] => Some(*only),
        _ => other.iter().copied().filter(target).max_by(|left, right| {
            left.documents
                .cmp(&right.documents)
                .then_with(|| right.key.cmp(&left.key))
        }),
    };
    let mut renames = String::new();
    if let Some(winner) = winner {
        for spelling in other.iter().filter(|spelling| {
            spelling.key != winner.key && !rule_keys.fixed.contains(spelling.key.as_str())
        }) {
            let rule = next_rule_name(schema, next_rule);
            renames.push_str(&format!(
                "  {rule}:\n    forbidden:\n      ? {}\n      : {{rename_to: {}}}\n",
                quoted(&spelling.key),
                quoted(&winner.key)
            ));
        }
    }
    if renames.is_empty() {
        "rules: {}\n".to_string()
    } else {
        format!("rules:\n{renames}")
    }
}

/// Advance the pass's rule names past every name the schema already declares.
fn next_rule_name(schema: &VaultSchema, next: &mut u64) -> String {
    loop {
        let name = format!("near_duplicate_{next}");
        *next += 1;
        if schema.rule(&name).is_none() {
            return name;
        }
    }
}

/// Use find's first-match drive for rule selectors and closed-set members.
fn rule_hygiene(
    snapshot: &Snapshot,
    schema: &VaultSchema,
    declared: &ContentModel,
    cost: &mut RepairAdvisoryCost,
) -> Result<(Collected, Collected), PageRefusal> {
    let mut unmatched = Collected::default();
    let mut unused = Collected::default();
    for rule in schema.rules() {
        cost.rules += 1;
        let predicates = selector_parts(rule, cost);
        let (matches, work) =
            snapshot.first_match(&predicates, rule.selector().exclude(), declared)?;
        cost.absorb(work);
        if !matches {
            unmatched.add(RepairAdvisory::schema_hygiene(
                SchemaHygiene::rule_matches_nothing(rule.name()),
            ));
        }
        unused_members(snapshot, schema, rule, declared, cost, &mut unused)?;
    }
    Ok((unmatched, unused))
}

/// Read a selector's membership values and captures through find's part grammar.
fn selector_parts(rule: &Rule, cost: &mut RepairAdvisoryCost) -> Vec<Predicate> {
    let selector = rule.selector();
    let mut predicates = Vec::new();
    for (key, values) in selector.frontmatter() {
        cost.selector_parts += values.len() as u64;
        predicates.push(Predicate::in_any(key, values.iter().cloned()));
    }
    if let Some(path) = selector.path() {
        let path = path
            .as_str()
            .split('/')
            .map(|segment| {
                if segment.starts_with('<') && segment.ends_with('>') {
                    "*"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        cost.selector_parts += path.chars().count() as u64;
        predicates.push(Predicate::path(path));
    }
    for path in selector.exclude() {
        cost.selector_parts += path.as_str().chars().count() as u64;
    }
    predicates
}

/// Treat a synonym as a use of its member in each vault-wide first-match find.
fn unused_members(
    snapshot: &Snapshot,
    schema: &VaultSchema,
    rule: &Rule,
    declared: &ContentModel,
    cost: &mut RepairAdvisoryCost,
    unused: &mut Collected,
) -> Result<(), PageRefusal> {
    for (field, set) in rule.one_of() {
        let mut synonyms: BTreeMap<_, (Vec<String>, Vec<String>)> = BTreeMap::new();
        for (written, target) in set.synonyms() {
            cost.selector_parts += 1;
            let group = synonyms
                .entry(schema.equality_key(field, target))
                .or_default();
            if schema.equality_key(field, written).is_some() {
                group.0.push(written.to_string());
            } else {
                group.1.push(written.to_string());
            }
        }
        let mut members = set.values().to_vec();
        members.sort();
        for member in members {
            cost.selector_parts += 1;
            let equality = schema.equality_key(field, &member);
            let group = synonyms.get(&equality);
            let mut values = vec![member.clone()];
            if let Some((readable, _)) = group {
                values.extend(readable.iter().cloned());
            }
            let (mut used, work) =
                snapshot.first_match(&[Predicate::in_any(field, values)], &[], declared)?;
            cost.absorb(work);
            if !used
                && let Some((_, written)) = group
                && !written.is_empty()
            {
                let (found, work) = snapshot.first_match_written(field, written, declared)?;
                used = found;
                cost.absorb(work);
            }
            if !used {
                unused.add(RepairAdvisory::schema_hygiene(
                    SchemaHygiene::unused_one_of_value(rule.name(), field, member),
                ));
            }
        }
    }
    Ok(())
}

/// Report only co-selecting rule-pair conflicts that admit no document.
fn empty_combinations(
    snapshot: &Snapshot,
    schema: &VaultSchema,
    cost: &mut RepairAdvisoryCost,
) -> Collected {
    let mut combinations = Collected::default();
    let rules: Vec<_> = schema.rules().collect();
    for (at, left) in rules.iter().enumerate() {
        for right in &rules[at + 1..] {
            cost.rule_pairs += 1;
            if !schema.may_co_select(left, right) {
                continue;
            }
            for conflict in schema
                .combined(&[left, right])
                .conflicts(snapshot.path_order().glob_case())
            {
                let conflict = match conflict {
                    RulesConflict::RequiredAndForbidden { field, .. } => {
                        HygieneConflict::required_and_forbidden(field)
                    }
                    RulesConflict::EmptyOneOf {
                        field,
                        required: true,
                        ..
                    } => HygieneConflict::empty_one_of(field),
                    RulesConflict::DisjointPlacement { .. } => {
                        HygieneConflict::disjoint_placement()
                    }
                    RulesConflict::EmptyOneOf {
                        required: false, ..
                    } => continue,
                };
                combinations.add(RepairAdvisory::schema_hygiene(
                    SchemaHygiene::empty_combination(
                        [left.name().to_string(), right.name().to_string()],
                        conflict,
                    ),
                ));
            }
        }
    }
    combinations
}

/// Merge catalogue kinds into the longest prefix that fits both ceilings.
fn truncated(kinds: [Collected; 7]) -> RepairAdvisories {
    let total = kinds.iter().map(|kind| kind.total).sum();
    let mut items = Vec::new();
    let mut bytes = 2;
    'catalogue: for kind in kinds {
        let complete = kind.items.len() as u64 == kind.total;
        for item in kind.items {
            let next = bytes + item_bytes(&item) + usize::from(!items.is_empty());
            if items.len() == ADVISORY_ITEMS || next > ADVISORY_BYTES {
                break 'catalogue;
            }
            items.push(item);
            bytes = next;
        }
        if !complete {
            break;
        }
    }
    RepairAdvisories::computed(items, total).expect("the prefix fits its total")
}

fn item_bytes(item: &RepairAdvisory) -> usize {
    serde_json::to_vec(item)
        .expect("an advisory serializes")
        .len()
}

/// Quote a YAML scalar, escaping characters its reader refuses or normalizes.
fn quoted(value: &str) -> String {
    let json = serde_json::to_string(value).expect("a string serializes");
    let mut yaml = String::with_capacity(json.len());
    for character in json.chars() {
        match character {
            '\u{7f}'..='\u{9f}' | '\u{fffe}' | '\u{ffff}' => {
                yaml.push_str(&format!("\\u{:04x}", character as u32))
            }
            character => yaml.push(character),
        }
    }
    yaml
}

fn declaration(kind: AdvisoryValueType) -> Option<&'static str> {
    match kind {
        AdvisoryValueType::String => Some("text"),
        AdvisoryValueType::Integer | AdvisoryValueType::Real => Some("number"),
        AdvisoryValueType::Boolean => Some("boolean"),
        _ => None,
    }
}

fn folded(key: &str) -> String {
    key.to_lowercase()
        .chars()
        .filter(|character| !matches!(character, '-' | '_' | ' ' | '.'))
        .collect()
}
