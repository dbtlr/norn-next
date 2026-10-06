//! Reading the `rules` section: each rule's structure, its selectors' values
//! under the declared types, its globs and captures, and the tokens its
//! templates may hold. What a rule states against itself or against other
//! rules is judged afterwards, over the whole schema, in `checks`.

use std::collections::{BTreeMap, BTreeSet};

use serde_yaml::{Mapping, Value};

use norn_wire::{AuthoredValue, FiniteFloat, PathProblem, Severity};

use super::super::creation::DefaultValue;
use super::super::template::{Part, Slot, Template, Token, is_identifier};
use super::super::{
    DeclaredField, FieldType, GlobProblem, Pattern, VaultSchemaError, at, known_keys_only,
    section_error,
};
use super::{
    AllowedPaths, ClosedSet, ForbiddenFix, Route, Rule, RuleDefault, RuleProblem, Selector,
    SelectorValues, equality_key, literal_holds,
};

/// The keys one rule holds.
const RULE_KEYS: &[&str] = &[
    "description",
    "severity",
    "match",
    "exclude",
    "required",
    "forbidden",
    "one_of",
    "max_length",
    "allowed_paths",
];

/// The keys a rule's `match` holds.
const MATCH_KEYS: &[&str] = &["frontmatter", "path"];

/// The keys a rule's `exclude` holds.
const EXCLUDE_KEYS: &[&str] = &["path"];

/// The keys one required field's declaration holds.
const REQUIRED_KEYS: &[&str] = &["default"];

/// The keys one forbidden field's rename holds.
const RENAME_KEYS: &[&str] = &["rename_to"];

/// The keys one closed set holds.
const ONE_OF_KEYS: &[&str] = &["values", "synonyms"];

/// The keys a rule's `allowed_paths` holds.
const ALLOWED_PATHS_KEYS: &[&str] = &["paths", "route"];

/// What a refusal of a rule node is.
fn refusal(at: impl Into<String>, problem: RuleProblem) -> VaultSchemaError {
    VaultSchemaError::Rule {
        at: at.into(),
        problem,
    }
}

/// Reads the `rules` section: each rule, by name.
///
/// A name written twice is a mapping key written twice, which the YAML
/// reader refuses before this runs, naming the key.
pub(in crate::schema) fn read_rules(
    document: &Mapping,
    fields: &BTreeMap<String, DeclaredField>,
) -> Result<BTreeMap<String, Rule>, VaultSchemaError> {
    let Some(value) = at(document, "rules") else {
        return Ok(BTreeMap::new());
    };
    let Value::Mapping(rules) = value else {
        return Err(section_error(
            "rules",
            "a mapping of rule name to rule",
            value,
        ));
    };
    let declared = |field: &str| {
        fields
            .get(field)
            .map_or(FieldType::Text, DeclaredField::kind)
    };
    rules
        .iter()
        .map(|(name, rule)| {
            let name = name
                .as_str()
                .ok_or_else(|| section_error("rules", "a mapping keyed by rule name", name))?;
            if !is_identifier(name) {
                return Err(refusal(
                    "rules",
                    RuleProblem::Name {
                        name: name.to_string(),
                    },
                ));
            }
            Ok((name.to_string(), read_rule(name, rule, &declared)?))
        })
        .collect()
}

/// The declared type of a field, text where nothing declares it.
type Declared<'a> = dyn Fn(&str) -> FieldType + 'a;

