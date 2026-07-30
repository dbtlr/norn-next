//! The lossless round trip, as properties over whole documents.
//!
//! Three claims, each checked over the same two corpora — documents authored
//! to be hostile, and documents a fixture profile generated:
//!
//! 1. **Reading loses nothing.** A document reassembles from what reading
//!    reports about it, byte for byte.
//! 2. **Writing a value back is a no-op.** Setting a field to what it already
//!    holds reproduces the document byte for byte, so an edit is idempotent
//!    and a no-change write is not a diff.
//! 3. **An edit is a one-construct diff.** Setting a field moves the value
//!    span and nothing else; removing one moves the entry and nothing else.

use norn_testkit::generated;
use norn_text::{Document, Value};

/// Documents written to defeat a byte scanner, a quoting denylist, or both.
const ADVERSARIAL: &[&str] = &[
    "",
    "no frontmatter at all\n",
    "\u{feff}no frontmatter, with a mark\n",
    "---\n---\n",
    "---\n---\nbody\n",
    "---\ntitle: hello\n---\n",
    "---\ntitle: hello\n---\n# body\n\nprose [[Link]] and `code`\n",
    "---\r\ntitle: hello\r\ntags:\r\n  - a\r\n---\r\nbody\r\n",
    "\u{feff}---\ntitle: hello\n---\nbody\n",
    "---\ntitle: hello\n\n# a standing comment\nstatus: draft\n---\nbody\n",
    "---\ntitle: hello   # trailing note\n---\n",
    "---\nplain: hello\nsingle: 'it''s'\ndouble: \"a\\tb\"\n---\n",
    "---\nliteral: |\n  one\n  two\nfolded: >-\n  three\n\n  four\n---\n",
    "---\nflow: [a, \"b,c\", 'd]e']\nblock:\n  - one\n  - two\n---\n",
    "---\nnested:\n  inner: 1\n  deeper:\n    leaf: x\n---\n",
    "---\nempty:\nnulled: ~\nzero: 0\nfloat: 1.10\nbool: yes\n---\n",
    "---\nitems:\n- name: one\n- name: two\ntitle: t\n---\n",
    "---\nnote: \"first\nkey: not a key\"\ntitle: t\n---\n",
    "---\nanchored: &base\n  k: 1\nreferenced: *base\n---\n",
    "---\na: &x 1\nb: *x\n---\nbody\n",
    "---\n<<: {a: 1}\ntitle: t\n---\n",
    "---\nbase: &b {title: x}\n<<: *b\ntitle: t\n---\n",
    "---\nratio: .nan\nup: .inf\ndown: -.inf\ntitle: t\n---\nbody\n",
    "---   \ntitle: loose opener\n---\nbody\n",
    "---\ntitle: loose closer\n---   \nbody\n",
    "---\t\ntitle: both loose\n---\t\nbody\n",
    "---\r\ntitle: t\r\n---\r\n## Alpha\r\n\r\none\r\ntwo\r\n\r\n## Beta\r\nb\r\n",
    "---\ntext: |+\n  a\n\n# a standing comment\nother: x\n---\n",
    "---\ntext: | # a note with a + in it\n  a\n\n# a standing comment\nother: x\n---\n",
    "---\ntitle: t\n---\nAlpha\n=====\n\nbody\n\n## Beta\n\n^block-id\n",
    "---\ntitle: t\n---\n## Tail",
    "---\nunclosed: hello\nno closing fence\n",
    "---\nbroken: : :\n---\nbody\n",
    "---\n1: not a string key\ntitle: t\n---\n",
    "---\n- a root sequence\n- second\n---\nbody\n",
    "---\ntitle: t\n---\n\n\n\n",
    "---\ntitle: t\n---",
];

