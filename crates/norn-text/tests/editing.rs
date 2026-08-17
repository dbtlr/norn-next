//! Editing a field: the fidelity boundary, and what refuses instead of
//! guessing.
//!
//! The invariant every case here serves is one sentence: **bytes outside the
//! edited construct do not move.** Comments, blank structure, key order, line
//! terminators and the quoting of untouched values all survive.

use norn_text::{
    DiagnosticCode, Document, EditError, LineEnding, Mapping, SourceSpan, SplitRefusal, Value,
    render_document,
};

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
    // It owns exactly as many as its value carries — two here, and no more.
    assert_eq!(
        remove("---\ntext: |+\n  a\n\n\nother: x\n---\n", "text"),
        Ok("---\nother: x\n---\n".to_string())
    );
}

/// NRN-434, again: a `+` in a comment trailing a block-scalar header is prose.
/// Reading it as keep-chomping hands the field the blank lines and the
/// standing comment below it, and the remove deletes both.
#[test]
fn a_plus_in_a_block_scalar_header_comment_is_not_keep_chomping() {
    let source = "---\ntext: | # keep this, and note the +\n  a\n\n# a standing comment\nother: \
                  x\n---\n";
    assert_eq!(
        remove(source, "text"),
        Ok("---\n\n# a standing comment\nother: x\n---\n".to_string())
    );
}

/// Keep-chomping owns blank lines, and a comment is not a blank line. A
/// column-0 comment ends the block scalar, so it and everything below it are
/// the document's.
#[test]
fn a_keep_chomping_block_does_not_own_the_comment_below_its_blank_lines() {
    let source = "---\ntext: |+\n  a\n\n# a standing comment\nother: x\n---\n";
    assert_eq!(
        remove(source, "text"),
        Ok("---\n# a standing comment\nother: x\n---\n".to_string())
    );
}

