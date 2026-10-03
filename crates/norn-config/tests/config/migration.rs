//! The migration ladders: a control file walked up to the version this build
//! reads, one text transform per version, refused rather than rewritten
//! where its version cannot be read, is ahead, has no step, or where the
//! rewrite would lose a comment.

use norn_config::migration::{Format, Ladder, Step};
use norn_wire::{ControlFile, MigrationRefusal};

/// A ladder over YAML text whose version is its `version` key, at version 3,
/// with a step from each of 1 and 2.
fn three_rungs() -> Ladder {
    Ladder {
        format: Format::Yaml,
        current: 3,
        version_of: Ladder::schema().version_of,
        steps: vec![
            Step {
                from: 1,
                rewrite: |text| {
                    text.replace("version: 1", "version: 2")
                        .replace("old:", "mid:")
                },
            },
            Step {
                from: 2,
                rewrite: |text| {
                    text.replace("version: 2", "version: 3")
                        .replace("mid:", "new:")
                },
            },
        ],
    }
}

/// A one-step YAML ladder from version 1 to 2 rewriting with `rewrite`.
fn one_step(rewrite: fn(&str) -> String) -> Ladder {
    Ladder {
        format: Format::Yaml,
        current: 2,
        version_of: Ladder::schema().version_of,
        steps: vec![Step { from: 1, rewrite }],
    }
}

/// A one-step TOML ladder from version 1 to 2 rewriting with `rewrite`, its
/// version the `version` key.
fn one_toml_step(rewrite: fn(&str) -> String) -> Ladder {
    Ladder {
        format: Format::Toml,
        current: 2,
        version_of: |text| {
            let table: toml::Table = text.parse().map_err(|error| format!("{error}"))?;
            table
                .get("version")
                .and_then(toml::Value::as_integer)
                .ok_or_else(|| "no version".to_string())
        },
        steps: vec![Step { from: 1, rewrite }],
    }
}

/// **A file at the version its ladder reads is current**, and nothing is
/// rewritten: the schema at `version: 1`, an empty schema and one holding
/// comments alone — the declaration that declares nothing — and any config
/// that reads as one, an empty one included.
#[test]
fn a_file_at_the_version_this_build_reads_is_current() {
    for schema in [
        "version: 1\n",
        "",
        "# nothing yet\n",
        "version: 1\nfields: {}\n",
    ] {
        assert_eq!(
            Ladder::schema().migrate(schema.as_bytes()),
            Ok(None),
            "{schema:?}"
        );
    }
    for config in ["", "[engine.sample]\nlimit = 4\n", "# a comment\n"] {
        assert_eq!(
            Ladder::config().migrate(config.as_bytes()),
            Ok(None),
            "{config:?}"
        );
    }
    assert_eq!(
        Ladder::of(ControlFile::Schema).current,
        Ladder::schema().current
    );
    assert_eq!(Ladder::of(ControlFile::Config).format, Format::Toml);
}

/// **Both shipped ladders are empty**: the schema has had one grammar version
/// and the config one shape, so no step is written yet.
#[test]
fn the_shipped_ladders_hold_no_step() {
    assert!(Ladder::schema().steps.is_empty());
    assert!(Ladder::config().steps.is_empty());
    assert_eq!(
        Ladder::schema().current,
        norn_config::schema::SCHEMA_VERSION
    );
}

/// **A file whose version cannot be read is refused, naming why**: bytes
/// that are not UTF-8, a schema that is not YAML, a mapping, or carries no
/// integer `version`, and a config that is not TOML.
#[test]
fn a_file_whose_version_cannot_be_read_is_refused_naming_why() {
    let unreadable = |ladder: Ladder, bytes: &[u8], says: &str| {
        let Err(MigrationRefusal::Unreadable { detail, .. }) = ladder.migrate(bytes) else {
            panic!("{bytes:?} was not refused as unreadable");
        };
        assert!(detail.contains(says), "{bytes:?}: {detail}");
    };
    unreadable(Ladder::schema(), b"version: 1\n\xff\n", "UTF-8");
    unreadable(Ladder::schema(), b"version: [1\n", "YAML");
    unreadable(Ladder::schema(), b"- a\n- b\n", "mapping");
    unreadable(Ladder::schema(), b"fields: {}\n", "`version`");
    unreadable(Ladder::schema(), b"version: one\n", "integer");
    unreadable(Ladder::config(), b"[engine\n", "TOML");
}

