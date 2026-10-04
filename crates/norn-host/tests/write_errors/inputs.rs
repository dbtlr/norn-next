use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use norn_testkit::process::Sandbox;

use super::*;

/// Every public request form is a seed. Typed constructors keep the initial
/// seed valid; the generated mutations exercise the wire reader as well.
fn seeds(verb: Verb, mode: ApplyMode) -> Vec<Value> {
    let mut seeds = vec![request(verb, mode)];
    match verb {
        Verb::Set => {
            for change in [
                FieldChange::remove("status"),
                FieldChange::push("items", AuthoredValue::string("two")),
                FieldChange::pop("items", AuthoredValue::string("one")),
                FieldChange::set(
                    "nested",
                    AuthoredValue::list(vec![AuthoredValue::string("one")]),
                ),
            ] {
                seeds.push(json!(SetParams::new(
                    address(),
                    mode,
                    WriteTarget::path(path("subject.md")),
                    vec![change]
                )));
            }
            seeds.push(json!(SetParams::new(
                address(),
                mode,
                WriteTarget::matching(vec![Predicate::equal_to("status", "draft")]),
                vec![FieldChange::set("status", AuthoredValue::string("final"))]
            )));
        }
        Verb::Edit => {
            for edit in [
                DocumentEdit::replace_body("replacement\n"),
                DocumentEdit::replace_section("Section", "replacement\n"),
                DocumentEdit::append_to_section("Section", "append\n"),
                DocumentEdit::delete_section("Section"),
                DocumentEdit::insert_before_heading("Section", "before\n"),
                DocumentEdit::insert_after_heading("Section", "after\n"),
            ] {
                seeds.push(json!(EditParams::new(
                    address(),
                    mode,
                    path("subject.md"),
                    vec![edit]
                )));
            }
        }
        Verb::NewAs => {
            seeds.push(json!(NewParams::for_subject(
                address(),
                mode,
                NewSubject::inbox(ValueMap::default(), Some("inbox\n".into()))
            )));
        }
        Verb::Move => seeds.push(json!(MoveParams::new(
            address(),
            mode,
            MoveSubject::new("old", "new").unwrap()
        ))),
        Verb::Delete => {
            seeds.push(json!(DeleteParams::new(address(), mode, path("source.md"))));
            seeds.push(json!(
                DeleteParams::new(address(), mode, path("source.md")).breaking_links()
            ));
        }
        Verb::Apply => {
            for (kind, _) in faults::operation_cases() {
                seeds.push(apply_request(vec![Operation::new(kind)], mode));
            }
            let id = OperationId::new("same").unwrap();
            let operation = Operation::new(OperationKind::replace_body(
                path("subject.md"),
                "replacement\n",
            ))
            .with_id(id.clone());
            seeds.push(apply_request(vec![operation.clone(), operation], mode));
            seeds.push(apply_request(
                vec![
                    Operation::new(OperationKind::replace_body(
                        path("subject.md"),
                        "replacement\n",
                    ))
                    .with_id(id.clone())
                    .with_requires(vec![id]),
                ],
                mode,
            ));
        }
        Verb::New | Verb::Rewrite | Verb::Init | Verb::Migrate => {}
    }
    if !matches!(verb, Verb::Init | Verb::Migrate) {
        let mut guarded = seeds[0].clone();
        let conditions = json!([AuthorCondition::expected_value(
            path("subject.md"),
            "status",
            ExpectedField::present(AuthoredValue::string("draft"))
        )]);
        if matches!(verb, Verb::Apply) {
            guarded["plan"]["operations"][0]["conditions"] = conditions;
            guarded["plan"]["operations"][0]["id"] = json!("guarded");
            guarded["plan"]["operations"][0]["footnote"] = json!("an authored explanation");
            guarded["plan"]["force"] = json!(true);
        } else {
            guarded["conditions"] = conditions;
            guarded["force"] = json!(true);
        }
        seeds.push(guarded);
    }
    seeds
}