/// The chomping indicator crossed with what may follow it on the header line.
/// Each row states what removing the field leaves standing, which is the only
/// thing chomping decides here: whether the blank line below the value is the
/// value's or the document's.
#[test]
fn every_chomping_indicator_owns_only_what_its_value_carries() {
    for (header, expected) in [
        // Clip and strip own no trailing blank line, with or without a comment
        // — and the `+` inside the comment changes nothing.
        ("|", "---\n\n# note\nother: x\n---\n"),
        ("| # trailing +", "---\n\n# note\nother: x\n---\n"),
        ("|-", "---\n\n# note\nother: x\n---\n"),
        ("|- # trailing +", "---\n\n# note\nother: x\n---\n"),
        // Keep owns the blank line and never the comment, however the header
        // spells itself.
        ("|+", "---\n# note\nother: x\n---\n"),
        ("|+ # trailing +", "---\n# note\nother: x\n---\n"),
        ("|2+", "---\n# note\nother: x\n---\n"),
        ("|+2 # trailing +", "---\n# note\nother: x\n---\n"),
    ] {
        let source = format!("---\ntext: {header}\n  a\n\n# note\nother: x\n---\n");
        assert_eq!(
            remove(&source, "text").as_deref(),
            Ok(expected),
            "for header {header:?}"
        );
    }
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

// ── One block, before and after (NRN-25) ─────────────────────────────────

/// A fence the reader does not recognize is a corruption, not a limitation:
/// the document has no block as far as the reader is concerned, so the edit
/// synthesizes one above the real one, the re-read finds the synthesized block
/// first, and everything the document actually said is shadowed.
#[test]
fn an_edit_into_a_loose_fenced_document_writes_into_the_block_that_is_there() {
    let source = "---   \na: 1\n---\nbody\n";
    let edited = set(source, "k", Value::String("v".into())).expect("an edit");
    assert_eq!(edited, "---   \na: 1\nk: v\n---\nbody\n");
    let reread = Document::parse(&edited);
    assert_eq!(
        reread.frontmatter(),
        Some(&Value::Map(
            [("a", Value::Int(1)), ("k", Value::String("v".into()))]
                .into_iter()
                .collect()
        ))
    );
    assert_eq!(reread.body(), "body\n");
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

    // Section content carries its own line structure, and single-line content
    // binds nothing: the terminator between two written lines is the one the
    // splice chooses, so the content has to have an interior break for the
    // claim to be tested at all. It arrives here written LF, as content
    // written anywhere else would be.
    let sections = "---\r\ntitle: t\r\n---\r\n## Alpha\r\n\r\nold\r\n\r\n## Beta\r\n";
    assert_eq!(
        Document::parse(sections).replace_section("Alpha", "one\ntwo\n\nthree"),
        Ok(
            "---\r\ntitle: t\r\n---\r\n## Alpha\r\n\r\none\r\ntwo\r\n\r\nthree\r\n\r\n## Beta\r\n"
                .to_string()
        )
    );

    let fields: Mapping = [("title", "t")].into_iter().collect();
    assert_eq!(
        render_document(&fields, "body\r\n", LineEnding::Crlf),
        Ok("---\r\ntitle: t\r\n---\r\nbody\r\n".to_string())
    );

    // Nothing above leaves a lone LF behind: every break in every result is a
    // CRLF pair.
    for edited in [
        set(existing, "status", Value::String("draft".into())),
        set(stub, "tags", Value::Sequence(vec!["a".into()])),
        set(bodiless, "title", Value::String("t".into())),
        Document::parse(sections).replace_section("Alpha", "one\ntwo"),
        remove(existing, "title"),
    ] {
        let edited = edited.expect("an edit");
        assert_eq!(
            edited.matches('\n').count(),
            edited.matches("\r\n").count(),
            "a lone line feed survived in {edited:?}"
        );
    }
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

/// The mark is the document's first bytes and stays them, through every path
/// that writes. A prefix untouched by a set but relocated by a remove or a
/// section replace is the same defect found one path later.
#[test]
fn editing_a_marked_document_leaves_the_mark_alone() {
    let source = "\u{feff}---\ntitle: old\nother: x\n---\n## Alpha\n\nbody\n";
    let document = Document::parse(source);
    let edits = [
        document.set_field("title", &Value::String("new".into())),
        document.remove_field("title"),
        document.replace_section("Alpha", "rewritten"),
    ];
    for edited in edits {
        let edited = edited.expect("an edit");
        assert!(edited.starts_with('\u{feff}'), "the mark moved: {edited:?}");
        assert_eq!(edited.matches('\u{feff}').count(), 1, "{edited:?}");
        let reread = Document::parse(&edited);
        assert!(reread.has_byte_order_mark());
        assert!(reread.frontmatter().is_some(), "{edited:?}");
    }
    assert_eq!(
        set(
            "\u{feff}---\ntitle: old\n---\nbody\n",
            "title",
            Value::String("new".into())
        ),
        Ok("\u{feff}---\ntitle: new\n---\nbody\n".to_string())
    );
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
            .any(|diagnostic| diagnostic.code == DiagnosticCode::FrontmatterNotEditable)
    );
    // The refusal is the block's, not any one field's: the disagreement is
    // about the block's split, and a field-level answer would suggest another
    // field might be fine.
    // The refusal names the disagreement: the parser's `a` is the key no
    // candidate line spells, and every edit into the block reports that one
    // cause rather than the field it was asked about.
    let unlocated = SplitRefusal::KeyNotLocated {
        key: "a".to_string(),
        candidates: 0,
    };
    for field in ["a", "x61"] {
        assert_eq!(
            document.set_field(field, &Value::Int(9)),
            Err(EditError::FrontmatterNotEditable {
                cause: unlocated.clone()
            })
        );
        assert_eq!(
            document.remove_field(field),
            Err(EditError::FrontmatterNotEditable {
                cause: unlocated.clone()
            })
        );
    }
    // Appending a field the block does not have is an edit into the block too,
    // and lands between lines nothing can attribute. It refuses on the same
    // terms rather than slipping past a per-field guard.
    assert_eq!(
        document.set_field("brand new", &Value::Int(1)),
        Err(EditError::FrontmatterNotEditable {
            cause: unlocated.clone()
        })
    );
    assert_eq!(
        document.remove_field("brand new"),
        Err(EditError::FrontmatterNotEditable { cause: unlocated })
    );
}

/// A block with no fields is not the same thing as a block whose fields cannot
/// be located, and the two refuse differently. An empty mapping is fully
/// understood — nothing about it is untrusted — so the write is attempted, and
/// what refuses it is the post-image check discovering that a flow mapping
/// cannot be appended to. A block holding a key the value model dropped never
/// gets that far.
#[test]
fn a_block_with_no_fields_and_a_block_with_no_trustworthy_fields_refuse_differently() {
    assert_eq!(
        set("---\n{}\n---\nbody\n", "title", Value::String("t".into())),
        Err(EditError::PostImageMismatch {
            field: "title".to_string()
        })
    );
    assert_eq!(
        set("---\n1: x\n---\nbody\n", "title", Value::String("t".into())),
        Err(EditError::FrontmatterNotEditable {
            cause: SplitRefusal::UncleanStrip { dropped_entries: 1 }
        })
    );
    // And the empty block, which is null rather than a mapping, still takes
    // its first field.
    assert_eq!(
        set("---\n---\nbody\n", "title", Value::String("t".into())),
        Ok("---\ntitle: t\n---\nbody\n".to_string())
    );
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
        Err(EditError::FrontmatterNotEditable {
            cause: SplitRefusal::KeyNotLocated {
                key: "a".to_string(),
                candidates: 0,
            }
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
        "---\n? title\n: My Note\nk: 1\n---\n",
        "---\nbase: &b {title: x}\n? <<\n: *b\ntitle: t\n---\n",
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

// ── The refused split names its cause (NRN-152) ──────────────────────────

/// The cause a refused block reports, and the note filed beside it.
fn refusal_and_detail(source: &str) -> (SplitRefusal, String) {
    let document = Document::parse(source);
    let cause = match document.remove_field("any") {
        Err(EditError::FrontmatterNotEditable { cause }) => cause,
        other => panic!("{source:?}: expected a refused split, got {other:?}"),
    };
    // A reader that proposes no edit reaches the same cause off the document.
    assert_eq!(
        document.split_refusal(),
        Some(&cause),
        "{source:?}: the read side and the edit refusal name one cause"
    );
    let detail = document
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::FrontmatterNotEditable)
        .unwrap_or_else(|| panic!("{source:?}: a note about the refusal"))
        .detail
        .clone()
        .unwrap_or_else(|| panic!("{source:?}: the note carries the cause"));
    (cause, detail)
}

/// A strip that dropped an entry is fixed by rewriting the entry the value
/// model has no shape for, so that is what the refusal says — and how many
/// entries went, which a block losing several does not have to be re-read to
/// find out.
#[test]
fn an_unclean_strip_reports_the_entries_the_value_model_dropped() {
    let (cause, detail) = refusal_and_detail("---\n1: x\ntitle: t\n---\nbody\n");
    assert_eq!(cause, SplitRefusal::UncleanStrip { dropped_entries: 1 });
    assert_eq!(detail, "the value model dropped an entry the parser read");

    let (cause, detail) = refusal_and_detail("---\n1: x\n2: y\ntitle: t\n---\nbody\n");
    assert_eq!(cause, SplitRefusal::UncleanStrip { dropped_entries: 2 });
    assert_eq!(detail, "the value model dropped 2 entries the parser read");
}

/// A drop below the top level costs the split too, and the cause says only
/// what is true of it: an entry went. Where it sits is the note filed as it
/// was dropped, which carries the path — the cause claims no top-level line
/// the dropped entry never occupied.
#[test]
fn a_drop_below_the_top_level_reports_the_same_cause_and_claims_no_line() {
    let source = "---\ntitle: t\nlist:\n  - 1: x\n---\nbody\n";
    let (cause, detail) = refusal_and_detail(source);
    assert_eq!(cause, SplitRefusal::UncleanStrip { dropped_entries: 1 });
    assert_eq!(detail, "the value model dropped an entry the parser read");

    // The note beside it is what places the entry, and it places this one
    // inside the sequence rather than at the top level.
    let placed = Document::parse(source)
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::FrontmatterNonStringKey)
        .and_then(|diagnostic| diagnostic.detail.clone());
    assert_eq!(placed.as_deref(), Some("at `list[0]`"));
}

/// A key the scan cannot locate is fixed by rewriting that key, so the refusal
/// names it. Zero candidate lines and two are the same refusal and different
/// documents — one key the scan never spells, one name two lines claim — and
/// the count separates them without a second read.
#[test]
fn a_key_no_line_uniquely_locates_reports_the_key_and_how_many_lines_claim_it() {
    // The parser decodes `"\x61"` to `a` and the scan does not, so `a` is a
    // parsed key no candidate line spells.
    let (cause, detail) = refusal_and_detail("---\n\"\\x61\": 1\nx61: 2\n---\nbody\n");
    assert_eq!(
        cause,
        SplitRefusal::KeyNotLocated {
            key: "a".to_string(),
            candidates: 0,
        }
    );
    assert_eq!(detail, "the parsed key \"a\" is on no candidate key line");

    // The quoted scalar's step-over stops at its interior quote, re-exposing
    // an `other:` line that collides with the real one.
    let (cause, detail) =
        refusal_and_detail("---\nq: 'a\nb'' c\nother: inside\nz'\nother: 2\n---\nbody\n");
    assert_eq!(
        cause,
        SplitRefusal::KeyNotLocated {
            key: "other".to_string(),
            candidates: 2,
        }
    );
    assert_eq!(
        detail,
        "the parsed key \"other\" is on 2 candidate key lines and belongs to one"
    );
}

/// An explicit-key indicator is fixed by rewriting one line, so the refusal
/// points at it. The position is the document's, not the block's: a caller
/// holding the source opens it at that line.
#[test]
fn an_explicit_key_indicator_reports_where_it_is_written() {
    let source = "---\nk: 1\n? title\n: My Note\n---\nbody\n";
    let (cause, detail) = refusal_and_detail(source);
    assert_eq!(
        cause,
        SplitRefusal::ExplicitKeyIndicator {
            at: SourceSpan {
                line: 3,
                column: 1,
                byte_offset: source.find("? title").expect("the indicator line"),
            }
        }
    );
    assert_eq!(
        detail,
        "the scan reads line 3 as a top-level explicit-key `?` indicator"
    );

    // The first indicator is the one reported: a block writing two is one
    // document to open, at the line the scan stopped on.
    let source = "---\n? one\n: 1\n? two\n: 2\n---\nbody\n";
    let (cause, _) = refusal_and_detail(source);
    assert_eq!(
        cause,
        SplitRefusal::ExplicitKeyIndicator {
            at: SourceSpan {
                line: 2,
                column: 1,
                byte_offset: source.find("? one").expect("the first indicator line"),
            }
        }
    );
}

/// The three causes are three answers, so a caller reading one knows which
/// shape to change. The set of blocks that refuse is unchanged by their being
/// told apart: each of these refused before it named why.
#[test]
fn the_three_refusal_shapes_report_three_different_causes() {
    let causes: Vec<SplitRefusal> = [
        "---\n1: x\ntitle: t\n---\nbody\n",
        "---\n\"\\x61\": 1\nx61: 2\n---\nbody\n",
        "---\nk: 1\n? title\n: My Note\n---\nbody\n",
    ]
    .iter()
    .map(|source| refusal_and_detail(source).0)
    .collect();

    assert!(
        matches!(causes[0], SplitRefusal::UncleanStrip { .. })
            && matches!(causes[1], SplitRefusal::KeyNotLocated { .. })
            && matches!(causes[2], SplitRefusal::ExplicitKeyIndicator { .. }),
        "{causes:?}"
    );
    // And the prose a caller reads differs with them, which is the whole point
    // of carrying the cause rather than a bare absence.
    let problems: Vec<String> = causes.iter().map(SplitRefusal::problem).collect();
    assert_eq!(
        problems
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "{problems:?}"
    );
}

/// A block failing two gates at once still reports one cause, and it is the
/// first gate checked: the strip is read before the block is scanned, so a
/// dropped entry is named ahead of an indicator further down. Which blocks
/// refuse does not move with that order — this one refuses either way — but
/// which remedy the caller is handed does, so the order is pinned here.
#[test]
fn a_block_failing_two_gates_reports_the_gate_checked_first() {
    // A non-string key the value model drops, and a top-level `? ` indicator
    // below it, each enough on its own.
    let (cause, detail) = refusal_and_detail("---\n1: x\n? k\n: 1\n---\nbody\n");
    assert_eq!(cause, SplitRefusal::UncleanStrip { dropped_entries: 1 });
    assert_eq!(detail, "the value model dropped an entry the parser read");
    assert_eq!(
        refusal_and_detail("---\n? k\n: 1\n---\nbody\n").0,
        SplitRefusal::ExplicitKeyIndicator {
            at: SourceSpan {
                line: 2,
                column: 1,
                byte_offset: 4,
            }
        }
    );
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

/// A field a merge *introduces* has no bytes of its own — the directive that
/// produced it names a mapping written somewhere else — so the block holding
/// it has a parsed key no line can be attributed to. There is no trustworthy
/// per-field split, so every field edit in the block refuses, and the merge
/// line is never absorbed into a neighbour's range and deleted.
#[test]
fn a_merge_that_introduces_a_new_key_refuses_every_field_edit() {
    let source = "---\n<<: {a: 1}\ntitle: t\n---\nbody\n";
    let document = Document::parse(source);
    assert!(document.fields().is_empty());
    for field in ["a", "title", "<<"] {
        assert!(
            document
                .set_field(field, &Value::String("x".into()))
                .is_err(),
            "setting {field:?}"
        );
        assert!(document.remove_field(field).is_err(), "removing {field:?}");
    }
    // Reading is unaffected, and the bytes round-trip.
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map(
            [("title", Value::String("t".into())), ("a", Value::Int(1))]
                .into_iter()
                .collect()
        ))
    );
}

/// The merge key is not itself the disqualifier. A merge that contributes no
/// key the block does not already write leaves every parsed key attributable
/// to exactly one line, so the block keeps its per-field split and edits in it
/// are ordinary edits — the directive's own line still belongs to no field.
#[test]
fn a_merge_that_introduces_no_new_key_leaves_the_block_editable() {
    let source = "---\nbase: &b {title: x}\ntitle: t\n<<: *b\n---\nbody\n";
    assert_eq!(
        set(source, "title", Value::String("new".into())),
        Ok("---\nbase: &b {title: x}\ntitle: new\n<<: *b\n---\nbody\n".to_string())
    );
    assert!(
        !Document::parse(source)
            .fields()
            .iter()
            .any(|field| field.name == "<<"),
        "the merge line is no field's bytes"
    );
}

/// Appending into such a block is an ordinary append: the new entry goes in
/// before the closing delimiter and the re-read proves every other field, its
/// position included, unmoved. An expansion that permuted key order made this
/// refuse — the re-read found the block's own fields in a different order than
/// the pre-image it was compared against.
#[test]
fn an_append_into_a_merge_bearing_block_succeeds() {
    let source = "---\nb: &m {x: 1}\n<<: *m\nx: 2\np: 3\nq: 4\nr: 5\n---\nbody\n";
    assert_eq!(
        set(source, "zz", Value::Int(7)),
        Ok("---\nb: &m {x: 1}\n<<: *m\nx: 2\np: 3\nq: 4\nr: 5\nzz: 7\n---\nbody\n".to_string())
    );
}

/// An explicit key — `? key` on one line, `: value` on the next — writes its
/// key on a line carrying no `key:` separator, so no scanned line can be
/// attributed to it. A block writing one at the top level refuses every field
/// edit, whatever the key is, and still reads its whole value model.
#[test]
fn an_explicit_key_refuses_every_field_edit() {
    for (source, keys) in [
        (
            "---\n? title\n: My Note\nk: 1\n---\nbody\n",
            &["title", "k"],
        ),
        (
            "---\nk: 1\n? title\n: My Note\n---\nbody\n",
            &["k", "title"],
        ),
        (
            "---\n? title\n: |\n  one\n  two\nk: 1\n---\nbody\n",
            &["title", "k"],
        ),
        (
            "---\n? title\n: My Note\n? other\n: o\nk: 1\n---\nbody\n",
            &["title", "other"],
        ),
        ("---\n? title\nk: 1\n---\nbody\n", &["title", "k"]),
    ] {
        let document = Document::parse(source);
        assert!(document.fields().is_empty(), "for {source:?}");
        assert!(
            document
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::FrontmatterNotEditable),
            "for {source:?}"
        );
        for field in ["title", "k", "other", "absent"] {
            assert!(
                matches!(
                    document.set_field(field, &Value::String("x".into())),
                    Err(EditError::FrontmatterNotEditable {
                        cause: SplitRefusal::ExplicitKeyIndicator { .. }
                    })
                ),
                "{source:?}: setting {field:?}"
            );
            assert!(
                matches!(
                    document.remove_field(field),
                    Err(EditError::FrontmatterNotEditable {
                        cause: SplitRefusal::ExplicitKeyIndicator { .. }
                    })
                ),
                "{source:?}: removing {field:?}"
            );
        }
        // The value model is untouched: the block still derives it whole,
        // explicit keys folded in like any other.
        let map = document
            .frontmatter()
            .and_then(Value::as_map)
            .unwrap_or_else(|| panic!("{source:?}: a mapping"));
        assert!(
            keys.iter().all(|key| map.contains_key(key)),
            "{source:?}: read {:?}",
            map.keys().collect::<Vec<_>>()
        );
    }

    // What is refused is the per-field split, not the block. A whole-block
    // write is built from the value model and the body, both of which the
    // refusal leaves standing, so it lands and writes the mapping out in the
    // ordinary form.
    let document = Document::parse("---\n? title\n: My Note\nk: 1\n---\nbody\n");
    let map = document
        .frontmatter()
        .and_then(Value::as_map)
        .expect("a mapping");
    assert_eq!(
        render_document(map, document.body(), document.line_ending()),
        Ok("---\ntitle: My Note\nk: 1\n---\nbody\n".to_string())
    );
}