/// **A version ahead of the ladder is refused**, as is one behind it that no
/// step migrates from — the shipped schema ladder has none, so a schema at
/// version 0 is refused rather than guessed at.
#[test]
fn a_version_ahead_or_with_no_step_is_refused() {
    assert_eq!(
        Ladder::schema().migrate(b"version: 2\n"),
        Err(MigrationRefusal::version_ahead(2, 1))
    );
    assert_eq!(
        Ladder::schema().migrate(b"version: 0\n"),
        Err(MigrationRefusal::no_step(0, 1))
    );
    assert_eq!(
        three_rungs().migrate(b"version: 0\n"),
        Err(MigrationRefusal::no_step(0, 3))
    );
}

/// **A file behind is walked up one step per version, in order**, from the
/// version it states: from 1 through both steps, from 2 through the last
/// alone; and the rewrite of a file the ladder then reads at its current
/// version is current, so migrating again rewrites nothing.
#[test]
fn a_file_behind_is_walked_up_one_step_per_version() {
    let ladder = three_rungs();
    let from_one = "# owner: me\nversion: 1\nold: x # why\n";
    let migrated = ladder
        .migrate(from_one.as_bytes())
        .expect("a migration")
        .expect("a rewrite");
    assert_eq!(migrated, "# owner: me\nversion: 3\nnew: x # why\n");
    assert_eq!(ladder.migrate(migrated.as_bytes()), Ok(None));
    assert_eq!(
        ladder.migrate(b"version: 2\nmid: y\n"),
        Ok(Some("version: 3\nnew: y\n".to_string()))
    );
}

/// **A rewrite that loses a comment of its input is refused, naming the
/// first comment lost**, whether the comment stood on a line of its own or
/// after a value; and one holding the same comment twice loses one when the
/// rewrite holds it once.
#[test]
fn a_rewrite_that_loses_a_comment_is_refused_naming_it() {
    let dropping = one_step(|text| {
        text.replace("version: 1", "version: 2")
            .replace("# the owner reads this\n", "")
            .replace(" # a trailing note", "")
    });
    assert_eq!(
        dropping.migrate(b"version: 1\n# the owner reads this\nkey: v # a trailing note\n"),
        Err(MigrationRefusal::comment_lost("# the owner reads this"))
    );
    assert_eq!(
        dropping.migrate(b"version: 1\nkey: v # a trailing note\n"),
        Err(MigrationRefusal::comment_lost("# a trailing note"))
    );
    let once = one_step(|text| {
        text.replace("version: 1", "version: 2")
            .replacen("# twice\n", "", 1)
    });
    assert_eq!(
        once.migrate(b"version: 1\n# twice\na: 1\n# twice\n"),
        Err(MigrationRefusal::comment_lost("# twice"))
    );
}

/// **A comment moved is not lost**, and neither is one whose trailing
/// whitespace a rewrite trims: what a comment says is its text from its `#`
/// to the end of its line.
#[test]
fn a_comment_moved_or_trimmed_is_kept() {
    let moving = one_step(|_| "# b\nversion: 2\n# a\n".to_string());
    assert_eq!(
        moving.migrate(b"# a   \nversion: 1\n# b\n"),
        Ok(Some("# b\nversion: 2\n# a\n".to_string()))
    );
}

/// **A `#` that begins no YAML comment is no comment**, so a rewrite changing
/// the text around it loses nothing: one inside a single- or double-quoted
/// scalar, quoted across lines included, inside a block scalar, and one not
/// preceded by whitespace in a plain scalar. A comment after a block
/// scalar's header and one after its content ends are comments.
#[test]
fn a_hash_that_begins_no_yaml_comment_is_no_comment() {
    let rewriting = one_step(|text| {
        text.replace("version: 1", "version: 2")
            .replace("'single # quoted'", "'x'")
            .replace("\"double # quoted\"", "\"x\"")
            .replace("\"spanning\n  # lines\"", "\"x\"")
            .replace("  # literal\n", "  text\n")
            .replace("a#b", "ab")
    });
    let file = "version: 1\n\
                s: 'single # quoted'\n\
                d: \"double # quoted\"\n\
                m: \"spanning\n  # lines\"\n\
                b: | # header note\n  # literal\n# after the block\n\
                p: a#b\n";
    assert_eq!(
        rewriting.migrate(file.as_bytes()),
        Ok(Some(
            "version: 2\ns: 'x'\nd: \"x\"\nm: \"x\"\nb: | # header note\n  text\n# after the block\np: ab\n"
                .to_string()
        ))
    );
    let dropping_header = one_step(|text| {
        text.replace("version: 1", "version: 2")
            .replace(" # header note", "")
    });
    assert_eq!(
        dropping_header.migrate(file.as_bytes()),
        Err(MigrationRefusal::comment_lost("# header note"))
    );
    let dropping_after = one_step(|text| {
        text.replace("version: 1", "version: 2")
            .replace("# after the block\n", "")
    });
    assert_eq!(
        dropping_after.migrate(file.as_bytes()),
        Err(MigrationRefusal::comment_lost("# after the block"))
    );
}

