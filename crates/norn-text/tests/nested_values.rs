//! Writing the whole value tree: maps, lists of maps and lists of lists, not
//! just scalars and flat lists of scalars.
//!
//! A nested value is written in block style, two spaces deeper per level, and
//! an empty collection as `[]` or `{}`. Every scalar inside it is quoted only
//! as far as its own context demands, and every write is proven by reading it
//! back, so a value that would not read back as itself is refused rather than
//! written.

use norn_text::{
    Document, EditError, LineEnding, Mapping, Value, frontmatter_reads_back, render_document,
};

fn map<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Map(entries.into_iter().collect())
}

fn list<const N: usize>(items: [Value; N]) -> Value {
    Value::Sequence(items.into())
}

fn set(source: &str, field: &str, value: &Value) -> Result<String, EditError> {
    Document::parse(source).set_field(field, value)
}

/// The value `field` reads as in `document`.
fn read(document: &str, field: &str) -> Value {
    Document::parse(document)
        .frontmatter()
        .and_then(Value::as_map)
        .and_then(|fields| fields.get(field))
        .cloned()
        .unwrap_or_else(|| panic!("{field:?} reads back from {document:?}"))
}

// ── Rendering ────────────────────────────────────────────────────────────

/// **A map is written in block style, each level two spaces deeper.**
#[test]
fn a_map_renders_as_an_indented_block() {
    let fields: Mapping = [(
        "meta",
        map([
            ("author", "ada".into()),
            ("draft", map([("rev", Value::Int(2))])),
        ]),
    )]
    .into_iter()
    .collect();
    assert_eq!(
        render_document(&fields, "", LineEnding::Lf),
        Ok("---\nmeta:\n  author: ada\n  draft:\n    rev: 2\n---\n".to_string())
    );
}

/// **A map inside a list shares its item's `-` line with its first entry**,
/// and the rest of it sits under that entry.
#[test]
fn a_list_of_maps_renders_each_map_as_one_item() {
    let fields: Mapping = [(
        "people",
        list([
            map([("name", "ada".into()), ("role", "author".into())]),
            map([("name", "bo".into())]),
        ]),
    )]
    .into_iter()
    .collect();
    assert_eq!(
        render_document(&fields, "", LineEnding::Lf),
        Ok("---\npeople:\n  - name: ada\n    role: author\n  - name: bo\n---\n".to_string())
    );
}

/// **A list inside a list shares its item's `-` line too**: `- - a`.
#[test]
fn a_nested_list_renders_on_its_items_dash_line() {
    let fields: Mapping = [(
        "grid",
        list([list(["a".into(), "b".into()]), list([list(["c".into()])])]),
    )]
    .into_iter()
    .collect();
    let rendered = render_document(&fields, "", LineEnding::Lf);
    assert_eq!(
        rendered,
        Ok("---\ngrid:\n  - - a\n    - b\n  - - - c\n---\n".to_string())
    );
    assert_eq!(
        read(&rendered.unwrap(), "grid"),
        fields.get("grid").cloned().unwrap()
    );
}

/// **An empty collection is written inline, `[]` or `{}`, at the top level
/// and nested alike**: a bare key reads back as null, and a bare `-` too.
#[test]
fn an_empty_map_or_list_is_written_inline_wherever_it_sits() {
    let fields: Mapping = [
        ("none", list([])),
        ("nothing", map([])),
        ("inner", map([("none", list([])), ("nothing", map([]))])),
        ("items", list([list([]), map([])])),
    ]
    .into_iter()
    .collect();
    let rendered = render_document(&fields, "", LineEnding::Lf).expect("empty collections");
    assert_eq!(
        rendered,
        "---\nnone: []\nnothing: {}\ninner:\n  none: []\n  nothing: {}\nitems:\n  - []\n  - {}\n---\n"
    );
    assert!(frontmatter_reads_back(&rendered, &Value::Map(fields)));
}

