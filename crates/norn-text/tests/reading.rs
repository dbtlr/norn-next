//! Reading a document: where the frontmatter block is, what it is worth, and
//! what reading it had to work around.
//!
//! Reading is forgiving by contract — a block that cannot be parsed yields a
//! diagnostic and a usable body, never an error return — so most of what this
//! file states is which diagnostic, and what survives beside it.

use norn_text::{
    BlockRefusal, DiagnosticCode, Document, LineEnding, Value, ValueStyle, render_document,
};

fn codes<'a>(document: &'a Document<'a>) -> Vec<&'a str> {
    document
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect()
}

fn map_of(source: &str) -> norn_text::Mapping {
    match Document::parse(source).frontmatter() {
        Some(Value::Map(map)) => map.clone(),
        other => panic!("expected a mapping, got {other:?}"),
    }
}

// ── The block, the body, and the boundary between them ───────────────────

#[test]
fn a_document_without_frontmatter_is_all_body() {
    let document = Document::parse("# heading\n");
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.frontmatter_range(), None);
    assert_eq!(document.body(), "# heading\n");
    assert_eq!(document.body_start(), 0);
    assert!(document.diagnostics().is_empty());
}

#[test]
fn the_block_range_covers_the_yaml_and_the_body_starts_past_the_closing_fence() {
    let source = "---\ntitle: hello\n---\n# heading\n";
    let document = Document::parse(source);
    let range = document.frontmatter_range().expect("a block");
    assert_eq!(&source[range], "title: hello\n");
    assert_eq!(document.body(), "# heading\n");
    assert_eq!(&source[document.body_start()..], "# heading\n");
    assert!(document.diagnostics().is_empty());
}

#[test]
fn a_crlf_document_reads_as_crlf() {
    let source = "---\r\ntitle: hello\r\n---\r\n# heading\r\n";
    let document = Document::parse(source);
    assert_eq!(document.line_ending(), LineEnding::Crlf);
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("title", "hello")].into_iter().collect()))
    );
    assert_eq!(document.body(), "# heading\r\n");
}

#[test]
fn a_document_with_no_line_break_at_all_reads_as_lf() {
    assert_eq!(
        Document::parse("no breaks here").line_ending(),
        LineEnding::Lf
    );
}

/// A fence carrying trailing spaces or tabs is a fence, at both ends. An
/// editor that trims neither writes both, and a fence this reader does not
/// recognize is not a smaller contract but a corrupting one: the fields go
/// into a second block synthesized above the real one.
#[test]
fn a_fence_with_trailing_whitespace_is_still_a_fence() {
    for (source, label) in [
        ("---   \ntitle: hello\n---\nbody\n", "loose opener"),
        ("---\ttitle\n", "not a fence at all"),
        ("---\ntitle: hello\n---   \nbody\n", "loose closer"),
        (
            "---\t\ntitle: hello\n---\t\nbody\n",
            "both loose, with tabs",
        ),
        (
            "---\r\ntitle: hello\r\n--- \r\nbody\r\n",
            "loose CRLF closer",
        ),
    ] {
        let document = Document::parse(source);
        if label == "not a fence at all" {
            assert_eq!(document.frontmatter(), None, "{label}");
            continue;
        }
        assert_eq!(
            document.frontmatter(),
            Some(&Value::Map([("title", "hello")].into_iter().collect())),
            "{label}"
        );
        assert!(document.diagnostics().is_empty(), "{label}");
        assert!(document.body().starts_with("body"), "{label}");
    }
    // Four dashes is not a fence, and neither is a fence with anything but
    // whitespace after it.
    for source in [
        "----\ntitle: hello\n---\n",
        "--- x\ntitle: hello\n---\n",
        "---\ntitle: hello\n--- x\n",
    ] {
        assert!(
            !matches!(Document::parse(source).frontmatter(), Some(Value::Map(_))),
            "{source:?} is not a block"
        );
    }
}

#[test]
fn an_unclosed_block_is_diagnosed_and_the_whole_document_stays_body() {
    let source = "---\ntitle: hello\n# heading (no close)\n";
    let document = Document::parse(source);
    assert_eq!(codes(&document), ["frontmatter-unclosed"]);
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.frontmatter_range(), None);
    assert_eq!(document.body(), source);
    assert_eq!(document.body_start(), 0);
}

#[test]
fn malformed_yaml_is_diagnosed_and_still_reports_where_the_block_was() {
    let document = Document::parse("---\ntitle: : :\n---\n# body\n");
    assert_eq!(codes(&document), ["frontmatter-parse-failed"]);
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.frontmatter_range(), Some(4..15));
    assert_eq!(document.body(), "# body\n");
    // A parse failure is a state of its own, not an absence: the diagnostic
    // carries the parser's own account of what went wrong.
    assert!(document.diagnostics()[0].detail.is_some());
}

/// The state a consumer deriving from a document branches on.
///
/// `frontmatter()` is `None` for a document with no block and for a document
/// whose block was read by nothing, and a consumer that cannot tell them apart
/// records *no tags, no title, no aliases* about a document whose fields were
/// never read. The refusal is what tells them apart, it names which defect the
/// block carries, and it agrees with the note filed beside it.
#[test]
fn a_block_read_by_nothing_carries_its_refusal_as_state() {
    let oversized = document_of(&block_of(norn_text::FRONTMATTER_MAX_BYTES + 1));
    let cases: [(&str, Option<DiagnosticCode>); 6] = [
        // No block at all: nothing refused anything.
        ("# heading\n", None),
        // A block that read.
        ("---\ntitle: hello\n---\n# body\n", None),
        // A block that read, with a note about what the read worked around.
        ("---\ntitle: !!str hello\n---\n# body\n", None),
        (
            "---\ntitle: hello\n# heading (no close)\n",
            Some(DiagnosticCode::FrontmatterUnclosed),
        ),
        (
            "---\ntitle: : :\n---\n# body\n",
            Some(DiagnosticCode::FrontmatterParseFailed),
        ),
        (
            oversized.as_str(),
            Some(DiagnosticCode::FrontmatterTooLarge),
        ),
    ];

    for (source, expected) in cases {
        let document = Document::parse(source);
        let refusal = document.frontmatter_refusal();
        assert_eq!(
            refusal.map(BlockRefusal::code),
            expected,
            "the refusal state disagrees for {:?}",
            &source[..source.len().min(40)]
        );
        assert_eq!(
            refusal.is_some(),
            document
                .diagnostics()
                .iter()
                .any(|note| note.code.refuses_the_block()),
            "the state and the notes disagree for {:?}",
            &source[..source.len().min(40)]
        );
        if refusal.is_some() {
            assert_eq!(
                document.frontmatter(),
                None,
                "a block read by nothing produced a value"
            );
        }
    }
}

