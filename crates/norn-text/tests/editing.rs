//! Editing a field: the fidelity boundary, and what refuses instead of
//! guessing.
//!
//! The invariant every case here serves is one sentence: **bytes outside the
//! edited construct do not move.** Comments, blank structure, key order, line
//! terminators and the quoting of untouched values all survive.

use norn_text::{Document, EditError, LineEnding, Mapping, Value, render_document};

fn set(source: &str, field: &str, value: Value) -> Result<String, EditError> {
    Document::parse(source).set_field(field, &value)
}

fn remove(source: &str, field: &str) -> Result<String, EditError> {
    Document::parse(source).remove_field(field)
}

// ── Comments and blank structure survive (NRN-434) ───────────────────────

/// NRN-434: a standalone comment and the blank lines around it belong to the
/// document, not to the field they happen to sit above. Removing a field left
/// them absorbed into its range and deleted them, exit 0.
#[test]
fn removing_a_field_leaves_the_comments_and_blank_lines_around_it() {
    let source = "---\ntitle: hello\n\n# a standing comment\nstatus: draft\n---\nbody\n";
    assert_eq!(
        remove(source, "title"),
        Ok("---\n\n# a standing comment\nstatus: draft\n---\nbody\n".to_string())
    );
    assert_eq!(
        remove(source, "status"),
        Ok("---\ntitle: hello\n\n# a standing comment\n---\nbody\n".to_string())
    );
}

/// NRN-434: the last field's range ran to the end of the block, so a trailing
/// comment went with it.
#[test]
fn removing_the_last_field_leaves_a_trailing_comment() {
    let source = "---\ntitle: hello\n# a trailing note\n---\nbody\n";
    assert_eq!(
        remove(source, "title"),
        Ok("---\n# a trailing note\n---\nbody\n".to_string())
    );
}

/// NRN-434's other half: absorbing indented tails is correct and stays. A
/// blank line inside a folded scalar is that scalar's, so the whole fold goes
/// with the field.
#[test]
fn removing_a_folded_field_takes_the_blank_line_inside_the_fold() {
    let source = "---\ndescription: >\n  line one\n\n  line two\nother: x\n---\n";
    assert_eq!(
        Document::parse(source).frontmatter().and_then(|value| value
            .as_map()
            .and_then(|map| map.get("description"))
            .cloned()),
        Some(Value::String("line one\nline two\n".into()))
    );
    assert_eq!(
        remove(source, "description"),
        Ok("---\nother: x\n---\n".to_string())
    );
}

/// A block scalar with keep-chomping owns the blank lines below it as content,
/// so its range is not trimmed and a remove takes them.
#[test]
fn removing_a_keep_chomping_block_takes_the_blank_lines_it_owns() {
    let source = "---\ntext: |+\n  a\n\nother: x\n---\n";
    assert_eq!(
        remove(source, "text"),
        Ok("---\nother: x\n---\n".to_string())
    );
}

/// NRN-133: a column-0 block-sequence item is `key:`-shaped but is not a key.
/// Treating it as one truncated the preceding field and orphaned its tail.
#[test]
fn a_column_zero_sequence_of_mappings_is_removed_whole() {
    let source = "---\nitems:\n- name: one\n- name: two\ntitle: t\n---\n";
    assert_eq!(
        remove(source, "items"),
        Ok("---\ntitle: t\n---\n".to_string())
    );
}

/// NRN-141: a `key:`-shaped line inside a multi-line quoted value is not a
/// key either.
#[test]
fn a_key_shaped_line_inside_a_quoted_value_is_part_of_that_value() {
    let source = "---\nnote: \"first\nkey: not a key\"\ntitle: t\n---\n";
    assert_eq!(
        remove(source, "note"),
        Ok("---\ntitle: t\n---\n".to_string())
    );
}

// ── Line terminators survive (NRN-143) ───────────────────────────────────