fn generated_documents() -> Vec<String> {
    let mut documents = Vec::new();
    for (profile, seed) in [("tiny", 7_u64), ("small", 11), ("ambiguous", 13)] {
        let generated = generated::documents(profile, seed)
            .unwrap_or_else(|error| panic!("generating `{profile}`: {error}"));
        assert!(
            !generated.is_empty(),
            "`{profile}` generated no documents, so this suite would assert nothing"
        );
        documents.extend(generated.into_iter().map(|document| document.text));
    }
    documents
}

/// Reading reports the block's range, the body, and where the body begins.
/// Those three have to reassemble the document exactly, or something read has
/// been lost.
fn reading_loses_nothing(source: &str, label: &str) -> usize {
    let document = Document::parse(source);
    assert_eq!(
        format!("{}{}", &source[..document.body_start()], document.body()),
        source,
        "{label}: the body and its offset do not reassemble the document"
    );
    if let Some(range) = document.frontmatter_range() {
        assert!(
            range.end <= source.len(),
            "{label}: the block range overruns"
        );
        let opener = source[..range.start]
            .trim_end_matches(['\r', '\n'])
            .trim_end_matches([' ', '\t']);
        assert!(
            opener.ends_with("---"),
            "{label}: the block does not begin after an opening fence"
        );
    }
    let mut checked = 1;
    for field in document.fields() {
        checked += 1;
        let range = document
            .frontmatter_range()
            .expect("a field implies a block");
        assert!(
            range.start <= field.line_range.start && field.line_range.end <= range.end,
            "{label}: field {:?} runs outside the block",
            field.name
        );
        if let Some(value) = &field.value_range {
            assert!(
                field.line_range.start <= value.start && value.end <= field.line_range.end,
                "{label}: field {:?} has a value outside its own entry",
                field.name
            );
        }
    }
    checked
}

/// Writing a field the value it already holds is a no-op on the bytes. It has
/// to be, or every write is a diff and no edit is idempotent.
fn writing_a_value_back_changes_nothing(source: &str, label: &str) -> usize {
    let document = Document::parse(source);
    let Some(Value::Map(map)) = document.frontmatter() else {
        return 0;
    };
    let mut checked = 0;
    for field in document.fields() {
        let Some(current) = map.get(&field.name) else {
            continue;
        };
        // Only the values a span can name are written in place; the rest are
        // refused, which the editing suite states.
        if field.value_range.is_none() || !matches!(current, Value::String(_)) {
            continue;
        }
        assert_eq!(
            document.set_field(&field.name, current).as_deref(),
            Ok(source),
            "{label}: rewriting {:?} with its own value changed the document",
            field.name
        );
        checked += 1;
    }
    checked
}

/// An accepted edit moves the bytes of the construct it addresses and no
/// others, and the document it produces reads back as the one that was asked
/// for.
fn an_edit_is_a_one_construct_diff(source: &str, label: &str) -> usize {
    let document = Document::parse(source);
    let Some(Value::Map(map)) = document.frontmatter() else {
        return 0;
    };
    let mut checked = 0;
    for field in document.fields() {
        if let (Some(range), Ok(edited)) = (
            field.value_range.clone(),
            document.set_field(&field.name, &Value::String("sentinel".into())),
        ) {
            assert_eq!(
                &edited[..range.start],
                &source[..range.start],
                "{label}: setting {:?} moved bytes before the value",
                field.name
            );
            let tail = source.len() - range.end;
            assert_eq!(
                &edited[edited.len() - tail..],
                &source[range.end..],
                "{label}: setting {:?} moved bytes after the value",
                field.name
            );
            checked += 1;
        }

        if let Ok(edited) = document.remove_field(&field.name) {
            let line = field.line_range.clone();
            assert_eq!(
                edited,
                format!("{}{}", &source[..line.start], &source[line.end..]),
                "{label}: removing {:?} moved bytes outside its entry",
                field.name
            );
            let mut expected = map.clone();
            expected.remove(&field.name);
            let reread = Document::parse(&edited);
            match reread.frontmatter() {
                Some(Value::Map(after)) => assert_eq!(
                    after, &expected,
                    "{label}: removing {:?} changed another field",
                    field.name
                ),
                Some(Value::Null) => assert!(
                    expected.is_empty(),
                    "{label}: removing {:?} emptied a block that still had fields",
                    field.name
                ),
                other => panic!("{label}: removing {:?} produced {other:?}", field.name),
            }
            checked += 1;
        }
    }
    checked
}