/// The merge directive written in the explicit-key form is a directive line no
/// field owns and none bounds. Absorbing it into the entry above handed a
/// remove of that entry the directive's bytes as well; the block refuses its
/// split instead, and the same refusal covers the whole explicit-key class
/// rather than this one key.
#[test]
fn an_explicit_merge_directive_is_never_deleted_by_a_neighbours_remove() {
    let source = "---\nbase: &b {title: x}\n? <<\n: *b\ntitle: t\n---\nbody\n";
    let document = Document::parse(source);
    assert!(document.fields().is_empty());
    // The directive is refused as the explicit-key spelling it is written in,
    // named at the line that writes it.
    let indicator = EditError::FrontmatterNotEditable {
        cause: SplitRefusal::ExplicitKeyIndicator {
            at: SourceSpan::at(source, source.find("? <<").expect("the directive line")),
        },
    };
    assert_eq!(document.remove_field("base"), Err(indicator.clone()));
    assert_eq!(
        document.set_field("title", &Value::String("new".into())),
        Err(indicator)
    );
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map(
            [
                (
                    "base",
                    Value::Map([("title", Value::String("x".into()))].into_iter().collect())
                ),
                ("title", Value::String("t".into())),
            ]
            .into_iter()
            .collect()
        ))
    );
}