/// Mutate each JSON node independently: shape, scalar boundaries, document
/// paths, Unicode, YAML-sensitive strings, and nested authored values. A
/// fixed cartesian set makes the tested inputs reproducible.
fn mutations(seed: &Value) -> Vec<Value> {
    fn visit(value: &Value, pointer: String, nodes: &mut Vec<String>) {
        nodes.push(pointer.clone());
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    let key = key.replace('~', "~0").replace('/', "~1");
                    visit(value, format!("{pointer}/{key}"), nodes);
                }
            }
            Value::Array(array) => {
                for (index, value) in array.iter().enumerate() {
                    visit(value, format!("{pointer}/{index}"), nodes);
                }
            }
            _ => {}
        }
    }
    let mut nodes = Vec::new();
    visit(seed, String::new(), &mut nodes);
    let replacements = [
        json!(null),
        json!(false),
        json!(0),
        json!(-1),
        json!(u64::MAX),
        json!(""),
        json!("missing"),
        json!("../escape.md"),
        json!("../outside"),
        json!(".."),
        json!("old/../.."),
        json!("./subject.md"),
        json!(".norn/schema.yaml"),
        json!("é🦀\n\u{0}"),
        json!("00000000000000000000000000000000"),
        json!("sha256:0000000000000000000000000000000000000000000000000000000000000000"),
        json!("[broken: yaml"),
        json!([]),
        json!([null, {"nested": [1, "text"]}]),
        json!({}),
    ];
    let mut requests = vec![seed.clone()];
    for pointer in nodes {
        for replacement in &replacements {
            let mut request = seed.clone();
            *request.pointer_mut(&pointer).unwrap() = replacement.clone();
            requests.push(request);
        }
    }
    let mut unknown = seed.clone();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("unknown".into(), json!(true));
    requests.push(unknown);
    for key in seed.as_object().unwrap().keys() {
        let mut missing = seed.clone();
        missing.as_object_mut().unwrap().remove(key);
        requests.push(missing);
    }
    requests
}

fn allowed(verb: Verb, error: &ErrorEnvelope) -> bool {
    use ReasonCode::*;
    match error.code() {
        // These are admission refusals caused by an authored vault address.
        HostUnknownVault | HostUnsupportedAttachMode => true,
        RequestPlanInvalid | VaultRootChanged => matches!(verb, Verb::Apply),
        // A where builder's input refusal becomes an unresolved operation,
        // so it too must answer plan-refused rather than a query-only code.
        VaultPlanRefused => !matches!(verb, Verb::Init | Verb::Migrate),
        VaultMigrationRefused => matches!(verb, Verb::Migrate),
        _ => false,
    }
}

/// Give back demand and the host before removing the files it served.
struct Trial {
    _lease: norn_host::DemandLease<norn_host::ProductionEntryOps>,
    host: attach::ServingHost,
    identity: RootIdentity,
    root: PathBuf,
    baseline: BTreeMap<PathBuf, TreeEntry>,
    _sandbox: Sandbox,
}

#[derive(Eq, PartialEq)]
enum TreeEntry {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}

/// Judge fixture reuse from files, not the outcome's claim about writes.
fn tree_at(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(root: &Path, folder: &Path, tree: &mut BTreeMap<PathBuf, TreeEntry>) {
        for entry in std::fs::read_dir(folder).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            let fact = if kind.is_dir() {
                visit(root, &path, tree);
                TreeEntry::Directory
            } else if kind.is_symlink() {
                TreeEntry::Link(std::fs::read_link(&path).unwrap())
            } else {
                TreeEntry::File(std::fs::read(&path).unwrap())
            };
            tree.insert(path.strip_prefix(root).unwrap().to_owned(), fact);
        }
    }
    let mut tree = BTreeMap::new();
    visit(root, root, &mut tree);
    tree
}

impl Trial {
    fn new(verb: Verb) -> Self {
        let sandbox =
            Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "write-input-trial").unwrap();
        let root = sandbox.work_dir().join("attached");
        faults::tree(&root, verb);
        let vault = attach::Vault::adopt(&root);
        let host = vault.host();
        let lease = attach::attach_and_wait(&host, vault.name());
        let identity = norn_fs::path_identity(vault.path()).unwrap().unwrap();
        let root = vault.path().to_owned();
        let baseline = tree_at(&root);
        Self {
            _lease: lease,
            host,
            identity: RootIdentity::from_device_and_inode(identity.dev, identity.ino),
            root,
            baseline,
            _sandbox: sandbox,
        }
    }

    fn unchanged(&self) -> bool {
        tree_at(&self.root) == self.baseline
    }

    fn answer(
        &self,
        mut request: WriteRequest,
        seed_root: Option<&RootIdentity>,
    ) -> Result<Value, ErrorEnvelope> {
        request.bind_fixture(seed_root, self.identity.clone());
        request.answer(&self.host, false)
    }
}