/// A refusal that has more to say than its variant carries it: the account the
/// decoder gave, which is what a consumer reports beside the state.
#[test]
fn a_refusal_carries_the_decoder_s_account_of_the_block() {
    let block = block_of(norn_text::FRONTMATTER_MAX_BYTES * 2);
    let source = document_of(&block);
    let document = Document::parse(&source);
    let problem = document
        .frontmatter_refusal()
        .expect("the block is past the bound")
        .problem()
        .expect("the size refusal states the block's length and the bound");
    assert!(problem.contains(&block.len().to_string()), "{problem}");
    assert!(
        problem.contains(&norn_text::FRONTMATTER_MAX_BYTES.to_string()),
        "{problem}"
    );

    let malformed = Document::parse("---\ntitle: : :\n---\n# body\n");
    assert!(
        malformed
            .frontmatter_refusal()
            .expect("the block is not well-formed")
            .problem()
            .is_some_and(|problem| !problem.is_empty()),
        "the parse refusal states nothing"
    );

    let unclosed = Document::parse("---\ntitle: hello\n# heading (no close)\n");
    assert_eq!(
        unclosed
            .frontmatter_refusal()
            .expect("the block never closes")
            .problem(),
        None,
        "an unclosed block has nothing to say past the state itself"
    );
}

// ── A key written twice ──────────────────────────────────────────────────

/// Everything a consumer of a read document can see about it, with a refusal
/// reduced to its class. What the account of a refusal says is compared
/// separately, by [`refusal_account`], because the two refusals of a repeated
/// key know different amounts about where it is.
fn posture(source: &str) -> (Option<&'static str>, Vec<String>, bool, String) {
    let document = Document::parse(source);
    (
        document.frontmatter_refusal().map(|refusal| match refusal {
            BlockRefusal::Unclosed => "unclosed",
            BlockRefusal::TooLarge { .. } => "too large",
            BlockRefusal::Unreadable { .. } => "unreadable",
        }),
        codes(&document)
            .iter()
            .map(|code| (*code).to_string())
            .collect(),
        document.frontmatter().is_some(),
        document.body().to_string(),
    )
}

/// The account an unreadable block carries.
fn refusal_account(source: &str) -> String {
    match Document::parse(source).frontmatter_refusal() {
        Some(BlockRefusal::Unreadable { problem }) => problem.clone(),
        other => panic!("{source:?} is not an unreadable block: {other:?}"),
    }
}

/// **A tag is invisible for uniqueness.** A key written twice refuses the
/// block, and the spelling of the two keys is not part of the answer: the
/// parser refuses the pair it sees as one key, and the pair it holds apart —
/// a tagged key and a plain one, which the strip into the value model collapses
/// into one name — is refused where they collapse. The refusal class, the
/// notes, the absent value and the body are the same document either way, and
/// so is the account wherever the parser's own refusal has no position to add
/// to it.
#[test]
fn a_key_written_twice_refuses_the_block_however_it_was_spelled() {
    let plain = posture("---\nk: 1\nk: 2\n---\nbody\n");
    assert_eq!(plain.0, Some("unreadable"));
    assert_eq!(plain.1, ["frontmatter-parse-failed"]);
    assert!(!plain.2, "a refused block produced a value");
    assert_eq!(plain.3, "body\n");
    assert_eq!(
        refusal_account("---\nk: 1\nk: 2\n---\nbody\n"),
        "duplicate entry with key \"k\""
    );

    for spelling in [
        "---\n!x k: 1\nk: 2\n---\nbody\n",
        "---\nk: 1\n!x k: 2\n---\nbody\n",
        "---\n!x k: 1\n!y k: 2\n---\nbody\n",
    ] {
        assert_eq!(
            posture(spelling),
            plain,
            "{spelling:?} degrades differently from the plain duplicate"
        );
        assert_eq!(
            refusal_account(spelling),
            "duplicate entry with key \"k\"",
            "{spelling:?} accounts for the refusal differently"
        );
    }
}

/// **The account places the repeated key only where the refusal held a
/// position.** The parser knows the line and column its own duplicate sits at
/// and says so; a pair that only collapses into one key collapses after every
/// span the parse had, so the account it carries stops at the key and the path
/// to the mapping holding it. What a reader sees of the document is the same
/// either way — the account is more precise where more was known, never
/// different about what is wrong.
#[test]
fn the_account_places_the_repeated_key_only_where_the_parser_refused() {
    for (plain, tagged) in [
        (
            "---\n# a note\nk: 1\nk: 2\n---\nbody\n",
            "---\n# a note\n!x k: 1\nk: 2\n---\nbody\n",
        ),
        (
            "---\nouter:\n  k: 1\n  k: 2\n---\nbody\n",
            "---\nouter:\n  !x k: 1\n  k: 2\n---\nbody\n",
        ),
        (
            "---\nlist:\n  - k: 1\n    k: 2\n---\nbody\n",
            "---\nlist:\n  - !x k: 1\n    k: 2\n---\nbody\n",
        ),
    ] {
        assert_eq!(
            posture(plain),
            posture(tagged),
            "{tagged:?} degrades differently from {plain:?}"
        );
        let (placed, unplaced) = (refusal_account(plain), refusal_account(tagged));
        assert!(
            placed.starts_with(&unplaced),
            "{placed:?} is not {unplaced:?} plus a position"
        );
        assert!(placed.contains(" at line "), "{placed:?} places nothing");
        assert!(
            !unplaced.contains(" at line "),
            "{unplaced:?} places a key the collapse has no span for"
        );
    }
}