/// What a refused block keeps and what it loses, on a block the explicit-key
/// rule refuses. The value model reads whole — every field's value is
/// still there to be read — while the reads that report a field's *bytes* go
/// empty, because the refusal is exactly the statement that no bytes can be
/// attributed to a field. A frontmatter tag and a frontmatter wikilink in such
/// a block are therefore not reported, the same answer every other refused
/// block already gives.
#[test]
fn a_refused_block_reads_its_values_and_reports_none_of_their_bytes() {
    let source = "---\nbase: &b {title: x}\n? <<\n: *b\ntitle: t\ntags: [alpha]\n\
                  link: \"[[Note]]\"\n---\nbody\n";
    let document = Document::parse(source);

    assert!(document.field_texts().is_empty());
    assert!(document.frontmatter_tags().is_empty());
    assert!(document.frontmatter_wikilinks().is_empty());

    let map = document
        .frontmatter()
        .and_then(Value::as_map)
        .expect("a mapping");
    assert_eq!(
        map.get("tags"),
        Some(&Value::Sequence(vec![Value::String("alpha".into())]))
    );
    assert_eq!(map.get("link"), Some(&Value::String("[[Note]]".into())));
    assert_eq!(map.get("title"), Some(&Value::String("t".into())));
}

/// The step-over that keeps a quoted scalar's lines out of the scan stops at
/// the first line holding the quote character, so a scalar carrying one inside
/// itself re-exposes the lines below. A `? ` there is read as the indicator and
/// the block refuses, though its split would have held. The block keeps its
/// value model either way. What the re-exposed lines cost when they spell a
/// key instead is pinned in the test below.
#[test]
fn a_question_mark_below_an_interior_quote_refuses_the_block() {
    for (source, key) in [
        ("---\nq: 'a\nb'' c\n? d\ne'\nk: 1\n---\nbody\n", "q"),
        ("---\nq: \"a\n\\\"x\n? d\ne\"\nk: 1\n---\nbody\n", "q"),
    ] {
        let document = Document::parse(source);
        assert!(document.fields().is_empty(), "for {source:?}");
        // The reported line is the re-exposed one, which is where the scan
        // read the indicator and not where the remedy is: those bytes are
        // interior to `q`'s value and the quoting above them is what would
        // have to change. The cause says what the scan read for that reason.
        assert_eq!(
            document.remove_field("k"),
            Err(EditError::FrontmatterNotEditable {
                cause: SplitRefusal::ExplicitKeyIndicator {
                    at: SourceSpan {
                        line: 4,
                        column: 1,
                        byte_offset: source.find("? d").expect("the re-exposed line"),
                    }
                }
            }),
            "for {source:?}"
        );
        assert_eq!(
            document
                .split_refusal()
                .map(SplitRefusal::problem)
                .as_deref(),
            Some("the scan reads line 4 as a top-level explicit-key `?` indicator"),
            "for {source:?}"
        );
        let map = document
            .frontmatter()
            .and_then(Value::as_map)
            .unwrap_or_else(|| panic!("{source:?}: a mapping"));
        assert!(map.contains_key(key), "for {source:?}");
        assert_eq!(map.get("k"), Some(&Value::Int(1)), "for {source:?}");
    }
}