/// **A nested key is quoted exactly as far as a field name is**: one that
/// would read as a comment, a nested mapping, an item, an empty key, null or
/// a number is quoted, and a plain one is not.
#[test]
fn a_nested_key_needing_quotes_is_quoted_and_reads_back() {
    let inner: Mapping = ["a: b", "#x", "- y", "", "null", "1", "plain"]
        .into_iter()
        .map(|key| (key, Value::String(format!("v{key}"))))
        .collect();
    let fields: Mapping = [
        ("meta", Value::Map(inner.clone())),
        ("rows", list([Value::Map(inner)])),
    ]
    .into_iter()
    .collect();
    let rendered = render_document(&fields, "", LineEnding::Lf).expect("quoted keys");
    assert!(
        rendered.contains("\n  plain: vplain\n") && rendered.contains("\n  'a: b': "),
        "{rendered:?}"
    );
    assert!(
        frontmatter_reads_back(&rendered, &Value::Map(fields)),
        "{rendered:?}"
    );
}

// ── Setting a field ──────────────────────────────────────────────────────

/// **A map set onto a field the block lacks is appended as a block**, and
/// nothing above it moves.
#[test]
fn a_map_set_onto_a_new_field_is_appended_in_block_style() {
    assert_eq!(
        set(
            "---\ntitle: t # kept\n---\nbody\n",
            "meta",
            &map([
                ("k", "v".into()),
                ("rows", list([map([("a", Value::Int(1))])]))
            ])
        ),
        Ok("---\ntitle: t # kept\nmeta:\n  k: v\n  rows:\n    - a: 1\n---\nbody\n".to_string())
    );
}

/// **A set over a field holding a map rewrites that field's whole entry**,
/// and every byte outside it stands.
#[test]
fn a_set_over_a_map_field_rewrites_its_entry_and_nothing_else() {
    let source = "---\ntitle: 'x' # kept\nmeta:\n    old: 1\n    deeper:\n      - a\n# standing\nafter: \"y\"\n---\nbody\n";
    assert_eq!(
        set(source, "meta", &map([("new", "v".into())])),
        Ok(
            "---\ntitle: 'x' # kept\nmeta:\n  new: v\n# standing\nafter: \"y\"\n---\nbody\n"
                .to_string()
        )
    );
    assert_eq!(
        set(
            "---\nmeta: {a: 1}\nafter: y\n---\n",
            "meta",
            &map([("b", Value::Int(2))])
        ),
        Ok("---\nmeta:\n  b: 2\nafter: y\n---\n".to_string())
    );
}

/// **A set over a map field whose entry carries a comment refuses**: the
/// whole entry is rewritten, so the comment would be dropped silently.
#[test]
fn a_set_over_a_commented_map_field_refuses() {
    for source in [
        "---\nmeta: # about\n  k: v\n---\n",
        "---\nmeta:\n  k: v # why\n---\n",
        "---\nmeta:\n  # inside\n  k: v\n---\n",
        "---\nmeta: {k: v} # why\n---\n",
    ] {
        assert_eq!(
            set(source, "meta", &map([("k", "w".into())])),
            Err(EditError::CommentWouldBeLost {
                field: "meta".into()
            }),
            "for {source:?}"
        );
    }
}

// ── Telling a comment from content ───────────────────────────────────────

fn comment_lost(field: &str) -> Result<String, EditError> {
    Err(EditError::CommentWouldBeLost {
        field: field.into(),
    })
}

/// **A comment after a quote that opens nothing is still a comment.** Inside
/// a plain scalar or a block scalar a quote is a literal character, so a
/// later `# comment` holding a matching quote is a comment all the same, and
/// a whole-entry rewrite that would drop it refuses.
#[test]
fn a_comment_after_a_literal_quote_refuses_a_whole_entry_rewrite() {
    for (source, field, value) in [
        (
            "---\nname: Lovelace, 'Ada # don't rename\nn: 1\n---\n",
            "name",
            map([("given", "Ada".into())]),
        ),
        (
            "---\nk: a - 'b # c'\nn: 1\n---\nbody\n",
            "k",
            list(["x".into()]),
        ),
        (
            "---\nk: a ? 'b # c'\nn: 1\n---\nbody\n",
            "k",
            list(["x".into()]),
        ),
        (
            "---\nk: a\n  'b # c'\nn: 1\n---\n",
            "k",
            map([("x", Value::Int(1))]),
        ),
        (
            "---\nmeta:\n  k: |\n    \"hello\n  # keep this \"\n  n: 1\nafter: x\n---\nbody\n",
            "meta",
            map([("n", Value::Int(2))]),
        ),
        (
            "---\nmeta:\n  k: hello - 'world # keep '\n  n: 1\n---\n",
            "meta",
            map([("n", Value::Int(2))]),
        ),
    ] {
        assert_eq!(
            set(source, field, &value),
            comment_lost(field),
            "for {source:?}"
        );
    }
}