/// The collapse is checked wherever a mapping is built, so a repeated key
/// refuses the block at depth and whether the block wrote both entries or a
/// merge contributed one of them. The account names the mapping the repeat is
/// in, by the path to it.
#[test]
fn a_key_repeated_below_the_top_level_refuses_the_block_too() {
    for (source, expected) in [
        (
            "---\nouter:\n  !x k: 1\n  k: 2\n---\nbody\n",
            "outer: duplicate entry with key \"k\"",
        ),
        (
            "---\nbase: &b\n  k: 1\nout:\n  <<: *b\n  !x k: 2\n---\nbody\n",
            "out: duplicate entry with key \"k\"",
        ),
        (
            "---\nlist:\n  - !x k: 1\n    k: 2\n---\nbody\n",
            "list[0]: duplicate entry with key \"k\"",
        ),
    ] {
        let document = Document::parse(source);
        assert_eq!(
            document.frontmatter_refusal(),
            Some(&BlockRefusal::Unreadable {
                problem: expected.to_string()
            }),
            "{source:?}"
        );
        assert_eq!(codes(&document), ["frontmatter-parse-failed"], "{source:?}");
        assert_eq!(document.frontmatter(), None, "{source:?}");
        assert_eq!(document.body(), "body\n", "{source:?}");
    }
}

/// **A block refused at the collapse reports the refusal and nothing else.**
/// What the conversion stripped on its way to the collapse was stripped from a
/// value the block never gets, so a note about it would describe a document no
/// reader holds. A block the parser refuses reports one note; so does this one.
#[test]
fn a_block_refused_at_the_collapse_reports_only_the_refusal() {
    for source in [
        // A tagged value, whose strip is reported wherever a value survives it.
        "---\na: !x 1\nk: 1\n!y k: 2\n---\nbody\n",
        // A non-string key, whose entry is dropped with a note of its own.
        "---\n1: dropped\nk: 1\n!y k: 2\n---\nbody\n",
    ] {
        assert_eq!(
            codes(&Document::parse(source)),
            ["frontmatter-parse-failed"],
            "{source:?}"
        );
    }
}

/// **A block of many keys is held to the same rule as a block of two.** The
/// uniqueness check answers a mapping past a size through a table rather than a
/// scan, which is a cost decision and not a different question: a repeat among
/// dozens of keys refuses the block exactly as a repeat among two does.
#[test]
fn a_repeated_key_among_many_refuses_the_block_too() {
    let mut source = String::from("---\n");
    for index in 0..24 {
        source.push_str(&format!("k{index}: {index}\n"));
    }
    source.push_str("!x k7: 99\n---\nbody\n");

    assert_eq!(posture(&source), posture("---\nk: 1\n!x k: 2\n---\nbody\n"));
    assert_eq!(
        refusal_account(&source),
        "duplicate entry with key \"k7\"",
        "the account of a repeat among many keys names another one"
    );
}

/// A tag beside a key nothing else writes is stripped and its block read, so
/// what the refusal above turns on is the repetition and not the tag. The strip
/// is reported: a tag is dropped loudly wherever it sits, and a key whose name
/// silently lost bytes is a field the document does not appear to hold.
#[test]
fn a_tagged_key_with_no_twin_reads() {
    let source = "---\n!x k: 1\nother: 2\n---\nbody\n";
    let document = Document::parse(source);
    assert_eq!(document.frontmatter_refusal(), None);
    let map = map_of(source);
    assert_eq!(map.get("k"), Some(&Value::Int(1)));
    assert_eq!(map.get("other"), Some(&Value::Int(2)));

    let stripped = document
        .diagnostics()
        .iter()
        .find(|note| note.code == DiagnosticCode::FrontmatterTagStripped)
        .expect("a tag dropped from a key is reported");
    assert_eq!(stripped.detail.as_deref(), Some("`!x` on a key at `k`"));
}

/// A tag on a key the model cannot address is the one tag no strip is reported
/// for: the entry goes whole, so nothing is kept for a stripped tag to be
/// about, and the dropped-key note is the entry's whole account.
#[test]
fn a_tag_on_a_non_string_key_goes_with_the_entry_it_names() {
    let document = Document::parse("---\n!x 1: v\nother: 2\n---\nbody\n");
    assert_eq!(document.frontmatter_refusal(), None);
    assert_eq!(
        codes(&document),
        ["frontmatter-non-string-key", "frontmatter-not-editable"]
    );
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("other", 2)].into_iter().collect()))
    );
}

// ── The bound on the block ───────────────────────────────────────────────

/// A block of `bytes` bytes that parses to a mapping, closed and well-formed.
///
/// Keys are padded to a fixed width so the block lands on the requested length
/// exactly, which is what makes a test either side of the bound a test of the
/// bound rather than of the padding.
fn block_of(bytes: usize) -> String {
    let mut yaml = String::with_capacity(bytes);
    let mut index = 0usize;
    while yaml.len() + 16 <= bytes {
        yaml.push_str(&format!("k{index:012}: 1\n"));
        index += 1;
    }
    // Pad the tail out with a comment, which is block bytes carrying no field.
    if yaml.len() < bytes {
        let padding = bytes - yaml.len();
        yaml.push('#');
        yaml.push_str(&"p".repeat(padding - 2));
        yaml.push('\n');
    }
    assert_eq!(yaml.len(), bytes, "the block is built to an exact length");
    yaml
}

/// The pathological shape: nested flow collections that never close. The YAML
/// scanner is quadratic in the length of this block, so the bound is the only
/// thing between one ordinary-looking document and seconds of CPU.
fn unclosed_flow_nest(bytes: usize) -> String {
    let mut yaml = String::with_capacity(bytes);
    yaml.push_str("a: ");
    while yaml.len() + 1 < bytes {
        yaml.push('[');
    }
    yaml.push('\n');
    assert_eq!(yaml.len(), bytes, "the block is built to an exact length");
    yaml
}

fn document_of(block: &str) -> String {
    format!("---\n{block}---\n# body\n")
}

#[test]
fn a_block_at_the_bound_parses_and_one_byte_past_it_is_refused() {
    let under = document_of(&block_of(norn_text::FRONTMATTER_MAX_BYTES));
    let document = Document::parse(&under);
    assert!(document.diagnostics().is_empty(), "{:?}", codes(&document));
    assert!(matches!(document.frontmatter(), Some(Value::Map(_))));

    let over = document_of(&block_of(norn_text::FRONTMATTER_MAX_BYTES + 1));
    let document = Document::parse(&over);
    assert_eq!(codes(&document), ["frontmatter-too-large"]);
    assert_eq!(document.frontmatter(), None);
}