/// NRN-143: every synthesis path emitted `\n` unconditionally, so editing a
/// CRLF document left it half LF.
#[test]
fn a_crlf_document_stays_crlf_through_every_synthesis_path() {
    let existing = "---\r\ntitle: hello\r\n---\r\nbody\r\n";
    assert_eq!(
        set(existing, "status", Value::String("draft".into())),
        Ok("---\r\ntitle: hello\r\nstatus: draft\r\n---\r\nbody\r\n".to_string())
    );
    assert_eq!(
        set(
            existing,
            "tags",
            Value::Sequence(vec!["a".into(), "b".into()])
        ),
        Ok("---\r\ntitle: hello\r\ntags:\r\n  - a\r\n  - b\r\n---\r\nbody\r\n".to_string())
    );

    let stub = "---\r\ntags:\r\n---\r\nbody\r\n";
    assert_eq!(
        set(stub, "tags", Value::Sequence(vec!["a".into()])),
        Ok("---\r\ntags:\r\n  - a\r\n---\r\nbody\r\n".to_string())
    );

    let bodiless = "# heading\r\ntext\r\n";
    assert_eq!(
        set(bodiless, "title", Value::String("t".into())),
        Ok("---\r\ntitle: t\r\n---\r\n# heading\r\ntext\r\n".to_string())
    );

    let sections = "---\r\ntitle: t\r\n---\r\n## Alpha\r\n\r\nold\r\n\r\n## Beta\r\n";
    assert_eq!(
        Document::parse(sections).replace_section("Alpha", "new"),
        Ok("---\r\ntitle: t\r\n---\r\n## Alpha\r\n\r\nnew\r\n\r\n## Beta\r\n".to_string())
    );

    let fields: Mapping = [("title", "t")].into_iter().collect();
    assert_eq!(
        render_document(&fields, "body\r\n", LineEnding::Crlf),
        Ok("---\r\ntitle: t\r\n---\r\nbody\r\n".to_string())
    );
}

// ── A stubbed field is settable (NRN-435) ────────────────────────────────

/// NRN-435: a key with nothing after its colon had no insertion point, so
/// populating a stub — a core real-vault flow — refused.
#[test]
fn a_stubbed_field_is_settable() {
    assert_eq!(
        set(
            "---\nstatus:\n---\n",
            "status",
            Value::String("draft".into())
        ),
        Ok("---\nstatus: draft\n---\n".to_string())
    );
    // Trailing padding is part of the empty value and goes with it.
    assert_eq!(
        set(
            "---\nstatus:   \n---\n",
            "status",
            Value::String("draft".into())
        ),
        Ok("---\nstatus: draft\n---\n".to_string())
    );
    // A stub whose value is a sequence becomes a block sequence.
    assert_eq!(
        set(
            "---\ntags:\n---\n",
            "tags",
            Value::Sequence(vec!["a".into()])
        ),
        Ok("---\ntags:\n  - a\n---\n".to_string())
    );
}

// ── An empty block is mutable (NRN-371) ──────────────────────────────────

/// NRN-371: a document norn creates has an empty block, which reads as null.
/// Refusing to write a field into it meant norn could not mutate its own
/// output. Writing the first field promotes the null to a mapping.
#[test]
fn an_empty_block_accepts_its_first_field() {
    assert_eq!(
        set("---\n---\n# body\n", "title", Value::String("t".into())),
        Ok("---\ntitle: t\n---\n# body\n".to_string())
    );
}

#[test]
fn removing_the_only_field_leaves_an_empty_block_that_is_still_mutable() {
    let emptied = remove("---\ntitle: t\n---\nbody\n", "title").expect("a removal");
    assert_eq!(emptied, "---\n---\nbody\n");
    assert_eq!(
        Document::parse(&emptied).set_field("title", &Value::String("again".into())),
        Ok("---\ntitle: again\n---\nbody\n".to_string())
    );
}

// ── The byte-order mark is never relocated (NRN-349, NRN-385) ────────────

/// NRN-385: a synthesizer that prepends a block writes it above the mark,
/// leaving the mark in the middle of the file and the block unreadable.
#[test]
fn a_synthesized_block_lands_after_the_byte_order_mark() {
    let edited = set(
        "\u{feff}# just a heading\n",
        "title",
        Value::String("t".into()),
    )
    .expect("a synthesized block");
    assert_eq!(edited, "\u{feff}---\ntitle: t\n---\n# just a heading\n");
    // The document reads back as one block, with the mark still first.
    let reread = Document::parse(&edited);
    assert!(reread.has_byte_order_mark());
    assert_eq!(
        reread.frontmatter(),
        Some(&Value::Map([("title", "t")].into_iter().collect()))
    );
    assert_eq!(edited.matches("---").count(), 2);
}

#[test]
fn editing_a_marked_document_leaves_the_mark_alone() {
    let edited = set(
        "\u{feff}---\ntitle: old\n---\nbody\n",
        "title",
        Value::String("new".into()),
    )
    .expect("an edit");
    assert_eq!(edited, "\u{feff}---\ntitle: new\n---\nbody\n");
}

// ── The whole-document span guard (NRN-128, NRN-133, NRN-141) ────────────