/// **A push or a pop that rewrites a flow list whole refuses for a comment
/// after a literal quote**, as a set does.
#[test]
fn a_flow_list_rewrite_refuses_for_a_comment_after_a_literal_quote() {
    let document = Document::parse("---\nk: [x, a - 'b] # c'\n---\n");
    assert_eq!(document.push_to_list("k", &"y".into()), comment_lost("k"));
    assert_eq!(document.pop_from_list("k", &"x".into()), comment_lost("k"));
}

/// **A block list that falls back to a whole rewrite refuses for a comment
/// a block scalar's quote hides**: an alias keeps its items from being
/// spliced one by one, and the comment below the block scalar is the
/// entry's.
#[test]
fn a_block_list_rewrite_refuses_for_a_comment_below_a_block_scalar() {
    let document = Document::parse(
        "---\nrows:\n  - &x\n    k: |\n      \"hello\n    # keep this \"\n    n: 1\n  - *x\n---\n",
    );
    assert_eq!(
        document.push_to_list("rows", &map([("n", Value::Int(2))])),
        comment_lost("rows")
    );
    assert_eq!(
        document.pop_from_list(
            "rows",
            &map([("k", "\"hello\n".into()), ("n", Value::Int(1))])
        ),
        comment_lost("rows")
    );
}

/// **A comment under keep chomping is a comment, whatever blank lines follow
/// it.** A `|+` scalar keeps its trailing blank lines, so the layout around a
/// comment below it is part of the value; the comment itself is not, and a
/// whole-entry rewrite that would drop it refuses.
#[test]
fn a_comment_below_a_keep_chomping_scalar_refuses_a_whole_entry_rewrite() {
    let kept = "---\nk: |+\n    a\n  # c\n\nn: 1\n---\n";
    for (source, value) in [
        (kept, map([("z", Value::Int(9))])),
        (kept, list(["x".into(), list([])])),
        (
            "---\nk:\n  m: |+\n    a\n  # c\n\n  o: 1\nn: 1\n---\n",
            map([("z", Value::Int(9))]),
        ),
        (
            "---\nk: |+\n    a\n\n  # c\n\nn: 1\n---\n",
            map([("z", Value::Int(9))]),
        ),
    ] {
        assert_eq!(
            set(source, "k", &value),
            comment_lost("k"),
            "for {source:?}"
        );
    }
}

/// **A `#` that is content is not a comment**: inside a quoted scalar, on a
/// block scalar's content line, or with no space before it. A whole-entry
/// rewrite over it lands.
#[test]
fn a_hash_that_is_content_does_not_refuse_a_whole_entry_rewrite() {
    let value = map([("x", Value::Int(1))]);
    for source in [
        "---\nk: 'a # b'\nn: 1\n---\n",
        "---\nk: \"a # b\"\nn: 1\n---\n",
        "---\nk: 'it''s # here'\nn: 1\n---\n",
        "---\nk: [a#b, 'c # d']\nn: 1\n---\n",
        "---\nk: |\n  a\n  # not a comment\nn: 1\n---\n",
        "---\nk: a#b\nn: 1\n---\n",
        "---\nk:\n  - 'x '' # y'\nn: 1\n---\n",
    ] {
        assert_eq!(
            set(source, "k", &value),
            Ok("---\nk:\n  x: 1\nn: 1\n---\n".to_string()),
            "for {source:?}"
        );
    }
}

/// **A scalar set over a map, and a map set over a scalar, each rewrite the
/// field's whole entry.**
#[test]
fn a_set_changing_a_field_between_scalar_and_map_rewrites_its_entry() {
    assert_eq!(
        set(
            "---\nmeta:\n  k: v\nafter: y\n---\n",
            "meta",
            &"flat".into()
        ),
        Ok("---\nmeta: flat\nafter: y\n---\n".to_string())
    );
    assert_eq!(
        set(
            "---\nmeta: flat\nafter: y\n---\n",
            "meta",
            &map([("k", "v".into())])
        ),
        Ok("---\nmeta:\n  k: v\nafter: y\n---\n".to_string())
    );
}

// ── Awkward values round-trip ────────────────────────────────────────────