#[allow(clippy::disallowed_macros)] // Harness evidence, not product rendering.
pub(super) fn run() {
    let mut totals = (0, 0, 0);
    for verb in Verb::ALL {
        for mode in [ApplyMode::Preview, ApplyMode::Apply] {
            for seed in seeds(verb, mode) {
                let written = seed.to_string();
                // Duplicate keys must be judged by the typed wire reader,
                // before any intermediate JSON value could drop one.
                for (key, value) in seed.as_object().unwrap() {
                    let duplicate = format!("{{{}:{value},{}", json!(key), &written[1..]);
                    assert!(
                        WriteRequest::read(verb, &duplicate).is_err(),
                        "{} accepted duplicate {key}: {duplicate}",
                        verb.name()
                    );
                }
                let mut subjects = vec![seed.clone()];
                if matches!(verb, Verb::Apply) {
                    let sandbox =
                        Sandbox::new(Path::new(env!("CARGO_TARGET_TMPDIR")), "write-input-plan")
                            .unwrap();
                    let root = sandbox.work_dir().join("attached");
                    faults::tree(&root, verb);
                    let vault = attach::Vault::adopt(&root);
                    let host = vault.host();
                    let _lease = attach::attach_and_wait(&host, vault.name());
                    let mut preview = seed.clone();
                    preview["mode"] = json!("preview");
                    if let Ok(Ok(answer)) = dispatch(&host, verb, &preview.to_string(), false) {
                        subjects.push(json!({"mode": if mode == ApplyMode::Apply { "apply" } else { "preview" }, "plan": answer["plan"]}));
                    }
                }
                let mut counts = (0, 0, 0);
                let mut trial: Option<Trial> = None;
                for subject in &subjects {
                    let seed_root: Option<RootIdentity> = subject["plan"]
                        .get("root")
                        .and_then(|root| serde_json::from_value(root.clone()).ok());
                    for request in mutations(subject) {
                        let label = format!("{} {mode:?}: {request}", verb.name());
                        let decoded = match WriteRequest::read(verb, &request.to_string()) {
                            Ok(decoded) => decoded,
                            Err(_) => {
                                counts.0 += 1;
                                continue;
                            }
                        };
                        let held = trial.get_or_insert_with(|| Trial::new(verb));
                        let mode = decoded.mode();
                        let answer = held.answer(decoded, seed_root.as_ref());
                        let unchanged = held.unchanged();
                        if mode == ApplyMode::Preview {
                            assert!(unchanged, "{label}: preview changed its fixture");
                        }
                        // Even a misreported landing cannot mask the next
                        // request: any physical change retires the whole host
                        // and tree, before another watcher lease is taken.
                        if !unchanged {
                            drop(trial.take());
                        }
                        match answer {
                            Ok(_) => counts.1 += 1,
                            Err(error) => {
                                assert!(
                                    allowed(verb, &error),
                                    "{label}: unexpected operation domain: {error:?}"
                                );
                                let encoded = serde_json::to_vec(&error).unwrap();
                                assert_eq!(
                                    serde_json::from_slice::<ErrorEnvelope>(&encoded).unwrap(),
                                    error,
                                    "{label}"
                                );
                                counts.2 += 1;
                            }
                        }
                    }
                }
                assert!(
                    counts.0 > 0,
                    "{}: generated malformed requests were not refused",
                    verb.name()
                );
                assert!(
                    counts.1 + counts.2 > 0,
                    "{}: no generated request reached the handler",
                    verb.name()
                );
                totals.0 += counts.0;
                totals.1 += counts.1;
                totals.2 += counts.2;
            }
        }
    }
    assert!(
        totals.1 > 0 && totals.2 > 0,
        "the generated requests must exercise success and domain refusal: {totals:?}"
    );
    println!(
        "write-inputs: {} decode refusals, {} successes, {} domain refusals",
        totals.0, totals.1, totals.2
    );
}