/// A re-exposed line that spells a key is a candidate the block never wrote as
/// structure, and which layer answers for it depends on the name it carries.
/// Colliding with a real key it refuses the block outright. Locating a parsed
/// key nothing else does it becomes a boundary: the entry above is truncated
/// inside its own quoted scalar and a field is emitted over bytes that belong
/// to that scalar. The scan does not catch the second shape — the post-image
/// re-read of what a write would produce is what refuses the edits.
#[test]
fn a_key_re_exposed_below_an_interior_quote_is_answered_for_downstream() {
    // The re-exposed `other:` collides with the real one, so no key is
    // uniquely located and the block refuses before any span is emitted.
    let colliding = Document::parse("---\nq: 'a\nb'' c\nother: inside\nz'\nother: 2\n---\nbody\n");
    assert!(colliding.fields().is_empty());
    assert_eq!(
        colliding.remove_field("other"),
        Err(EditError::FrontmatterNotEditable {
            cause: SplitRefusal::KeyNotLocated {
                key: "other".to_string(),
                candidates: 2,
            }
        })
    );
    assert_eq!(
        colliding.frontmatter().and_then(Value::as_map),
        Some(
            &[
                ("q", Value::String("a b' c other: inside z".into())),
                ("other", Value::Int(2)),
            ]
            .into_iter()
            .collect::<Mapping>()
        )
    );

    // Here the re-exposed `title:` is the only line naming the merge-supplied
    // `title`, so the split proceeds and is wrong in two places at once.
    let source =
        "---\nbase: &b {title: real}\n<<: *b\nq: 'a\nb'' c\ntitle: inside\nz'\n---\nbody\n";
    let split = Document::parse(source);
    assert_eq!(
        split
            .fields()
            .iter()
            .map(|field| (field.name.as_str(), &source[field.line_range.clone()]))
            .collect::<Vec<_>>(),
        [
            ("base", "base: &b {title: real}\n"),
            ("q", "q: 'a\nb'' c\n"),
            ("title", "title: inside\nz'\n"),
        ]
    );
    for field in ["base", "q", "title"] {
        assert_eq!(
            split.remove_field(field),
            Err(EditError::PostImageMismatch {
                field: field.to_string()
            }),
            "removing {field:?}"
        );
        assert_eq!(
            split.set_field(field, &Value::Int(1)),
            Err(EditError::FieldNotEditable {
                field: field.to_string()
            }),
            "setting {field:?}"
        );
    }
}