/// The split resolves every scanned key line against the parsed mapping, and a
/// block at the bound carries hundreds of them: each gets its own field, in
/// document order, and each field's value reads back as what the key holds.
#[test]
fn a_block_at_the_bound_splits_into_one_field_per_key() {
    let block = block_of(norn_text::FRONTMATTER_MAX_BYTES);
    let source = document_of(&block);
    let document = Document::parse(&source);

    let keys: Vec<&str> = match document.frontmatter() {
        Some(Value::Map(map)) => map.keys().collect(),
        other => panic!("expected a mapping, got {other:?}"),
    };
    assert!(keys.len() > 900, "the bound admits {} keys", keys.len());
    let named: Vec<&str> = document
        .fields()
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(named, keys, "one field per key, in document order");

    let texts = document.field_texts();
    assert!(
        texts.is_empty(),
        "every value in this block is an integer, so none of them is a text"
    );
    let last = document.fields().last().expect("a final field");
    assert_eq!(
        &source[last.value_range.clone().expect("a scalar value span")],
        "1"
    );
}

/// The refusal is the same shape every other block refusal has: no value, the
/// block's range still reported, the body still read, and the note carrying
/// the parser-free account of what was refused. Nothing is truncated — a
/// truncated block would read back as a document nobody wrote.
#[test]
fn an_oversized_block_is_refused_with_its_body_and_its_range_intact() {
    let block = block_of(norn_text::FRONTMATTER_MAX_BYTES * 2);
    let source = document_of(&block);
    let document = Document::parse(&source);
    assert_eq!(codes(&document), ["frontmatter-too-large"]);
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.frontmatter_range(), Some(4..4 + block.len()));
    assert_eq!(document.body(), "# body\n");
    let detail = document.diagnostics()[0]
        .detail
        .clone()
        .expect("the note states the block's length and the bound");
    assert!(detail.contains(&block.len().to_string()), "{detail}");
    assert!(
        detail.contains(&norn_text::FRONTMATTER_MAX_BYTES.to_string()),
        "{detail}"
    );
}

/// The regression: an unclosed nest of flow collections costs the YAML scanner
/// time quadratic in the block's length, so a 100 KiB block of it is seconds of
/// CPU on one document. Past the bound no parser sees it at all, and the wall
/// bound below is loose by orders of magnitude so that a slow machine still
/// measures the refusal rather than the parse.
#[test]
fn a_pathological_block_past_the_bound_is_refused_without_parsing_it() {
    let source = document_of(&unclosed_flow_nest(100 * 1024));
    let start = std::time::Instant::now();
    let document = Document::parse(&source);
    let elapsed = start.elapsed();
    assert_eq!(codes(&document), ["frontmatter-too-large"]);
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.body(), "# body\n");
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "reading a refused block took {elapsed:?}"
    );
}

/// The same shape *under* the bound is parsed, and what comes back is the
/// ordinary refusal for YAML that does not close. The bound refuses by size
/// and never by shape.
#[test]
fn the_pathological_shape_under_the_bound_is_still_read_and_diagnosed_as_yaml() {
    let source = document_of(&unclosed_flow_nest(1024));
    let document = Document::parse(&source);
    assert_eq!(codes(&document), ["frontmatter-parse-failed"]);
    assert_eq!(document.frontmatter(), None);
}

/// NRN-371, NRN-114: a document norn itself creates has an empty block, and it
/// must stay mutable. An empty block is null with a range, not an absent one.
#[test]
fn an_empty_block_reads_as_null_with_a_range() {
    let source = "---\n---\n# body\n";
    let document = Document::parse(source);
    assert_eq!(document.frontmatter(), Some(&Value::Null));
    assert_eq!(document.frontmatter_range(), Some(4..4));
    assert_eq!(document.body(), "# body\n");
    assert_eq!(document.body_start(), 8);
    assert!(document.diagnostics().is_empty());
}

#[test]
fn a_block_holding_a_sequence_reads_as_a_sequence_and_has_no_fields() {
    let document = Document::parse("---\n- one\n- two\n---\n# body\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Sequence(vec!["one".into(), "two".into()]))
    );
    assert!(document.fields().is_empty());
    assert!(document.diagnostics().is_empty());
}

// ── The byte-order mark (NRN-349, NRN-385) ───────────────────────────────

#[test]
fn a_byte_order_mark_does_not_hide_the_block() {
    let source = "\u{feff}---\ntitle: hello\n---\n# body\n";
    let document = Document::parse(source);
    assert!(document.has_byte_order_mark());
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("title", "hello")].into_iter().collect()))
    );
    // Offsets are content-absolute, mark included, so a splice computed from
    // them lands where it was measured.
    let range = document.frontmatter_range().expect("a block");
    assert_eq!(range, 7..20);
    assert_eq!(&source[range], "title: hello\n");
    assert_eq!(&source[document.body_start()..], "# body\n");
    assert!(document.diagnostics().is_empty());
}

#[test]
fn a_byte_order_mark_alone_does_not_invent_a_block() {
    let source = "\u{feff}# just a heading\n";
    let document = Document::parse(source);
    assert!(document.has_byte_order_mark());
    assert_eq!(document.frontmatter(), None);
    assert_eq!(document.frontmatter_range(), None);
    assert_eq!(document.body(), source);
    assert_eq!(document.body_start(), 0);
}

// ── The value model, and what it strips at the boundary ──────────────────

#[test]
fn the_seven_shapes_read_as_themselves() {
    let map = map_of(
        "---\nnothing:\nflag: true\ncount: 3\nratio: 1.5\nname: text\nlist:\n  - a\nnested:\n  \
         inner: x\n---\n",
    );
    assert_eq!(map.get("nothing"), Some(&Value::Null));
    assert_eq!(map.get("flag"), Some(&Value::Bool(true)));
    assert_eq!(map.get("count"), Some(&Value::Int(3)));
    assert_eq!(map.get("ratio"), Some(&Value::Float(1.5)));
    assert_eq!(map.get("name"), Some(&Value::String("text".into())));
    assert_eq!(map.get("list"), Some(&Value::Sequence(vec!["a".into()])));
    assert_eq!(
        map.get("nested"),
        Some(&Value::Map([("inner", "x")].into_iter().collect()))
    );
}