/// Strings that change meaning unquoted, in a key, in a block value or as a
/// list item.
const AWKWARD: &[&str] = &[
    "",
    " lead",
    "trail ",
    "a: b",
    "a:",
    "#x",
    "a #b",
    "- y",
    "-",
    "? q",
    "[x",
    "{x",
    "x]",
    "'q'",
    "\"q\"",
    "it's",
    "two\nlines",
    "tab\there",
    "---",
    "...",
    "%x",
    "@x",
    "`x",
    "*x",
    "&x",
    "!x",
    "|",
    ">",
    "null",
    "~",
    "true",
    "1",
    "1.5",
    "0x1F",
    ".nan",
    "\u{85}nel",
];

/// `value` set over `field` in a block with neighbours on both sides, read
/// back: the value it reads as, and the neighbours as they read.
fn round_trip_set(value: &Value) -> Value {
    let source = "---\nbefore: 'b' # kept\nfield: [seed]\nafter: [a, b]\n---\nbody\n";
    let edited =
        set(source, "field", value).unwrap_or_else(|error| panic!("setting {value:?}: {error}"));
    assert!(
        edited.starts_with("---\nbefore: 'b' # kept\n")
            && edited.ends_with("after: [a, b]\n---\nbody\n"),
        "setting {value:?} moved a neighbour: {edited:?}"
    );
    read(&edited, "field")
}

/// **Every awkward nested value sets and reads back as itself**: strings
/// needing quotes as keys and values inside maps, maps inside lists inside
/// maps, lists of lists, empty collections at depth and the non-string
/// scalars, each written over a flow list and proven by re-reading it.
#[test]
fn awkward_nested_values_round_trip() {
    let strings: Mapping = AWKWARD
        .iter()
        .map(|text| (*text, Value::String((*text).to_string())))
        .collect();
    let items = Value::Sequence(AWKWARD.iter().map(|text| Value::from(*text)).collect());
    let scalars = list([
        Value::Null,
        Value::Bool(false),
        Value::Int(-7),
        Value::Float(-0.0),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Float(1.0),
    ]);
    let table = [
        Value::Map(strings.clone()),
        map([("outer", map([("inner", Value::Map(strings.clone()))]))]),
        map([(
            "rows",
            list([
                Value::Map(strings.clone()),
                map([("deeper", list([map([("leaf", items.clone())])]))]),
            ]),
        )]),
        list([items.clone(), list([items.clone(), list([])])]),
        list([
            map([]),
            list([]),
            map([("a", map([]))]),
            list([list([map([])])]),
        ]),
        list([Value::Map(strings), scalars.clone(), "plain".into()]),
        map([("scalars", scalars), ("nothing", Value::Null)]),
    ];
    for value in &table {
        assert_eq!(&round_trip_set(value), value);
        let fields: Mapping = [("field", value.clone())].into_iter().collect();
        let rendered = render_document(&fields, "", LineEnding::Crlf)
            .unwrap_or_else(|error| panic!("rendering {value:?}: {error}"));
        assert!(
            frontmatter_reads_back(&rendered, &Value::Map(fields)),
            "{rendered:?}"
        );
        assert!(!rendered.replace("\r\n", "").contains('\n'), "{rendered:?}");
    }
}

// ── Pushing and popping a nested element ─────────────────────────────────

/// A block list of maps whose items each span several lines, with comments
/// inside an item, between items and after the list.
const PEOPLE: &str = "---\ntitle: t\npeople:\n- name: ada # first\n  role: author\n# between\n- name: bo\n  # inside\n  role: editor\nafter: x\n---\nbody\n";

/// **A map pushed onto a block list of maps is one item spliced below the
/// last**, at the list's item indent, and every other byte stands — the
/// comments inside and between the items included.
#[test]
fn a_map_pushed_onto_a_block_list_of_maps_splices_one_item() {
    assert_eq!(
        Document::parse(PEOPLE).push_to_list(
            "people",
            &map([("name", "cy".into()), ("role", "reader".into())])
        ),
        Ok(PEOPLE.replace(
            "  role: editor\n",
            "  role: editor\n- name: cy\n  role: reader\n"
        ))
    );
}

/// **A map popped from a block list of maps deletes only that item's lines**,
/// its own comments with them; the other items and the comments between
/// them stand.
#[test]
fn a_map_popped_from_a_block_list_of_maps_deletes_only_its_lines() {
    let ada = map([("name", "ada".into()), ("role", "author".into())]);
    assert_eq!(
        Document::parse(PEOPLE).pop_from_list("people", &ada),
        Ok(PEOPLE.replace("- name: ada # first\n  role: author\n", ""))
    );
    let bo = map([("name", "bo".into()), ("role", "editor".into())]);
    assert_eq!(
        Document::parse(PEOPLE).pop_from_list("people", &bo),
        Ok(PEOPLE.replace("- name: bo\n  # inside\n  role: editor\n", ""))
    );
}