/// The indicator is `?` and the separator after it. A plain key opening with a
/// `?`, and a quoted key spelling one, are ordinary fields, and the lines of a
/// value that merely holds an explicit key belong to the entry that opened
/// them — the scan steps over an unclosed quoted scalar whose interior lines
/// carry no quote character of their own, and an indented explicit key is
/// inside a value its own top-level entry owns whole.
#[test]
fn a_question_mark_that_opens_no_explicit_key_leaves_the_block_editable() {
    for (source, edited) in [
        (
            "---\n?title: v\nk: 1\n---\nbody\n",
            "---\n?title: v\n---\nbody\n",
        ),
        (
            "---\n\"? title\": v\nk: 1\n---\nbody\n",
            "---\n\"? title\": v\n---\nbody\n",
        ),
        (
            "---\nq: \"one\n? two\n: three\"\nk: 1\n---\nbody\n",
            "---\nq: \"one\n? two\n: three\"\n---\nbody\n",
        ),
        (
            "---\nmeta:\n  ? a\n  : 1\nk: 1\n---\nbody\n",
            "---\nmeta:\n  ? a\n  : 1\n---\nbody\n",
        ),
        (
            "---\nnotes: |\n  ? a\n  : 1\nk: 1\n---\nbody\n",
            "---\nnotes: |\n  ? a\n  : 1\n---\nbody\n",
        ),
    ] {
        assert_eq!(
            remove(source, "k"),
            Ok(edited.to_string()),
            "for {source:?}"
        );
    }
}

/// A block written as one flow mapping reads. Where the opening brace shares a
/// line with a key the scan reads the brace as part of that key's name, so no
/// parsed key is located and the block refuses its split outright; where a
/// lone key survives that, its value span carries the flow's punctuation and
/// refuses, and the post-image re-read answers for the rest. These shapes pin
/// that nothing gets through, not which layer answers.
///
/// A flow mapping whose braces sit on lines of their own is the shape that
/// does take an edit, and it is pinned in the test below.
#[test]
fn no_edit_into_a_flow_written_block_is_accepted() {
    for source in [
        "---\n{k: 1, title: t}\n---\nbody\n",
        "---\n{k: 1,\n title: t}\n---\nbody\n",
        "---\n{k: 1}\n---\nbody\n",
        "---\n{\nk: 1}\n---\nbody\n",
        "---\n{}\n---\nbody\n",
    ] {
        let document = Document::parse(source);
        for field in ["k", "title", "appended"] {
            assert!(
                document.set_field(field, &Value::Int(2)).is_err(),
                "{source:?}: setting {field:?}"
            );
            assert!(
                document.remove_field(field).is_err(),
                "{source:?}: removing {field:?}"
            );
        }
        assert!(
            document.frontmatter().and_then(Value::as_map).is_some(),
            "{source:?}: reads as a mapping"
        );
    }
    // A flow mapping written across lines is one block-level node, so a key
    // beside it is not a second entry and the block does not parse at all.
    assert_eq!(
        remove("---\n{k: 1}\ntitle: t\n---\nbody\n", "k"),
        Err(EditError::FrontmatterUnreadable)
    );
}