#[test]
fn a_mapping_keeps_the_order_the_document_wrote() {
    let map = map_of("---\nzebra: 1\nalpha: 2\nmiddle: 3\n---\n");
    assert_eq!(map.keys().collect::<Vec<_>>(), ["zebra", "alpha", "middle"]);
}

/// Expanding a merge moves no explicit key. The directive's line vacates and
/// everything the document wrote stays where it was written — the keys below
/// the `<<` line included, which is the half a swap-based removal breaks by
/// hoisting the block's last entry into the vacated slot.
#[test]
fn expanding_a_merge_keeps_the_order_the_document_wrote() {
    let merged = map_of("---\nb: &m {x: 1}\n<<: *m\nx: 2\np: 3\nq: 4\nr: 5\n---\n");
    assert_eq!(merged.keys().collect::<Vec<_>>(), ["b", "x", "p", "q", "r"]);
    // The explicit `x` wins over the merged one, as YAML says a merge means.
    assert_eq!(merged.get("x"), Some(&Value::Int(2)));

    // A key only the merge contributes has no position of its own, so it is
    // appended after every explicit key, in the order its source wrote it.
    let contributed = map_of("---\nbase: &m {y: 1, z: 2}\n<<: *m\na: 3\n---\n");
    assert_eq!(
        contributed.keys().collect::<Vec<_>>(),
        ["base", "a", "y", "z"]
    );
}

/// The order a merge-bearing block reads as is the order a rendered document
/// emits: the value model carries document order all the way to the bytes.
#[test]
fn a_merge_bearing_block_re_emits_in_document_order() {
    let map = map_of("---\n<<: {a: 1}\nb: 2\nc: 3\n---\n");
    assert_eq!(map.keys().collect::<Vec<_>>(), ["b", "c", "a"]);
    assert_eq!(
        render_document(&map, "body\n", LineEnding::Lf),
        Ok("---\nb: 2\nc: 3\na: 1\n---\nbody\n".to_string())
    );
}

/// A non-string key has no addressable form, so its entry is dropped rather
/// than coerced into a field the document does not contain. The block's spans
/// go with it: the scanner still sees the line the value model no longer
/// names, and absorbing that line into a neighbour is how an unrelated remove
/// deletes it.
#[test]
fn a_non_string_key_is_dropped_and_makes_the_block_uneditable() {
    let document = Document::parse("---\n1: x\ntrue: y\nname: z\n---\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("name", "z")].into_iter().collect()))
    );
    assert_eq!(
        codes(&document),
        [
            "frontmatter-non-string-key",
            "frontmatter-non-string-key",
            "frontmatter-not-editable"
        ]
    );
    assert!(document.fields().is_empty());
}

#[test]
fn a_sequence_key_and_a_null_key_are_dropped_without_losing_the_rest() {
    let document = Document::parse("---\n[a, b]: x\n~: v\nname: z\n---\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("name", "z")].into_iter().collect()))
    );
    assert_eq!(
        document
            .diagnostics()
            .iter()
            .filter(|d| d.code == DiagnosticCode::FrontmatterNonStringKey)
            .count(),
        2
    );
}

/// An explicit tag is dropped and its value kept. The tag's bytes are never
/// rewritten either: a value opening with `!` carries no value span, so no
/// edit can splice over the marker.
#[test]
fn an_explicit_tag_is_stripped_and_its_value_kept() {
    let document = Document::parse("---\na: !foo bar\n---\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("a", "bar")].into_iter().collect()))
    );
    assert_eq!(codes(&document), ["frontmatter-tag-stripped"]);
    let field = document.field("a").expect("the key stays visible");
    assert_eq!(field.value_range, None);
}

/// A tag the YAML parser itself resolves — `!!str` and the other core schema
/// tags — never reaches the value model, so nothing is reported for it.
#[test]
fn a_core_schema_tag_is_resolved_by_the_parser_and_reported_by_nobody() {
    let document = Document::parse("---\na: !!str 5\n---\n");
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map([("a", "5")].into_iter().collect()))
    );
    assert!(document.diagnostics().is_empty());
}