/// **Popping every item of a block list of maps writes `[]` on its key
/// line**, as popping a flat list's last item does; the comment between the
/// items stays below it.
#[test]
fn popping_the_last_maps_of_a_block_list_leaves_an_empty_list() {
    let source = "---\nrows:\n  - k: v\n    n: 1\n  # between\n  - k: v\n    n: 1\n---\n";
    assert_eq!(
        Document::parse(source)
            .pop_from_list("rows", &map([("k", "v".into()), ("n", Value::Int(1))])),
        Ok("---\nrows: []\n  # between\n---\n".to_string())
    );
}

/// **A pushed map takes the list's indent and line terminator on every one
/// of its lines.**
#[test]
fn a_pushed_map_takes_the_lists_indent_and_terminator_on_every_line() {
    assert_eq!(
        Document::parse("---\r\nrows:\r\n    - a\r\n---\r\n").push_to_list(
            "rows",
            &map([("k", list(["v".into()])), ("n", Value::Int(1))])
        ),
        Ok(
            "---\r\nrows:\r\n    - a\r\n    - k:\r\n        - v\r\n      n: 1\r\n---\r\n"
                .to_string()
        )
    );
}

/// **A nested element pushed onto a flow list, or onto a field the block
/// lacks, is written as a set writes it**: in block style, since only a flat
/// list is written inline.
#[test]
fn a_nested_element_pushed_onto_a_flow_or_absent_list_is_written_in_block_style() {
    assert_eq!(
        Document::parse("---\nrows: [a]\n---\n").push_to_list("rows", &list(["b".into()])),
        Ok("---\nrows:\n  - a\n  - - b\n---\n".to_string())
    );
    assert_eq!(
        Document::parse("---\ntitle: t\n---\n").push_to_list("rows", &map([("k", "v".into())])),
        Ok("---\ntitle: t\nrows:\n  - k: v\n---\n".to_string())
    );
}

// ── Setting a field to what it holds ─────────────────────────────────────

/// **A nested field set to the value it already holds changes no byte**,
/// however it is spelled and whatever comment its entry carries: the set
/// writes nothing, so it neither re-spells the value nor refuses for the
/// comment it would otherwise drop.
#[test]
fn setting_a_nested_field_to_its_own_value_changes_no_byte() {
    let source = "---\nmeta: # about\n    k: \"v\"   # why\n    rows: [ {a: 1}, [x] ]\nafter: y\n---\nbody\n";
    let held = map([
        ("k", "v".into()),
        (
            "rows",
            list([map([("a", Value::Int(1))]), list(["x".into()])]),
        ),
    ]);
    assert_eq!(set(source, "meta", &held), Ok(source.to_string()));
}

/// **A flat list set to the value it already holds changes no byte**, the
/// author's spacing and the comment inside the entry included.
#[test]
fn setting_a_flat_list_to_its_own_value_changes_no_byte() {
    for source in [
        "---\ntags: [ a,b ]   # kept\n---\n",
        "---\ntags:\n    - 'a'\n    # between\n    - b\n---\n",
    ] {
        assert_eq!(
            set(source, "tags", &list(["a".into(), "b".into()])),
            Ok(source.to_string()),
            "for {source:?}"
        );
    }
}

/// **A field set to the value it already holds changes no byte even where
/// the block's fields cannot be split apart**: an explicit `? key` or a `<<`
/// merge keeps any other set from locating its entry, but a set that writes
/// nothing needs no entry.
#[test]
fn setting_a_field_to_its_own_value_in_an_unsplittable_block_changes_no_byte() {
    for (source, field, value) in [
        (
            "---\n? meta\n: {k: v}\n---\n",
            "meta",
            map([("k", "v".into())]),
        ),
        ("---\n? x\n: 1\nb: 2\n---\n", "b", Value::Int(2)),
        ("---\n<<: {a: 1}\nb: 2\n---\n", "b", Value::Int(2)),
    ] {
        assert_eq!(
            set(source, field, &value),
            Ok(source.to_string()),
            "for {source:?}"
        );
    }
}