fn read_rule(name: &str, rule: &Value, declared: &Declared<'_>) -> Result<Rule, VaultSchemaError> {
    let section = format!("rules.{name}");
    // A rule written with nothing under its name is a rule stating nothing:
    // it selects every document and constrains none.
    let empty = Mapping::new();
    let rule = match rule {
        Value::Null => &empty,
        Value::Mapping(rule) => rule,
        other => return Err(section_error(&section, "a mapping", other)),
    };
    known_keys_only(&section, rule, RULE_KEYS)?;

    let description = match at(rule, "description") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| section_error(&format!("{section}.description"), "a string", value))?
                .to_string(),
        ),
    };
    let severity = match at(rule, "severity") {
        None => Severity::Warning,
        Some(value) => match value.as_str() {
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => {
                return Err(section_error(
                    &format!("{section}.severity"),
                    "`error` or `warning`",
                    value,
                ));
            }
        },
    };
    let selector = read_selector(&section, rule, declared)?;
    let captures: BTreeSet<String> = selector
        .path
        .as_ref()
        .map(|path| path.captures().map(str::to_string).collect())
        .unwrap_or_default();

    let required = field_map(&section, rule, "required")?
        .into_iter()
        .map(|(field, value, at_path)| Ok((field, read_requirement(&at_path, value, &captures)?)))
        .collect::<Result<_, VaultSchemaError>>()?;
    let forbidden = field_map(&section, rule, "forbidden")?
        .into_iter()
        .map(|(field, value, at_path)| Ok((field, read_forbidden(&at_path, value)?)))
        .collect::<Result<_, VaultSchemaError>>()?;
    let one_of = field_map(&section, rule, "one_of")?
        .into_iter()
        .map(|(field, value, at_path)| {
            let set = read_closed_set(&at_path, value, declared(&field))?;
            Ok((field, set))
        })
        .collect::<Result<_, VaultSchemaError>>()?;
    let max_length = field_map(&section, rule, "max_length")?
        .into_iter()
        .map(|(field, value, at_path)| {
            let limit = value
                .as_u64()
                .filter(|limit| *limit > 0)
                .ok_or_else(|| section_error(&at_path, "a positive integer", value))?;
            Ok((field, limit))
        })
        .collect::<Result<_, VaultSchemaError>>()?;
    let allowed_paths = match at(rule, "allowed_paths") {
        None => None,
        Some(value) => Some(read_allowed_paths(
            &format!("{section}.allowed_paths"),
            value,
            &captures,
        )?),
    };

    Ok(Rule {
        name: name.to_string(),
        description,
        severity,
        selector,
        required,
        forbidden,
        one_of,
        max_length,
        allowed_paths,
    })
}

/// The entries of the constraint `key` of `rule`, each a field name, its
/// value and its dotted path; none where the rule states no such constraint.
fn field_map<'a>(
    section: &str,
    rule: &'a Mapping,
    key: &str,
) -> Result<Vec<(String, &'a Value, String)>, VaultSchemaError> {
    let at_path = format!("{section}.{key}");
    let Some(value) = at(rule, key) else {
        return Ok(Vec::new());
    };
    let Value::Mapping(entries) = value else {
        return Err(section_error(
            &at_path,
            "a mapping keyed by field name",
            value,
        ));
    };
    entries
        .iter()
        .map(|(field, value)| {
            let field = field
                .as_str()
                .filter(|field| !field.is_empty())
                .ok_or_else(|| section_error(&at_path, "a mapping keyed by field name", field))?;
            Ok((field.to_string(), value, format!("{at_path}.{field}")))
        })
        .collect()
}

fn read_selector(
    section: &str,
    rule: &Mapping,
    declared: &Declared<'_>,
) -> Result<Selector, VaultSchemaError> {
    let mut selector = Selector::default();
    if let Some(value) = at(rule, "match") {
        let at_path = format!("{section}.match");
        let Value::Mapping(selecting) = value else {
            return Err(section_error(&at_path, "a mapping", value));
        };
        known_keys_only(&at_path, selecting, MATCH_KEYS)?;
        if let Some(value) = at(selecting, "frontmatter") {
            let at_path = format!("{at_path}.frontmatter");
            let Value::Mapping(keys) = value else {
                return Err(section_error(&at_path, "a mapping of key to value", value));
            };
            for (key, values) in keys {
                let key = key
                    .as_str()
                    .filter(|key| !key.is_empty())
                    .ok_or_else(|| section_error(&at_path, "a mapping keyed by field name", key))?;
                let at_key = format!("{at_path}.{key}");
                let read = read_selector_values(&at_key, values, declared(key))?;
                selector.frontmatter.insert(key.to_string(), read);
            }
        }
        if let Some(value) = at(selecting, "path") {
            selector.path = Some(read_glob(&format!("{at_path}.path"), value, true)?);
        }
    }
    if let Some(value) = at(rule, "exclude") {
        let at_path = format!("{section}.exclude");
        let Value::Mapping(excluding) = value else {
            return Err(section_error(&at_path, "a mapping", value));
        };
        known_keys_only(&at_path, excluding, EXCLUDE_KEYS)?;
        if let Some(value) = at(excluding, "path") {
            selector.exclude = read_globs(&format!("{at_path}.path"), value)?;
        }
    }
    Ok(selector)
}