/// Scalar resolution is the YAML 1.2 core schema, stated rather than
/// inherited. Every spelling here meant something else in YAML 1.1, and a
/// vault holding `publish: no` means the word, not the boolean. A change of
/// dialect is a change of contract and shows up as a diff in this table.
#[test]
fn scalar_resolution_is_the_yaml_1_2_core_schema() {
    for (written, expected) in [
        // The 1.1 booleans that are words in 1.2.
        ("no", Value::String("no".into())),
        ("yes", Value::String("yes".into())),
        ("on", Value::String("on".into())),
        ("off", Value::String("off".into())),
        ("y", Value::String("y".into())),
        ("n", Value::String("n".into())),
        ("Y", Value::String("Y".into())),
        // The core schema names exactly three spellings per boolean, and a
        // fourth casing is a word.
        ("TrUe", Value::String("TrUe".into())),
        // Sexagesimal, leading-zero octal, digit separators, dates.
        ("12:34:56", Value::String("12:34:56".into())),
        ("0777", Value::String("0777".into())),
        ("1_000", Value::String("1_000".into())),
        ("2026-07-29", Value::String("2026-07-29".into())),
        // What the core schema does name.
        ("true", Value::Bool(true)),
        ("True", Value::Bool(true)),
        ("TRUE", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("null", Value::Null),
        ("~", Value::Null),
        ("42", Value::Int(42)),
        ("-42", Value::Int(-42)),
        ("0x1f", Value::Int(31)),
        ("0o17", Value::Int(15)),
        ("1e3", Value::Float(1000.0)),
        ("1.5", Value::Float(1.5)),
        (".inf", Value::Float(f64::INFINITY)),
    ] {
        let map = map_of(&format!("---\na: {written}\n---\n"));
        assert_eq!(map.get("a"), Some(&expected), "reading {written:?}");
    }
}

/// The integer ladder is three rungs, and only the middle one is a diagnostic.
/// Inside `i64` a whole number is an integer; past it but inside `u64` it is
/// carried as a float and said so; outside `u64`, or below `i64`, the block
/// does not parse at all and there is no value to carry.
#[test]
fn an_integer_climbs_a_three_rung_ladder_and_says_which_rung() {
    for (written, expected) in [
        ("9223372036854775807", Value::Int(i64::MAX)),
        ("-9223372036854775808", Value::Int(i64::MIN)),
    ] {
        let source = format!("---\na: {written}\n---\n");
        let document = Document::parse(&source);
        assert_eq!(codes(&document), Vec::<&str>::new(), "reading {written:?}");
        assert_eq!(
            document
                .frontmatter()
                .and_then(Value::as_map)
                .unwrap()
                .get("a"),
            Some(&expected)
        );
    }
    for written in ["9223372036854775808", "18446744073709551615"] {
        let source = format!("---\na: {written}\n---\n");
        let document = Document::parse(&source);
        assert_eq!(
            codes(&document),
            ["frontmatter-integer-out-of-range"],
            "reading {written:?}"
        );
        assert!(matches!(
            document
                .frontmatter()
                .and_then(Value::as_map)
                .and_then(|map| map.get("a")),
            Some(Value::Float(_))
        ));
    }
    for written in ["18446744073709551616", "-9223372036854775809"] {
        let source = format!("---\na: {written}\n---\n");
        let document = Document::parse(&source);
        assert_eq!(
            codes(&document),
            ["frontmatter-parse-failed"],
            "reading {written:?}"
        );
        assert_eq!(document.frontmatter(), None);
    }
}

#[test]
fn the_non_finite_floats_survive_as_floats() {
    let map = map_of("---\na: .nan\nb: .inf\nc: -.inf\n---\n");
    assert!(matches!(map.get("a"), Some(Value::Float(f)) if f.is_nan()));
    assert_eq!(map.get("b"), Some(&Value::Float(f64::INFINITY)));
    assert_eq!(map.get("c"), Some(&Value::Float(f64::NEG_INFINITY)));
}

/// A key written twice is refused rather than stripped: the block is read by
/// nothing, so there is no value for a last entry to win in.
#[test]
fn duplicate_keys_are_a_parse_failure_not_a_silent_last_wins() {
    let document = Document::parse("---\na: 1\na: 2\n---\n");
    assert_eq!(codes(&document), ["frontmatter-parse-failed"]);
    assert_eq!(document.frontmatter(), None);
}

/// An anchor and its alias are expanded by the parser before a value exists,
/// so the model holds the expansion and there is nothing to strip. The marker
/// bytes are still never rewritten: neither value carries a span.
#[test]
fn an_anchor_and_its_alias_read_as_the_expanded_value() {
    let source = "---\na: &x 1\nb: *x\n---\n";
    let document = Document::parse(source);
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map(
            [("a", Value::Int(1)), ("b", Value::Int(1))]
                .into_iter()
                .collect()
        ))
    );
    assert!(document.diagnostics().is_empty());
    assert_eq!(document.field("a").expect("a").value_range, None);
    assert_eq!(document.field("b").expect("b").value_range, None);
}

/// A merge key is a directive, not a field. It is expanded before a value
/// exists, so the model holds the merged mapping and `<<` is never a key in
/// it — carrying one would be a field naming a document construct rather than
/// a document's data.
#[test]
fn a_merge_key_is_expanded_and_never_becomes_a_field() {
    let source = "---\n<<: {a: 1}\nb: 2\n---\n";
    let document = Document::parse(source);
    let map = map_of(source);
    assert!(!map.contains_key("<<"));
    assert_eq!(map.get("a"), Some(&Value::Int(1)));
    assert_eq!(map.get("b"), Some(&Value::Int(2)));
    assert!(
        !document.fields().iter().any(|field| field.name == "<<"),
        "the merge key is not a field"
    );
    // Expansion itself is silent: the only thing said about the block is that
    // its per-field split cannot be trusted, which is why every field edit in
    // it refuses.
    assert_eq!(codes(&document), ["frontmatter-not-editable"]);
}

/// An alias merge expands the same way, at any depth.
#[test]
fn a_merge_key_expands_through_an_alias_and_inside_a_nested_mapping() {
    let map = map_of("---\nbase: &b\n  a: 1\nouter:\n  <<: *b\n  c: 3\n---\n");
    assert_eq!(
        map.get("outer"),
        Some(&Value::Map(
            [("c", Value::Int(3)), ("a", Value::Int(1))]
                .into_iter()
                .collect()
        ))
    );
}

/// A merge source that itself carries a merge expands child-first, so the
/// outer expansion folds in fully-expanded keys and `<<` survives as a field
/// at no depth.
#[test]
fn a_merge_source_carrying_its_own_merge_expands_without_a_phantom_key() {
    let map = map_of("---\ninner: &i\n  p: 1\nmid: &m\n  <<: *i\n  q: 2\ntitle: t\n<<: *m\n---\n");
    let keys: Vec<&str> = map.keys().collect();
    assert_eq!(keys, ["inner", "mid", "title", "q", "p"]);
    assert_eq!(
        map.get("mid"),
        Some(&Value::Map(
            [("q", Value::Int(2)), ("p", Value::Int(1))]
                .into_iter()
                .collect()
        ))
    );
}

/// A `<<` naming something that is not a mapping is not a merge, and there is
/// no honest reading of it: expanding is impossible and carrying it as a field
/// is the phantom the expansion exists to prevent. The block is refused.
#[test]
fn a_merge_key_that_names_no_mapping_refuses_the_block() {
    let document = Document::parse("---\n<<: 1\ntitle: t\n---\n");
    assert_eq!(codes(&document), ["frontmatter-parse-failed"]);
    assert_eq!(document.frontmatter(), None);
}

/// A tag makes `<<` no less the directive — a tag names no part of a key — but
/// a tag the parser keeps makes it one the fold cannot remove: the directive is
/// removed by name and a tagged key is not that name, so the entry survives
/// expansion and reaches the model as the phantom `<<` field the expansion
/// exists to prevent. The block is refused instead.
#[test]
fn a_tagged_merge_key_refuses_the_block() {
    for source in [
        "---\nbase: &b\n  x: 1\n!x <<: *b\n---\nbody\n",
        "---\nbase: &b\n  x: 1\nouter:\n  !x <<: *b\n---\nbody\n",
    ] {
        let document = Document::parse(source);
        assert_eq!(
            document.frontmatter_refusal(),
            Some(&BlockRefusal::Unreadable {
                problem: "a merge key carries no tag, but this one is written `!x <<`".to_string()
            }),
            "{source:?}"
        );
        assert_eq!(codes(&document), ["frontmatter-parse-failed"], "{source:?}");
        assert_eq!(document.frontmatter(), None, "{source:?}");
        assert_eq!(document.body(), "body\n", "{source:?}");
    }
}