/// A flow mapping whose braces sit on lines of their own leaves every key on
/// an ordinary `key:` line, so the split holds and a remove of a key another
/// entry follows lands: the deleted line carries its own separating comma and
/// the flow stays closed. What still refuses is everything that would leave
/// the flow half-written: removing the last key would take the closing brace,
/// and a set over a plain scalar refuses because its value span runs to the
/// end of the line and so holds the flow's comma or brace. A single-line
/// quoted value ends its span at the closing quote, leaving the punctuation
/// outside it, and its set lands; a quoted value continuing onto the next
/// line has no value span at all, and its set refuses.
#[test]
fn a_flow_mapping_braced_on_its_own_lines_takes_a_remove() {
    assert_eq!(
        remove("---\n{\nk: 1,\ntitle: t}\n---\nbody\n", "k"),
        Ok("---\n{\ntitle: t}\n---\nbody\n".to_string())
    );
    assert_eq!(
        remove("---\n{\na: 1,\nb: 2,\nc: 3}\n---\n", "b"),
        Ok("---\n{\na: 1,\nc: 3}\n---\n".to_string())
    );
    // The nested flow's own lines belong to the entry that opened it, so that
    // entry's remove takes them and no more.
    assert_eq!(
        remove("---\n{\na: {\nb: 1},\nc: 2}\n---\n", "a"),
        Ok("---\n{\nc: 2}\n---\n".to_string())
    );
    // The last key holds the closing brace, and the re-read of what the remove
    // would write refuses it.
    assert_eq!(
        remove("---\n{\nk: 1,\ntitle: t}\n---\nbody\n", "title"),
        Err(EditError::PostImageMismatch {
            field: "title".to_string()
        })
    );
    assert_eq!(
        set("---\n{\nk: 1,\ntitle: t}\n---\nbody\n", "k", Value::Int(2)),
        Err(EditError::FieldNotEditable {
            field: "k".to_string()
        })
    );
    assert_eq!(
        set(
            "---\n{\nk: 1,\ntitle: t}\n---\nbody\n",
            "appended",
            Value::Int(2)
        ),
        Err(EditError::PostImageMismatch {
            field: "appended".to_string()
        })
    );
    // A quoted value's span stops at its closing quote, so the comma after it
    // is not written over and the set lands. The quoted bytes may spell a
    // brace; it is the flow's own punctuation the span has to stay clear of.
    assert_eq!(
        set(
            "---\n{\na: \"x}y\",\nb: 2}\n---\n",
            "a",
            Value::String("NEW".into())
        ),
        Ok("---\n{\na: \"NEW\",\nb: 2}\n---\n".to_string())
    );
    assert_eq!(
        remove("---\n{\na: \"x}y\",\nb: 2}\n---\n", "a"),
        Ok("---\n{\nb: 2}\n---\n".to_string())
    );
}

/// A value spelled `---` is a value, and the span layer refuses it rather than
/// splicing over bytes that also spell a fence. Nothing here writes a fence
/// into a value's place, and nothing reads this one as a block boundary — the
/// fence scan works on whole lines, and this one is not one.
#[test]
fn a_value_spelled_like_a_fence_refuses_an_in_place_edit() {
    let source = "---\na: ---\nother: x\n---\nbody\n";
    let document = Document::parse(source);
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map(
            [("a", Value::String("---".into())), ("other", "x".into())]
                .into_iter()
                .collect()
        ))
    );
    assert_eq!(
        set(source, "a", Value::String("new".into())),
        Err(EditError::FieldNotEditable {
            field: "a".to_string()
        })
    );
    // It stays removable through its whole entry, like every other value no
    // span can name.
    assert_eq!(
        remove(source, "a"),
        Ok("---\nother: x\n---\nbody\n".to_string())
    );
}

/// A tab-indented continuation is not YAML: tabs are forbidden as indentation.
/// The block does not parse, so there is nothing to read and every edit
/// refuses — which is the honest answer, rather than a scan guessing at a
/// structure the parser rejected.
#[test]
fn a_tab_indented_sequence_does_not_parse_and_refuses_every_edit() {
    let source = "---\ntags:\n\t- a\n\t- b\ntitle: t\n---\nbody\n";
    let document = Document::parse(source);
    assert_eq!(document.frontmatter(), None);
    assert!(
        document
            .diagnostics()
            .iter()
            .any(|d| d.code == DiagnosticCode::FrontmatterParseFailed)
    );
    assert_eq!(
        set(source, "title", Value::String("x".into())),
        Err(EditError::FrontmatterUnreadable)
    );
}