/// One `match.frontmatter` key's value or any-of list, each value read under
/// the key's declared type.
fn read_selector_values(
    at_path: &str,
    value: &Value,
    declared: FieldType,
) -> Result<SelectorValues, VaultSchemaError> {
    const WANTED: &str = "a value or a non-empty list of values";
    let items: Vec<&Value> = match value {
        Value::Sequence(items) if !items.is_empty() => items.iter().collect(),
        Value::Sequence(_) => return Err(section_error(at_path, WANTED, value)),
        scalar => vec![scalar],
    };
    let mut values = SelectorValues {
        written: Vec::new(),
        keys: BTreeSet::new(),
    };
    for item in items {
        let raw = yaml_scalar_text(item).ok_or_else(|| section_error(at_path, WANTED, item))?;
        let key = equality_key(declared, &raw).ok_or_else(|| {
            refusal(
                at_path,
                RuleProblem::SelectorValue {
                    value: raw.clone(),
                    declared,
                },
            )
        })?;
        values.keys.insert(key);
        values.written.push(raw);
    }
    Ok(values)
}

/// A required field's declaration: nothing, or `{default: <value>}`.
fn read_requirement(
    at_path: &str,
    value: &Value,
    captures: &BTreeSet<String>,
) -> Result<Option<RuleDefault>, VaultSchemaError> {
    let declaration = match value {
        Value::Null => return Ok(None),
        Value::Mapping(declaration) => declaration,
        other => {
            return Err(section_error(
                at_path,
                "nothing, or a mapping holding a `default`",
                other,
            ));
        }
    };
    known_keys_only(at_path, declaration, REQUIRED_KEYS)?;
    let at_default = format!("{at_path}.default");
    match declaration.get("default") {
        None => Ok(None),
        // A default that is null would fill a field the requirement still
        // finds missing.
        Some(Value::Null) => Err(section_error(
            &at_default,
            "a value that is not null",
            &Value::Null,
        )),
        Some(value) => Ok(Some(RuleDefault {
            value: read_default(&at_default, value, captures, true)?,
        })),
    }
}

/// A default: a scalar, a string being a template, or — at the top — a list
/// of them.
fn read_default(
    at_path: &str,
    value: &Value,
    captures: &BTreeSet<String>,
    top: bool,
) -> Result<DefaultValue, VaultSchemaError> {
    const WANTED: &str = "a value or a list of values";
    Ok(match value {
        Value::Bool(flag) => DefaultValue::Plain(AuthoredValue::Bool(*flag)),
        Value::Number(number) => DefaultValue::Plain(if let Some(integer) = number.as_i64() {
            AuthoredValue::Integer(integer)
        } else if number.is_u64() {
            return Err(section_error(
                at_path,
                "an integer that fits a signed 64-bit integer",
                value,
            ));
        } else {
            number
                .as_f64()
                .and_then(|float| FiniteFloat::new(float).ok())
                .map(AuthoredValue::Float)
                .ok_or_else(|| section_error(at_path, "a finite number", value))?
        }),
        Value::String(source) => DefaultValue::Text(rule_template(at_path, source, captures)?),
        Value::Sequence(items) if top => DefaultValue::List(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    read_default(&format!("{at_path}.{index}"), item, captures, false)
                })
                .collect::<Result<_, _>>()?,
        ),
        other => return Err(section_error(at_path, WANTED, other)),
    })
}

/// A rule's template: the grammar's tokens restricted to the clock's and to
/// the captures the rule's own `match.path` defines.
fn rule_template(
    at_path: &str,
    source: &str,
    captures: &BTreeSet<String>,
) -> Result<Template, VaultSchemaError> {
    let template =
        Template::parse(source).map_err(|error| refusal(at_path, RuleProblem::Template(error)))?;
    for part in template.parts() {
        let Part::Token(token) = part else {
            continue;
        };
        match &token.slot {
            Slot::Now | Slot::Date | Slot::Time => {}
            Slot::Path(name) if captures.contains(name) => {}
            Slot::Path(name) => {
                return Err(refusal(
                    at_path,
                    RuleProblem::UndefinedCapture { name: name.clone() },
                ));
            }
            Slot::Seq | Slot::Var(_) => {
                return Err(refusal(
                    at_path,
                    RuleProblem::InadmissibleToken {
                        token: Token {
                            slot: token.slot.clone(),
                            slug: false,
                        }
                        .to_string(),
                    },
                ));
            }
        }
    }
    Ok(template)
}