/// A tag the parser resolves — `!!merge`, `!!str`, or the verbatim form of
/// either — is gone before a value exists, so the directive it was written on
/// is a plain one to the fold and folds like one. The spelling survives in the
/// block's text alone, which is where the per-field split reads past it: the
/// line bounds the entry above it and no field's range absorbs it, exactly as
/// for a directive written bare.
#[test]
fn a_merge_key_under_a_resolved_tag_folds_like_a_bare_one() {
    // The anchor is on `anchor:`, so the directive does not depend on `base`
    // and has to outlive its removal.
    let bare = "---\nanchor: &b\n  keep: 1\nkeep: 0\nbase: 5\n<<: *b\n---\nbody\n";
    for spelling in ["!!merge", "!!str", "!<tag:yaml.org,2002:merge>"] {
        let source = bare.replace("\n<<: *b", &format!("\n{spelling} <<: *b"));
        let document = Document::parse(&source);
        assert_eq!(document.frontmatter_refusal(), None, "{source:?}");
        assert!(
            !document.fields().iter().any(|field| field.name == "<<"),
            "{source:?}"
        );
        assert_eq!(
            document.remove_field("base"),
            Ok(source.replace("base: 5\n", "")),
            "{source:?}"
        );
    }
}

/// A quoted key is a name, not a spelling: the parser keys the mapping by the
/// bytes inside the quotes and no tag is written there. `"<<"` is the
/// directive, and a quoted key that merely reads like a tagged one is an
/// ordinary field of that exact name.
#[test]
fn a_quoted_key_that_reads_like_a_tagged_directive_is_a_field() {
    let source = "---\n\"!!merge <<\": v\nbase: 5\n---\nbody\n";
    let document = Document::parse(source);
    assert_eq!(
        document
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["!!merge <<", "base"]
    );
    assert_eq!(
        document.remove_field("base"),
        Ok("---\n\"!!merge <<\": v\n---\nbody\n".to_string())
    );
}

/// A merge line belongs to no field, so no field's range may absorb it. It
/// bounds the entry above it instead, and the block it sits in refuses every
/// field edit — reading is unaffected and the bytes round-trip.
#[test]
fn a_merge_line_is_never_absorbed_into_a_neighbouring_field() {
    // The merge contributes nothing new here, so every parsed key is locatable
    // and the entry above the merge line stops at it rather than swallowing it.
    let source = "---\nbase: &b {title: x}\n<<: *b\ntitle: t\n---\n";
    let document = Document::parse(source);
    let base = document.field("base").expect("base is a field");
    assert_eq!(&source[base.line_range.clone()], "base: &b {title: x}\n");
    assert!(!document.fields().iter().any(|field| field.name == "<<"));
}

/// Every construct the value model has no shape for is resolved at the parse
/// boundary, and each is held to both halves of the same bargain: something is
/// said about it — a diagnostic, or an expansion — and the construct itself is
/// absent from the model afterwards. A class that only satisfies the first
/// half leaves a phantom in the value; a class that only satisfies the second
/// drops a document's content in silence.
#[test]
fn every_stripped_class_is_both_reported_and_absent() {
    // A non-string key: diagnosed, and no key of that spelling in the model.
    let dropped = Document::parse("---\n1: x\nname: z\n---\n");
    assert!(
        dropped
            .diagnostics()
            .iter()
            .any(|d| d.code == DiagnosticCode::FrontmatterNonStringKey)
    );
    let map = dropped
        .frontmatter()
        .and_then(Value::as_map)
        .expect("a map");
    assert_eq!(map.keys().collect::<Vec<_>>(), ["name"]);

    // A tag: diagnosed, and no tag survives anywhere in the value — the value
    // it wrapped is what the model holds.
    let tagged = Document::parse("---\na: !foo bar\n---\n");
    assert!(
        tagged
            .diagnostics()
            .iter()
            .any(|d| d.code == DiagnosticCode::FrontmatterTagStripped)
    );
    assert_eq!(
        tagged.frontmatter(),
        Some(&Value::Map([("a", "bar")].into_iter().collect()))
    );

    // A merge key: expanded rather than diagnosed, which is the other way of
    // saying something about it, and `<<` is not in the model.
    let merged = Document::parse("---\n<<: {a: 1}\nb: 2\n---\n");
    let map = merged.frontmatter().and_then(Value::as_map).expect("a map");
    assert!(!map.contains_key("<<"));
    assert_eq!(map.get("a"), Some(&Value::Int(1)));
    // The expansion is loud in the other currency: the block's fields are
    // withheld, and the diagnostic says so.
    assert!(
        merged
            .diagnostics()
            .iter()
            .any(|d| d.code == DiagnosticCode::FrontmatterNotEditable)
    );
    assert!(merged.fields().is_empty());
}

/// A column-0 `---` inside a block scalar ends the frontmatter block, and the
/// rest of the scalar becomes body. YAML reads `---` at column 0 as a document
/// boundary, so this is a defensible read of an ambiguous document rather than
/// a bug — but it is silent, and what it costs is content.
///
/// It is pinned as characterized, not fixed. A heuristic warning here would
/// guess about a document nobody has complained about; noticing that a
/// document lost content belongs to a health scan that can see the vault, not
/// to the grammar that can see one string.
#[test]
fn a_column_zero_fence_inside_a_block_scalar_truncates_the_block() {
    let source = "---\ntext: |\n  one\n---\n  two\nother: x\n---\nbody\n";
    let document = Document::parse(source);
    assert_eq!(
        document.frontmatter(),
        Some(&Value::Map(
            [("text", Value::String("one\n".into()))]
                .into_iter()
                .collect()
        ))
    );
    // Everything past the first column-0 fence is body, `other` included.
    assert_eq!(document.body(), "  two\nother: x\n---\nbody\n");
    assert!(document.diagnostics().is_empty());
    // Indent the fence and it is part of the scalar, which is what an author
    // who meant it to be content writes.
    let indented = Document::parse("---\ntext: |\n  one\n  ---\n  two\n---\nbody\n");
    assert_eq!(
        indented.frontmatter(),
        Some(&Value::Map(
            [("text", Value::String("one\n---\ntwo\n".into()))]
                .into_iter()
                .collect()
        ))
    );
}