/// A block past [`norn_text::FRONTMATTER_MAX_BYTES`] was never read, so there
/// is no mapping to write into and every edit refuses. The refusal matters
/// more than the read does: an edit that took the block for absent would
/// synthesize a *second* block above the first, and the re-read would find the
/// synthesized one and approve.
#[test]
fn an_oversized_block_refuses_every_edit_rather_than_writing_a_second_block() {
    let mut block = String::from("a: ");
    while block.len() < norn_text::FRONTMATTER_MAX_BYTES + 1 {
        block.push('x');
    }
    block.push('\n');
    let source = format!("---\n{block}---\nbody\n");
    let document = Document::parse(&source);
    assert_eq!(document.frontmatter(), None);
    assert!(
        document
            .diagnostics()
            .iter()
            .any(|d| d.code == DiagnosticCode::FrontmatterTooLarge)
    );
    assert_eq!(
        set(&source, "title", Value::String("x".into())),
        Err(EditError::FrontmatterUnreadable)
    );
    assert_eq!(remove(&source, "a"), Err(EditError::FrontmatterUnreadable));
}

/// A block that reads today and would be past the bound once a field is added
/// refuses on the bound, by name. The document parses clean, so the refusal
/// cannot be reported as the splice failing to read back: nothing is wrong
/// with the bytes the splice writes, and the reason the edit stops is the rule
/// that no read will turn them into fields again. Shrinking the same block
/// stays available, which is how the document gets back under the bound.
#[test]
fn an_edit_that_grows_the_block_past_the_bound_names_the_bound() {
    let bound = norn_text::FRONTMATTER_MAX_BYTES;
    let mut block = String::new();
    let mut index = 0;
    while block.len() + 17 <= bound {
        block.push_str(&format!("k{index:012}: 1\n"));
        index += 1;
    }
    // A comment line pads the tail out to the bound exactly, carrying block
    // bytes and no field.
    let padding = bound - block.len();
    block.push('#');
    block.push_str(&"p".repeat(padding - 2));
    block.push('\n');
    let source = format!("---\n{block}---\nbody\n");
    let document = Document::parse(&source);
    assert!(
        document.diagnostics().is_empty(),
        "{:?}",
        document.diagnostics()
    );
    assert_eq!(
        block.len(),
        bound,
        "the block is built to the bound exactly"
    );

    let entry = "newfield: v\n";
    assert_eq!(
        set(&source, "newfield", Value::String("v".into())),
        Err(EditError::FrontmatterPastBound {
            bytes: bound + entry.len(),
            bound,
        })
    );
    assert!(
        set(&source, "newfield", Value::String("v".into()))
            .unwrap_err()
            .to_string()
            .contains(&bound.to_string())
    );

    // Rewriting a field in place keeps the block the length it was, and
    // removing one shortens it: neither crosses the bound, so both stand.
    assert!(set(&source, "k000000000000", Value::Int(2)).is_ok());
    let shortened = remove(&source, "k000000000000").expect("removing a field shortens the block");
    assert!(Document::parse(&shortened).frontmatter().is_some());
}

/// A stub carrying a trailing comment takes a scalar and refuses a sequence,
/// and the asymmetry is not an oversight. A scalar splices into the insertion
/// point between the colon and the comment, so the comment stays put. A
/// sequence replaces the whole entry, and the comment sits inside it — so the
/// write would silently delete a comment the author put there, which the
/// post-image check has no way to notice because the block still reads back as
/// intended.
#[test]
fn a_stub_with_a_comment_takes_a_scalar_and_takes_a_sequence_with_the_comment() {
    let source = "---\ntags: # what goes here\nother: x\n---\n";
    assert_eq!(
        set(source, "tags", Value::String("draft".into())),
        Ok("---\ntags: draft # what goes here\nother: x\n---\n".to_string())
    );
    assert_eq!(
        set(source, "tags", Value::Sequence(vec!["a".into()])),
        Ok("---\ntags:\n  - a\nother: x\n---\n".to_string())
    );
}

/// Removing an anchor definition leaves its alias dangling, which makes the
/// block unparseable — so the post-image check refuses and no bytes are
/// returned. This is the gate doing its job on a document where every span was
/// correct and the edit was still wrong.
#[test]
fn removing_an_anchor_definition_its_alias_still_needs_is_refused() {
    let source = "---\na: &x 1\nb: *x\n---\nbody\n";
    assert_eq!(
        remove(source, "a"),
        Err(EditError::PostImageMismatch {
            field: "a".to_string()
        })
    );
    // Removing the alias is fine: nothing depends on it.
    assert_eq!(
        remove(source, "b"),
        Ok("---\na: &x 1\n---\nbody\n".to_string())
    );
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

/// Where the construct ends is where the boundary is. A scalar set replaces
/// the value's bytes, so a comment sharing that line survives; a sequence set
/// replaces the whole entry, so a comment written inside it is replaced with
/// it. A comment on its own line belongs to neither entry and always survives.
#[test]
fn a_comment_inside_a_replaced_entry_goes_with_it_and_one_outside_stays() {
    assert_eq!(
        set(
            "---\ntitle: hello # note\n---\n",
            "title",
            Value::String("new".into())
        ),
        Ok("---\ntitle: new # note\n---\n".to_string())
    );
    assert_eq!(
        set(
            "---\ntags: [a] # note\nother: x\n---\n",
            "tags",
            Value::Sequence(vec!["z".into()])
        ),
        Ok("---\ntags: [z]\nother: x\n---\n".to_string())
    );
    assert_eq!(
        set(
            "---\ntags: [a]\n# a standing note\nother: x\n---\n",
            "tags",
            Value::Sequence(vec!["z".into()])
        ),
        Ok("---\ntags: [z]\n# a standing note\nother: x\n---\n".to_string())
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