/// A forbidden field's fix: nothing, `remove`, or `{rename_to: <field>}`.
fn read_forbidden(at_path: &str, value: &Value) -> Result<ForbiddenFix, VaultSchemaError> {
    const WANTED: &str = "nothing, `remove`, or a mapping holding a `rename_to`";
    match value {
        Value::Null => Ok(ForbiddenFix::Unfixed),
        Value::String(word) if word == "remove" => Ok(ForbiddenFix::Remove),
        Value::Mapping(rename) => {
            known_keys_only(at_path, rename, RENAME_KEYS)?;
            let at_target = format!("{at_path}.rename_to");
            let target = rename
                .get("rename_to")
                .ok_or_else(|| VaultSchemaError::Section {
                    at: at_target.clone(),
                    wanted: "a field name",
                    found: "absent".to_string(),
                })?;
            let name = target
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| section_error(&at_target, "a field name", target))?;
            Ok(ForbiddenFix::RenameTo(name.to_string()))
        }
        other => Err(section_error(at_path, WANTED, other)),
    }
}

/// A closed set: `{values: [...], synonyms: {written: member}}`.
fn read_closed_set(
    at_path: &str,
    value: &Value,
    declared: FieldType,
) -> Result<ClosedSet, VaultSchemaError> {
    let Value::Mapping(set) = value else {
        return Err(section_error(at_path, "a mapping holding `values`", value));
    };
    known_keys_only(at_path, set, ONE_OF_KEYS)?;
    let at_values = format!("{at_path}.values");
    const VALUES: &str = "a non-empty list of values";
    let values = match at(set, "values") {
        Some(Value::Sequence(items)) if !items.is_empty() => items
            .iter()
            .map(|item| {
                yaml_scalar_text(item).ok_or_else(|| section_error(&at_values, VALUES, item))
            })
            .collect::<Result<Vec<String>, _>>()?,
        Some(other) => return Err(section_error(&at_values, VALUES, other)),
        None => {
            return Err(VaultSchemaError::Section {
                at: at_values,
                wanted: VALUES,
                found: "absent".to_string(),
            });
        }
    };
    let mut members = BTreeMap::new();
    for value in &values {
        // A member that does not read as the type is refused by the checks,
        // which name it; it compares as nothing here.
        if let Some(key) = equality_key(declared, value) {
            members.entry(key).or_insert_with(|| value.clone());
        }
    }
    let at_synonyms = format!("{at_path}.synonyms");
    const SYNONYMS: &str = "a mapping of written value to member";
    let synonyms = match at(set, "synonyms") {
        None => Vec::new(),
        Some(Value::Mapping(entries)) => entries
            .iter()
            .map(|(written, member)| {
                let written = yaml_scalar_text(written)
                    .ok_or_else(|| section_error(&at_synonyms, SYNONYMS, written))?;
                let member = yaml_scalar_text(member)
                    .ok_or_else(|| section_error(&at_synonyms, SYNONYMS, member))?;
                Ok((written, member))
            })
            .collect::<Result<_, VaultSchemaError>>()?,
        Some(other) => return Err(section_error(&at_synonyms, SYNONYMS, other)),
    };
    Ok(ClosedSet {
        values,
        members,
        synonyms,
    })
}

fn read_allowed_paths(
    at_path: &str,
    value: &Value,
    captures: &BTreeSet<String>,
) -> Result<AllowedPaths, VaultSchemaError> {
    let Value::Mapping(allowed) = value else {
        return Err(section_error(at_path, "a mapping holding `paths`", value));
    };
    known_keys_only(at_path, allowed, ALLOWED_PATHS_KEYS)?;
    let at_paths = format!("{at_path}.paths");
    let paths = match at(allowed, "paths") {
        Some(value) => read_globs(&at_paths, value)?,
        None => {
            return Err(VaultSchemaError::Section {
                at: at_paths,
                wanted: "a non-empty list of globs",
                found: "absent".to_string(),
            });
        }
    };
    if paths.is_empty() {
        return Err(VaultSchemaError::Section {
            at: at_paths,
            wanted: "a non-empty list of globs",
            found: "an empty list".to_string(),
        });
    }
    let route = match at(allowed, "route") {
        None => None,
        Some(value) => {
            let at_route = format!("{at_path}.route");
            let source = value
                .as_str()
                .ok_or_else(|| section_error(&at_route, "a folder path", value))?;
            Some(read_route(&at_route, source, captures)?)
        }
    };
    Ok(AllowedPaths { paths, route })
}