/// NRN-141: when the scan and the parser disagree anywhere in the block there
/// is no safe per-field split, so no field gets a span. Reading is unaffected
/// and every field edit refuses — the trust-preserving outcome, because a
/// wrong span corrupts a document while a refusal costs an edit.
#[test]
fn a_key_the_scanner_decodes_differently_refuses_the_whole_block() {
    let source = "---\n\"\\x61\": 1\nx61: 2\n---\n";
    let document = Document::parse(source);
    // Reading still works, and reports both keys the parser found.
    let map = document
        .frontmatter()
        .and_then(Value::as_map)
        .expect("a map");
    assert_eq!(map.keys().collect::<Vec<_>>(), ["a", "x61"]);
    assert!(document.fields().is_empty());
    assert!(
        document
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "frontmatter-not-editable")
    );
    for field in ["a", "x61"] {
        assert_eq!(
            document.set_field(field, &Value::Int(9)),
            Err(EditError::FieldNotEditable {
                field: field.to_string()
            })
        );
        assert_eq!(
            document.remove_field(field),
            Err(EditError::FieldNotEditable {
                field: field.to_string()
            })
        );
    }
}

/// The guard is whole-document: a well-formed field beside an unlocatable one
/// is refused too, because the unlocatable key's bytes would be absorbed into
/// its neighbour's range.
#[test]
fn a_well_formed_field_beside_an_ambiguous_one_refuses_as_well() {
    let source = "---\n\"\\x61\": 1\nx61: 2\ntitle: fine\n---\n";
    let document = Document::parse(source);
    assert!(document.field("title").is_none());
    assert_eq!(
        document.set_field("title", &Value::String("changed".into())),
        Err(EditError::FieldNotEditable {
            field: "title".to_string()
        })
    );
}

/// The refusal is what protects the block, so simplifying the guard away is a
/// regression. Over a corpus of documents built to defeat a byte scanner, no
/// accepted edit may lose a field or leave the block unparseable.
#[test]
fn no_accepted_edit_over_hostile_documents_loses_a_field() {
    let corpus = [
        "---\n\"\\x61\": 1\nx61: 2\n---\n",
        "---\nitems:\n- name: one\n- name: two\ntitle: t\n---\n",
        "---\nnote: \"first\nkey: not a key\"\ntitle: t\n---\n",
        "---\nflow: [\"a,b\", \"c]d\"]\ntitle: t\n---\n",
        "---\nflow: [\n  \"a: b\",\n  \"c\"\n]\ntitle: t\n---\n",
        "---\nanchored: &base\n  k: 1\ntitle: t\n---\n",
        "---\naliased: *missing\n---\n",
        "---\nfolded: >-\n  one\n\n  two\ntitle: t\n---\n",
        "---\nliteral: |\n  key: not a key\ntitle: t\n---\n",
        "---\n<<: {a: 1}\ntitle: t\n---\n",
        "---\nempty:\ntitle: t\n---\n",
        "---\ntime: 12:30\ntitle: t\n---\n",
        "---\ntrailing: hello   # note\ntitle: t\n---\n",
    ];
    for source in corpus {
        let document = Document::parse(source);
        let Some(map) = document.frontmatter().and_then(Value::as_map) else {
            continue;
        };
        for name in map.keys() {
            let before = map.clone();
            if let Ok(edited) = document.set_field(name, &Value::String("sentinel".into())) {
                let reread = Document::parse(&edited);
                let after = reread
                    .frontmatter()
                    .and_then(Value::as_map)
                    .unwrap_or_else(|| panic!("{source:?}: setting {name:?} broke the block"));
                let mut expected = before.clone();
                expected.insert(name, Value::String("sentinel".into()));
                assert_eq!(after, &expected, "{source:?}: setting {name:?}");
            }
            if let Ok(edited) = document.remove_field(name) {
                let reread = Document::parse(&edited);
                let mut expected = before.clone();
                expected.remove(name);
                let after = match reread.frontmatter() {
                    Some(Value::Map(map)) => map.clone(),
                    Some(Value::Null) => Mapping::new(),
                    other => panic!("{source:?}: removing {name:?} produced {other:?}"),
                };
                assert_eq!(after, expected, "{source:?}: removing {name:?}");
            }
        }
    }
}

// ── What refuses ─────────────────────────────────────────────────────────

#[test]
fn a_value_no_span_can_name_refuses_an_in_place_edit_and_stays_removable() {
    for source in [
        "---\nk: |\n  literal\nother: x\n---\n",
        "---\nk: >\n  folded\nother: x\n---\n",
        "---\nk: {a: 1}\nother: x\n---\n",
        "---\nk: &anchor value\nother: x\n---\n",
        "---\nk: !tagged value\nother: x\n---\n",
    ] {
        assert_eq!(
            set(source, "k", Value::String("new".into())),
            Err(EditError::FieldNotEditable {
                field: "k".to_string()
            }),
            "for {source:?}"
        );
        assert_eq!(
            remove(source, "k"),
            Ok("---\nother: x\n---\n".to_string()),
            "for {source:?}"
        );
    }
}