/// **A `#` inside a TOML string is no comment, and one after a value is one
/// even with no space before it**: basic, literal and both multi-line
/// strings hold a `#` as text, and `a = 1#note` holds the comment `#note`.
#[test]
fn a_hash_inside_a_toml_string_is_no_comment() {
    let rewriting = one_toml_step(|text| {
        text.replace("version = 1", "version = 2")
            .replace("\"basic # text\"", "\"x\"")
            .replace("'literal # text'", "'x'")
            .replace("\"\"\"\nmulti # basic\n\"\"\"", "\"x\"")
            .replace("'''\nmulti # literal\n'''", "'x'")
    });
    let file = "version = 1\n\
                a = \"basic # text\"\n\
                b = 'literal # text'\n\
                c = \"\"\"\nmulti # basic\n\"\"\"\n\
                d = '''\nmulti # literal\n'''\n\
                e = 1#note\n";
    assert_eq!(
        rewriting.migrate(file.as_bytes()),
        Ok(Some(
            "version = 2\na = \"x\"\nb = 'x'\nc = \"x\"\nd = 'x'\ne = 1#note\n".to_string()
        ))
    );
    let dropping = one_toml_step(|text| {
        text.replace("version = 1", "version = 2")
            .replace("#note", "")
    });
    assert_eq!(
        dropping.migrate(file.as_bytes()),
        Err(MigrationRefusal::comment_lost("#note"))
    );
}

/// **A quote inside a plain scalar opens no quoted scalar**, so the comments
/// after it are still comments and a rewrite dropping them is refused: a
/// quote after a word on a key's line, on a plain scalar's continuation
/// line, in a sequence entry and in a flow collection's plain scalar.
#[test]
fn a_quote_inside_a_plain_scalar_hides_no_comment() {
    let dropping = one_step(|text| {
        text.replace("version: 1", "version: 2")
            .replace(" # keep me", "")
            .replace("# the owner reads this\n", "")
    });
    for file in [
        "version: 1\ntitle: rock 'n roll # keep me\n# the owner reads this\nb: 2\n",
        "version: 1\nheight: 5 \"tall # keep me\n# the owner reads this\nb: 2\n",
        "version: 1\ntitle: rock\n  'n roll # keep me\n# the owner reads this\nb: 2\n",
        "version: 1\nitems:\n- a 'b # keep me\n# the owner reads this\n",
        "version: 1\nflow: [a 'b, c] # keep me\n# the owner reads this\n",
    ] {
        assert_eq!(
            dropping.migrate(file.as_bytes()),
            Err(MigrationRefusal::comment_lost("# keep me")),
            "{file:?}"
        );
    }
}

/// **A quote where a scalar begins opens a quoted scalar**, whose `#` is no
/// comment: as a key, a sequence entry, a flow entry, and after a tag or an
/// anchor.
#[test]
fn a_quote_where_a_scalar_begins_hides_its_hash() {
    let rewriting = one_step(|text| {
        text.replace("version: 1", "version: 2")
            .replace(" # quoted", "")
    });
    let file = "version: 1\n\
                'key # quoted': v\n\
                seq:\n\
                - 'entry # quoted'\n\
                flow: ['flow # quoted', {k: \"map # quoted\"}]\n\
                tagged: !!str 'tag # quoted'\n\
                anchored: &a 'anchor # quoted'\n";
    assert_eq!(
        rewriting.migrate(file.as_bytes()),
        Ok(Some(
            "version: 2\n'key': v\nseq:\n- 'entry'\nflow: ['flow', {k: \"map\"}]\ntagged: !!str 'tag'\nanchored: &a 'anchor'\n"
                .to_string()
        ))
    );
}

/// **A block scalar's explicit indentation indicator sets its content's
/// indentation**, so a line indented less than that, though more than its
/// header's line, is no content: a comment moved there from outside the
/// block becomes the block's content and is lost.
#[test]
fn a_block_scalar_s_indentation_indicator_bounds_its_content() {
    let moving = one_step(|text| {
        text.replace("version: 1", "version: 2")
            .replace("# keep me\n", "")
            .replace("   lead\n", "   lead\n # keep me\n")
    });
    assert_eq!(
        moving.migrate(b"version: 1\n# keep me\na: |1\n   lead\n"),
        Err(MigrationRefusal::comment_lost("# keep me"))
    );
}