// ── Field spans ──────────────────────────────────────────────────────────

fn styles(source: &str) -> Vec<(String, ValueStyle, Option<String>)> {
    Document::parse(source)
        .fields()
        .iter()
        .map(|field| {
            (
                field.name.clone(),
                field.style,
                field
                    .value_range
                    .clone()
                    .map(|range| source[range].to_string()),
            )
        })
        .collect()
}

#[test]
fn a_scalar_value_span_holds_exactly_the_value_bytes() {
    let source = "---\nplain: hello world\nsingle: 'it''s'\ndouble: \"a\\tb\"\n---\n";
    assert_eq!(
        styles(source),
        [
            (
                "plain".to_string(),
                ValueStyle::Plain,
                Some("hello world".to_string())
            ),
            (
                "single".to_string(),
                ValueStyle::SingleQuoted,
                Some("'it''s'".to_string())
            ),
            (
                "double".to_string(),
                ValueStyle::DoubleQuoted,
                Some("\"a\\tb\"".to_string())
            ),
        ]
    );
}

#[test]
fn a_plain_value_span_stops_before_a_trailing_comment_and_its_padding() {
    let source = "---\ntitle: hello   # a note\n---\n";
    assert_eq!(
        styles(source),
        [(
            "title".to_string(),
            ValueStyle::Plain,
            Some("hello".to_string())
        )]
    );
}

#[test]
fn a_hash_that_is_not_preceded_by_space_is_part_of_the_value() {
    let source = "---\ntitle: a#b\n---\n";
    assert_eq!(
        styles(source),
        [(
            "title".to_string(),
            ValueStyle::Plain,
            Some("a#b".to_string())
        )]
    );
}

#[test]
fn a_value_holding_a_colon_is_spanned_precisely() {
    let source = "---\ntime: 12:30\n---\n";
    assert_eq!(
        styles(source),
        [(
            "time".to_string(),
            ValueStyle::Plain,
            Some("12:30".to_string())
        )]
    );
}

#[test]
fn a_multibyte_value_slices_to_the_whole_string() {
    let source = "---\ntitle: héllo wörld\n---\n";
    assert_eq!(
        styles(source),
        [(
            "title".to_string(),
            ValueStyle::Plain,
            Some("héllo wörld".to_string())
        )]
    );
}

/// Every structural style that cannot be named as one span on the key line
/// declines a value span and keeps its key visible, so a whole-entry remove
/// still works.
#[test]
fn structural_values_decline_a_value_span() {
    for (source, expected) in [
        ("---\nk: |\n  literal\n---\n", ValueStyle::BlockLiteral),
        ("---\nk: |+\n  literal\n---\n", ValueStyle::BlockLiteral),
        ("---\nk: >\n  folded\n---\n", ValueStyle::BlockFolded),
        ("---\nk: >-\n  folded\n---\n", ValueStyle::BlockFolded),
        ("---\nk: [a, b]\n---\n", ValueStyle::FlowSequence),
        ("---\nk: {a: 1}\n---\n", ValueStyle::FlowMapping),
        ("---\nk:\n  - a\n---\n", ValueStyle::BlockSequence),
        ("---\nk:\n  a: 1\n---\n", ValueStyle::BlockMapping),
    ] {
        let document = Document::parse(source);
        let field = document.field("k").expect("the key stays visible");
        assert_eq!(field.style, expected, "for {source:?}");
        assert_eq!(field.value_range, None, "for {source:?}");
        assert_eq!(
            &source[field.line_range.clone()],
            &source[4..source.len() - 4]
        );
    }
}

/// A literal null token is editable in place; a `# comment` re-parses to null
/// and must not be mistaken for one.
#[test]
fn a_null_token_is_editable_and_a_comment_that_reads_as_null_is_not() {
    let source = "---\nexplicit: ~\ncommented: # nothing here\n---\n";
    let document = Document::parse(source);
    let explicit = document.field("explicit").expect("explicit");
    assert_eq!(
        explicit.value_range.clone().map(|range| &source[range]),
        Some("~")
    );
    // A key followed only by a comment is a stub, not a field whose value is
    // the comment. The insertion point sits between the colon and the comment,
    // so writing the field leaves the comment standing.
    let commented = document.field("commented").expect("commented");
    assert_eq!(commented.style, ValueStyle::EmptyValue);
    let range = commented.value_range.clone().expect("an insertion point");
    assert_eq!(&source[range.clone()], "");
    assert_eq!(&source[..range.start], "---\nexplicit: ~\ncommented:");
    assert_eq!(
        Document::parse(source).set_field("commented", &Value::String("draft".into())),
        Ok("---\nexplicit: ~\ncommented: draft # nothing here\n---\n".to_string())
    );
}

#[test]
fn a_quoted_or_numeric_key_is_visible_under_the_name_the_parser_uses() {
    for (source, name) in [
        ("---\n\"key with spaces\": 1\n---\n", "key with spaces"),
        ("---\n'it''s': 1\n---\n", "it's"),
        ("---\n123: 1\n---\n", "123"),
    ] {
        let document = Document::parse(source);
        // A numeric key is dropped by the value model, so only the quoted keys
        // survive as fields; both spellings must at least agree with the
        // parser about the name.
        if let Some(Value::Map(map)) = document.frontmatter()
            && map.contains_key(name)
        {
            assert!(document.field(name).is_some(), "for {source:?}");
        }
    }
}

#[test]
fn a_column_zero_comment_is_not_a_key() {
    let document = Document::parse("---\n# not: a key\ntitle: hello\n---\n");
    assert_eq!(
        document
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["title"]
    );
}

#[test]
fn an_indented_line_is_not_a_top_level_key() {
    let document = Document::parse("---\nouter:\n  inner: 1\n---\n");
    assert_eq!(
        document
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["outer"]
    );
}