/// A route: a rule template naming a folder, ending in `/`, judged on its
/// literal text with each token standing as a plain value, as a creation
/// rule's target is.
fn read_route(
    at_path: &str,
    source: &str,
    captures: &BTreeSet<String>,
) -> Result<Route, VaultSchemaError> {
    let template = rule_template(at_path, source, captures)?;
    let standing: String = template
        .parts()
        .iter()
        .map(|part| match part {
            Part::Literal(text) => text.as_str(),
            Part::Token(_) => "0",
        })
        .collect();
    let Some(folder) = standing.strip_suffix('/') else {
        return Err(refusal(at_path, RuleProblem::RouteNotFolder));
    };
    if let Some(problem) = PathProblem::of_segments(folder) {
        return Err(refusal(at_path, RuleProblem::RoutePath(problem)));
    }
    if let Some(character) = [':', '*', '?']
        .into_iter()
        .find(|character| literal_holds(&template, *character))
    {
        return Err(refusal(at_path, RuleProblem::RouteCharacter { character }));
    }
    if let Some(token) = template.parts().iter().find_map(|part| match part {
        Part::Token(Token {
            slot: Slot::Now,
            slug: false,
        }) => Some("now"),
        Part::Token(Token {
            slot: Slot::Time,
            slug: false,
        }) => Some("time"),
        _ => None,
    }) {
        return Err(refusal(
            at_path,
            RuleProblem::RouteClockWithColon {
                token: token.to_string(),
            },
        ));
    }
    Ok(Route { template })
}

/// A list of rule globs, none of which captures.
fn read_globs(at_path: &str, value: &Value) -> Result<Vec<Pattern>, VaultSchemaError> {
    let Value::Sequence(items) = value else {
        return Err(section_error(at_path, "a list of globs", value));
    };
    items
        .iter()
        .map(|item| read_glob(at_path, item, false))
        .collect()
}

/// One rule glob. Where `capturing`, a whole segment `<name>` is a capture,
/// named by an identifier and written once; elsewhere `<` and `>` are
/// refused, since in a rule glob they would read as a capture the grammar
/// binds only in `match.path` (see the [rules module](super)). No rule glob
/// holds an empty segment.
fn read_glob(at_path: &str, value: &Value, capturing: bool) -> Result<Pattern, VaultSchemaError> {
    let source = value
        .as_str()
        .ok_or_else(|| section_error(at_path, "a glob", value))?;
    let problem = |problem: GlobProblem| VaultSchemaError::Glob {
        at: at_path.to_string(),
        glob: source.to_string(),
        problem,
    };
    if source.is_empty() {
        return Err(VaultSchemaError::Section {
            at: at_path.to_string(),
            wanted: "a glob",
            found: "an empty string".to_string(),
        });
    }
    if source.split('/').any(str::is_empty) {
        return Err(problem(GlobProblem::EmptySegment));
    }
    if !capturing {
        if source.contains(['<', '>']) {
            return Err(problem(GlobProblem::CaptureOutsideMatch));
        }
        return Pattern::parse(source).map_err(|error| VaultSchemaError::Section {
            at: at_path.to_string(),
            wanted: "a glob",
            found: error.to_string(),
        });
    }
    let mut names = BTreeSet::new();
    for segment in source.split('/') {
        if !segment.contains(['<', '>']) {
            continue;
        }
        let Some(name) = segment
            .strip_prefix('<')
            .and_then(|rest| rest.strip_suffix('>'))
            .filter(|name| !name.contains(['<', '>']))
        else {
            return Err(problem(GlobProblem::CaptureNotWholeSegment {
                segment: segment.to_string(),
            }));
        };
        if !is_identifier(name) {
            return Err(problem(GlobProblem::CaptureName {
                name: name.to_string(),
            }));
        }
        if !names.insert(name) {
            return Err(problem(GlobProblem::CaptureTwice {
                name: name.to_string(),
            }));
        }
    }
    Pattern::parse_capturing(source).map_err(|error| VaultSchemaError::Section {
        at: at_path.to_string(),
        wanted: "a glob",
        found: error.to_string(),
    })
}

/// A YAML scalar's text as a field value's raw text reads, or nothing for a
/// null, a list, a map or a tagged value: a boolean as `true` or `false`, an
/// integer in decimal, and a float with its fraction kept.
fn yaml_scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                Some(integer.to_string())
            } else if let Some(integer) = number.as_u64() {
                Some(integer.to_string())
            } else {
                number
                    .as_f64()
                    .filter(|float| float.is_finite())
                    .map(super::float_text)
            }
        }
        Value::Null | Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => None,
    }
}