/// How many frontmatter blocks a document holds, counted by reading the body
/// of each for another one.
///
/// A document has one block or none. Two is the shape a fence the reader did
/// not recognize produces: the fields go into a block synthesized above the
/// real one, and the re-read finds the synthesized block first and approves an
/// edit that shadowed everything the document said.
fn frontmatter_blocks(source: &str) -> usize {
    let document = Document::parse(source);
    if document.frontmatter_range().is_none() {
        return 0;
    }
    1 + frontmatter_blocks(document.body())
}

/// How many lines of `source` are `---` and nothing else but whitespace.
///
/// Counted by looking at the text rather than by asking this crate, which is
/// the point: the block count above is blind to a fence the reader does not
/// recognize, and that is exactly the case where an edit synthesizes a second
/// block. A count that reads the bytes sees the fences either way.
fn fence_lines(source: &str) -> usize {
    source
        .trim_start_matches('\u{feff}')
        .lines()
        .filter(|line| line.trim_end_matches(['\r', ' ', '\t']) == "---")
        .count()
}

fn headings_of(source: &str) -> Vec<(u8, String)> {
    Document::parse(source)
        .scan_body()
        .headings()
        .iter()
        .map(|heading| (heading.level, heading.text.clone()))
        .collect()
}

/// An accepted field edit leaves exactly one frontmatter block — the one it
/// wrote into, or the one it synthesized for a document that had none — writes
/// a pair of fences only into a document that had none, and leaves every
/// heading the body already had.
///
/// All three are corpus-wide because all three are ways an edit corrupts a
/// document without failing any per-field assertion: a second block shadows
/// the real one, a pair of fences written above a fence the reader missed is
/// how the second block gets there, and a heading that stopped parsing means
/// the body's structure moved under an edit that was only supposed to touch
/// the block.
fn an_accepted_edit_keeps_one_block_and_every_heading(source: &str, label: &str) -> usize {
    let document = Document::parse(source);
    let headings_before = headings_of(source);
    let fences_before = fence_lines(source);
    let mut checked = 0;
    let judge = |edited: &str, what: &str| {
        assert_eq!(
            frontmatter_blocks(edited),
            1,
            "{label}: {what} left {} frontmatter blocks",
            frontmatter_blocks(edited)
        );
        let fences_after = fence_lines(edited);
        let synthesized = fences_after.saturating_sub(fences_before);
        assert!(
            synthesized == 0 || (synthesized == 2 && fences_before == 0),
            "{label}: {what} wrote {synthesized} fence lines into a document that had \
             {fences_before}"
        );
        let after = headings_of(edited);
        for heading in &headings_before {
            assert!(
                after.contains(heading),
                "{label}: {what} lost the heading {heading:?}"
            );
        }
    };
    for name in ["title", "status", "a brand new field"] {
        if let Ok(edited) = document.set_field(name, &Value::String("sentinel".into())) {
            judge(&edited, "setting a field");
            checked += 1;
        }
    }
    for field in document.fields() {
        if let Ok(edited) = document.remove_field(&field.name) {
            judge(&edited, "removing a field");
            checked += 1;
        }
    }
    checked
}