#[test]
fn a_sequence_offered_for_a_scalar_field_refuses_rather_than_restyling_it() {
    assert!(matches!(
        set(
            "---\ntitle: hello\n---\n",
            "title",
            Value::Sequence(vec!["a".into()])
        ),
        Err(EditError::Render(_))
    ));
}

#[test]
fn a_map_value_refuses() {
    let nested: Mapping = [("inner", "x")].into_iter().collect();
    assert!(matches!(
        set("---\ntitle: hello\n---\n", "title", Value::Map(nested)),
        Err(EditError::Render(_))
    ));
}

#[test]
fn an_unreadable_block_refuses_every_field_edit() {
    for source in [
        "---\ntitle: : :\n---\nbody\n",
        "---\ntitle: hello\nno closing fence\n",
        "---\na: 1\na: 2\n---\n",
    ] {
        assert_eq!(
            set(source, "title", Value::String("x".into())),
            Err(EditError::FrontmatterUnreadable),
            "for {source:?}"
        );
        assert_eq!(
            remove(source, "title"),
            Err(EditError::FrontmatterUnreadable),
            "for {source:?}"
        );
    }
}

#[test]
fn a_block_that_is_not_a_mapping_has_no_fields_to_edit() {
    assert_eq!(
        set(
            "---\n- one\n- two\n---\n",
            "title",
            Value::String("x".into())
        ),
        Err(EditError::FrontmatterNotAMapping { kind: "sequence" })
    );
}

#[test]
fn removing_a_field_that_is_not_there_says_so() {
    assert_eq!(
        remove("---\ntitle: t\n---\n", "missing"),
        Err(EditError::FieldAbsent {
            field: "missing".to_string()
        })
    );
}

// ── Style and order survive ──────────────────────────────────────────────

#[test]
fn a_new_field_is_appended_and_never_sorted_in() {
    assert_eq!(
        set("---\nzebra: 1\nalpha: 2\n---\n", "middle", Value::Int(3)),
        Ok("---\nzebra: 1\nalpha: 2\nmiddle: 3\n---\n".to_string())
    );
}

#[test]
fn a_sequence_keeps_the_spelling_its_author_chose() {
    assert_eq!(
        set(
            "---\ntags: [a, b]\n---\n",
            "tags",
            Value::Sequence(vec!["x".into(), "y".into()])
        ),
        Ok("---\ntags: [x, y]\n---\n".to_string())
    );
    assert_eq!(
        set(
            "---\ntags:\n  - a\n  - b\n---\n",
            "tags",
            Value::Sequence(vec!["x".into()])
        ),
        Ok("---\ntags:\n  - x\n---\n".to_string())
    );
}

#[test]
fn an_empty_sequence_is_written_so_it_reads_back_as_a_sequence() {
    // A bare `tags:` line would read back as null, not as the empty list it
    // was written as.
    let edited = set("---\ntags:\n  - a\n---\n", "tags", Value::Sequence(vec![]))
        .expect("an empty sequence");
    assert_eq!(edited, "---\ntags: []\n---\n");
    assert_eq!(
        Document::parse(&edited)
            .frontmatter()
            .and_then(Value::as_map)
            .and_then(|map| map.get("tags")),
        Some(&Value::Sequence(vec![]))
    );
}

/// The fidelity boundary, stated as bytes: a set moves the value span and
/// nothing else, and a remove moves the entry and nothing else.
#[test]
fn an_edit_moves_only_the_bytes_of_the_construct_it_addresses() {
    let corpus = [
        "---\ntitle: hello\nstatus: draft\n\n# note\ncount: 3\n---\nbody\n",
        "---\nquoted: \"a b\"\nsingle: 'c d'\n---\n",
        "---\ntitle: hello   # note\nother: x\n---\n",
        "\u{feff}---\ntitle: hello\nother: x\n---\nbody\n",
        "---\r\ntitle: hello\r\nother: x\r\n---\r\nbody\r\n",
    ];
    for source in corpus {
        let document = Document::parse(source);
        for field in document.fields() {
            if let Some(range) = &field.value_range {
                let edited = document
                    .set_field(&field.name, &Value::String("sentinel".into()))
                    .unwrap_or_else(|error| {
                        panic!("{source:?}: setting {:?}: {error}", field.name)
                    });
                assert_eq!(&edited[..range.start], &source[..range.start]);
                assert_eq!(
                    &edited[edited.len() - (source.len() - range.end)..],
                    &source[range.end..],
                    "{source:?}: setting {:?} moved bytes after the value",
                    field.name
                );
            }
            let line = field.line_range.clone();
            let edited = document
                .remove_field(&field.name)
                .unwrap_or_else(|error| panic!("{source:?}: removing {:?}: {error}", field.name));
            assert_eq!(
                edited,
                format!("{}{}", &source[..line.start], &source[line.end..]),
                "{source:?}: removing {:?}",
                field.name
            );
        }
    }
}