/// Replacing a section with what it already holds is a no-op too.
fn writing_a_section_back_changes_nothing(source: &str, label: &str) -> usize {
    let document = Document::parse(source);
    let scan = document.scan_body();
    let mut seen: Vec<&str> = Vec::new();
    let mut checked = 0;
    for heading in scan.headings() {
        if seen.contains(&heading.text.as_str()) {
            continue;
        }
        seen.push(&heading.text);
        let Ok(span) = scan.resolve_section(heading.text.as_str().into()) else {
            continue;
        };
        let content = &document.body()[span.content_start..span.content_end];
        assert_eq!(
            document
                .replace_section(heading.text.as_str(), content)
                .as_deref(),
            Ok(source),
            "{label}: rewriting section {:?} with its own content changed the document",
            heading.text
        );
        checked += 1;
    }
    checked
}

/// A property loop that iterated over nothing would pass, so each of these
/// counts what it actually checked and states a floor. The floors are the
/// corpus as it stands: adding a document raises what is checked, and deleting
/// the ones that carry a property trips the count rather than going quiet.
#[test]
fn adversarial_documents_reassemble_from_what_reading_reports() {
    let checked: usize = ADVERSARIAL
        .iter()
        .map(|source| reading_loses_nothing(source, &format!("{source:?}")))
        .sum();
    assert!(checked >= 60, "only {checked} reassembly checks ran");
}

#[test]
fn adversarial_documents_survive_a_write_of_what_they_already_say() {
    let mut values = 0;
    let mut sections = 0;
    for source in ADVERSARIAL {
        let label = format!("{source:?}");
        values += writing_a_value_back_changes_nothing(source, &label);
        sections += writing_a_section_back_changes_nothing(source, &label);
    }
    assert!(values >= 15, "only {values} value rewrites ran");
    assert!(sections >= 3, "only {sections} section rewrites ran");
}

#[test]
fn adversarial_document_edits_are_one_construct_diffs() {
    let checked: usize = ADVERSARIAL
        .iter()
        .map(|source| an_edit_is_a_one_construct_diff(source, &format!("{source:?}")))
        .sum();
    assert!(checked >= 40, "only {checked} one-construct edits ran");
}

#[test]
fn adversarial_document_edits_keep_one_block_and_every_heading() {
    let checked: usize = ADVERSARIAL
        .iter()
        .map(|source| {
            an_accepted_edit_keeps_one_block_and_every_heading(source, &format!("{source:?}"))
        })
        .sum();
    assert!(checked >= 60, "only {checked} accepted edits were judged");
}

/// The same three properties over documents nobody hand-picked. The generator
/// is independent of this crate — it links nothing here and shares no parser —
/// so the trees it writes are inputs rather than a mirror of these
/// expectations.
#[test]
fn generated_documents_hold_the_same_properties() {
    let documents = generated_documents();
    assert!(
        documents.len() > 50,
        "the corpora are too small to say much"
    );
    let mut checked = 0;
    for (index, source) in documents.iter().enumerate() {
        let label = format!("generated document {index}");
        checked += reading_loses_nothing(source, &label);
        checked += writing_a_value_back_changes_nothing(source, &label);
        checked += an_edit_is_a_one_construct_diff(source, &label);
        checked += an_accepted_edit_keeps_one_block_and_every_heading(source, &label);
        checked += writing_a_section_back_changes_nothing(source, &label);
    }
    assert!(
        checked > 1000,
        "only {checked} checks ran over the generated corpora"
    );
}

/// A generated document is well-formed, so reading one has nothing to work
/// around and every field it declares is editable. A diagnostic here means the
/// reader and the generator disagree about ordinary Markdown.
#[test]
fn generated_documents_read_without_a_single_diagnostic() {
    for (index, source) in generated_documents().iter().enumerate() {
        let document = Document::parse(source);
        assert_eq!(
            document.diagnostics(),
            &[],
            "generated document {index} was not read cleanly"
        );
        let map = document
            .frontmatter()
            .and_then(Value::as_map)
            .unwrap_or_else(|| panic!("generated document {index} has no frontmatter mapping"));
        assert_eq!(
            document.fields().len(),
            map.len(),
            "generated document {index} has fields the span layer could not locate"
        );
    }
}
