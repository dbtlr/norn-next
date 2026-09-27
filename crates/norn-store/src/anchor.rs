//! Whether a document holds the place a link's anchor names: one predicate over
//! readings stored per heading, per block and per link.
//!
//! A heading anchor matches a heading under the first of its three readings
//! that matches any ([`crate::AnchorReadings`]), so whether a document holds
//! the heading it names is whether any heading matches one of the three: a
//! heading reading equal to the anchor's text or marked reading, or a slug
//! equal to its slug reading. A block reference matches a block definition by
//! its identifier, exactly. Each is an equality seek on an index that leads
//! with the document — `headings_document_reading`, `headings_document_slug`
//! and `blocks_document_block_id` — so the predicate costs a handful of seeks
//! into one document, whatever the vault holds.
//!
//! The predicate is meaningful only for a link that carries an anchor: a link
//! with none holds no reading and no block reference, and the predicate is
//! false for it.
//!
//! **A dormant carrier.** The link-health judgment ([ADR 0027]) files a
//! missing-anchor finding where a link resolves to one document and that
//! document does not hold the place the link's anchor names, and it embeds
//! [`anchor_held`] to decide that. The store judges no finding yet, so no
//! reader runs the predicate: [`link_anchor_held_sql`] is explained through
//! [`crate::ExplainedStatement::LinkAnchorHeld`], which is what bars the
//! indexes it seeks until the judgment runs it.
//!
//! [ADR 0027]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0027-link-health-rides-the-changeset.md

/// The predicate that the document whose row id `target` evaluates to holds
/// the place named by the anchor of the `links` row aliased `link`.
///
/// `link` is a table alias and `target` an SQL expression, both spliced into
/// the text: neither is ever a value a caller supplies.
pub(crate) fn anchor_held(link: &str, target: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM headings AS hr
                  WHERE hr.document = {target}
                    AND hr.reading IN ({link}.anchor_text, {link}.anchor_marked))
          OR EXISTS (SELECT 1 FROM headings AS hs
                     WHERE hs.document = {target} AND hs.slug = {link}.anchor_slug)
          OR EXISTS (SELECT 1 FROM blocks AS hb
                     WHERE hb.document = {target} AND hb.block_id = {link}.block_ref))"
    )
}

/// Whether the document whose row id is `?3` holds the place named by the
/// anchor of the link at ordinal `?2` among the links of the document whose
/// row id is `?1`.
///
/// The link is reached by `links_document_ordinal`, and the predicate is
/// [`anchor_held`] over it.
pub(crate) fn link_anchor_held_sql() -> String {
    format!(
        "SELECT {} FROM links AS l WHERE l.document = ?1 AND l.ordinal = ?2",
        anchor_held("l", "?3")
    )
}

#[cfg(test)]
mod tests {
    use norn_db::rusqlite::params;
    use norn_text::{BodyScan, Heading, SectionAddress, anchor_readings, heading_reading};

    use super::*;
    use crate::facts::{
        AnchorReadings, BlockFact, DerivationVersion, DocumentFacts, HeadingFact, LinkFact,
        LinkFamily, Span, StoredPathOrder,
    };
    use crate::increment::{Change, IncrementProvenance};
    use crate::path::DocumentPath;
    use crate::store::Store;

    /// How many generated documents the property is checked over.
    const CASES: usize = 120;

    /// How many generated anchors each document is asked about.
    const ANCHORS: usize = 18;

    /// The words generated heading text is spelled from: ASCII and not, case
    /// that folds and case that does not, and the characters an anchor's
    /// readings treat specially — `#` and `%`.
    const WORDS: &[&str] = &[
        "Alpha", "beta", "GAMMA", "C#", "Top", "Sub", "My", "Heading", "Über", "émile", "100%",
        "x-y", "a.b",
    ];

    /// The separators generated heading text joins its words with: spaces that
    /// fold, and a no-break space that does not.
    const SEPARATORS: &[&str] = &[" ", "  ", " \t", "\u{a0}"];

    /// SplitMix64: a deterministic stream, so a failing case is the same case
    /// on every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }

        fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
            from[self.below(from.len())]
        }
    }

    /// One to three words joined by generated separators.
    fn heading_text(rng: &mut Rng) -> String {
        let words = 1 + rng.below(3);
        let mut text = rng.pick(WORDS).to_string();
        for _ in 1..words {
            text.push_str(rng.pick(SEPARATORS));
            text.push_str(rng.pick(WORDS));
        }
        text
    }

    /// A body of one to five ATX headings, some closed, some quoted, each
    /// over a paragraph that may carry a block identifier.
    fn body(rng: &mut Rng) -> String {
        let mut body = String::new();
        for at in 0..1 + rng.below(5) {
            let quote = if rng.below(6) == 0 { "> " } else { "" };
            let closer = if rng.below(4) == 0 { " ##" } else { "" };
            let hashes = "#".repeat(1 + rng.below(6));
            body.push_str(&format!(
                "{quote}{hashes} {}{closer}\n\n",
                heading_text(rng)
            ));
            let block = if rng.below(2) == 0 {
                format!(" ^b{at}")
            } else {
                String::new()
            };
            body.push_str(&format!("para {at}{block}\n\n"));
        }
        body
    }

    /// `text` with its ASCII letters' case flipped at random.
    fn recased(rng: &mut Rng, text: &str) -> String {
        text.chars()
            .map(|ch| match rng.below(2) {
                0 => ch.to_ascii_uppercase(),
                _ => ch.to_ascii_lowercase(),
            })
            .collect()
    }

    /// `text` with its ASCII spaces respelled and padding added.
    fn respaced(rng: &mut Rng, text: &str) -> String {
        let words: Vec<&str> = text.split(' ').filter(|word| !word.is_empty()).collect();
        let pad = rng.pick(&["", " ", "\t", "  "]);
        let inner = rng.pick(&[" ", "  ", "\t", " \t "]);
        format!("{pad}{}{pad}", words.join(inner))
    }

    /// `text` with its spaces, hashes and percent signs percent-encoded.
    fn encoded(text: &str) -> String {
        text.replace('%', "%25")
            .replace(' ', "%20")
            .replace('#', "%23")
    }

    /// What a generated link names past its target.
    enum Anchor {
        Heading(String),
        Block(String),
    }

    /// An anchor drawn against `headings` and `blocks`: each reading's
    /// spelling of a heading the document carries, a heading chain, an
    /// encoded anchor, a block reference, and anchors that name nothing.
    fn anchor(rng: &mut Rng, headings: &[Heading], blocks: &[String]) -> Anchor {
        let heading = &headings[rng.below(headings.len())];
        let text = heading.text.clone();
        Anchor::Heading(match rng.below(14) {
            0 => text,
            1 => recased(rng, &text),
            2 => respaced(rng, &text),
            3 => format!("{} {}", "#".repeat(1 + rng.below(7)), recased(rng, &text)),
            4 => format!("## {text} ##"),
            5 => heading.slug.clone(),
            6 => heading.slug.to_ascii_uppercase(),
            7 => format!("{}#{}", rng.pick(WORDS), recased(rng, &text)),
            8 => encoded(&recased(rng, &text)),
            9 => heading_text(rng),
            10 => format!("#{text}"),
            11 => format!("## {text}\n## {text}"),
            12 => String::new(),
            _ => {
                let block = match (rng.below(3), blocks.is_empty()) {
                    (0, _) | (_, true) => format!("b{}", rng.below(6)),
                    _ => blocks[rng.below(blocks.len())].clone(),
                };
                return Anchor::Block(if rng.below(8) == 0 {
                    String::new()
                } else {
                    block
                });
            }
        })
    }

    fn span() -> Span {
        Span {
            line: 1,
            column: 1,
            byte_offset: 0,
        }
    }

    /// A document at `at` holding `body`'s headings and blocks, each heading
    /// with the reading the text layer gives it, as the host derives them.
    fn target(at: &DocumentPath, body: &str) -> DocumentFacts {
        let scan = BodyScan::new(body);
        let mut facts = DocumentFacts::new(at.clone(), "hash", body, body.len() as u64);
        facts.headings = scan
            .headings()
            .iter()
            .map(|heading| HeadingFact {
                level: heading.level,
                text: heading.text.clone(),
                reading: heading_reading(&heading.text),
                slug: heading.slug.clone(),
                span: span(),
                body_offset: heading.body_offset as u64,
                inside_container: heading.inside_container,
            })
            .collect();
        facts.blocks = scan
            .block_ids()
            .into_iter()
            .map(|block| BlockFact {
                block_id: block.id,
                span: None,
            })
            .collect();
        facts
    }

    /// A link to `target` naming `anchor`, carrying the readings the text
    /// layer gives it, as the host derives them.
    fn link(target: &str, anchor: &Anchor) -> LinkFact {
        let (anchor, block_ref) = match anchor {
            Anchor::Heading(text) => (Some(text.clone()), None),
            Anchor::Block(id) => (None, Some(id.clone())),
        };
        LinkFact {
            family: LinkFamily::Wikilink,
            embed: false,
            protocol: None,
            target: target.to_string(),
            title: None,
            anchor_readings: anchor.as_deref().and_then(anchor_readings).map(|readings| {
                AnchorReadings {
                    text: readings.text,
                    marked: readings.marked,
                    slug: readings.slug,
                }
            }),
            anchor,
            block_ref,
            span: span(),
        }
    }

    /// **The stored predicate agrees with the section resolver.** Over
    /// generated documents and anchors, [`anchor_held`] over the readings the
    /// store holds is true exactly where `resolve_section` finds a section for
    /// a heading anchor, and exactly where the document defines the block a
    /// block reference names. The generated anchors reach a heading through
    /// each of the three readings alone, so a predicate missing one, or
    /// comparing the text under a case fold alone, disagrees.
    #[test]
    fn stored_readings_agree_with_resolve_section() {
        let root = norn_testkit::scratch::Scratch::new("norn-store-anchor-readings");
        let mut store = Store::open_throwaway(
            root.join("store.sqlite3"),
            StoredPathOrder::Sensitive,
            DerivationVersion::new(1),
        )
        .expect("opening a store");
        let mut rng = Rng(0x5eed_253a);
        let (mut held, mut missed, mut folded, mut marked_alone, mut slug_alone) = (0, 0, 0, 0, 0);

        for case in 0..CASES {
            let body = body(&mut rng);
            let scan = BodyScan::new(&body);
            let headings = scan.headings().to_vec();
            let blocks: Vec<String> = scan.block_ids().into_iter().map(|b| b.id).collect();
            let anchors: Vec<Anchor> = (0..ANCHORS)
                .map(|_| anchor(&mut rng, &headings, &blocks))
                .collect();
            let target_at = DocumentPath::new(&format!("t{case}.md")).expect("a path");
            let holder_at = DocumentPath::new(&format!("h{case}.md")).expect("a path");
            let mut holder = DocumentFacts::new(holder_at.clone(), "hash", "", 0);
            holder.links = anchors
                .iter()
                .map(|anchor| link(&format!("t{case}"), anchor))
                .collect();
            store
                .begin_request()
                .apply_increment(
                    IncrementProvenance::Derived,
                    [
                        Change::Upsert(target(&target_at, &body)),
                        Change::Upsert(holder),
                    ],
                    &[],
                )
                .expect("writing a case");
            let id = |at: &DocumentPath| -> i64 {
                store
                    .connection()
                    .query_row(
                        "SELECT id FROM documents WHERE path = ?1",
                        [at.as_str()],
                        |row| row.get(0),
                    )
                    .expect("a document row")
            };
            let (holder_id, target_id) = (id(&holder_at), id(&target_at));

            for (ordinal, anchor) in anchors.iter().enumerate() {
                let expected = match anchor {
                    Anchor::Heading(text) => {
                        norn_text::resolve_section(&headings, &body, SectionAddress::first(text))
                            .is_ok()
                    }
                    Anchor::Block(id) => blocks.contains(id),
                };
                let stored: bool = store
                    .connection()
                    .query_row(
                        &link_anchor_held_sql(),
                        params![holder_id, ordinal as i64, target_id],
                        |row| row.get(0),
                    )
                    .expect("evaluating the predicate");
                assert_eq!(
                    stored,
                    expected,
                    "case {case}, link {ordinal}: the store and the resolver disagree about \
                     {:?} over {body:?}",
                    match anchor {
                        Anchor::Heading(text) => format!("#{text}"),
                        Anchor::Block(id) => format!("#^{id}"),
                    }
                );
                if !expected {
                    missed += 1;
                    continue;
                }
                held += 1;
                let Anchor::Heading(text) = anchor else {
                    continue;
                };
                let readings = anchor_readings(text).expect("a held anchor is not empty");
                let reads = |wanted: &str| {
                    headings
                        .iter()
                        .any(|heading| heading_reading(&heading.text) == wanted)
                };
                if reads(&readings.text) {
                    if !headings.iter().any(|heading| heading.text == *text) {
                        folded += 1;
                    }
                } else if readings.marked.as_deref().is_some_and(reads) {
                    marked_alone += 1;
                } else {
                    slug_alone += 1;
                }
            }
        }

        for (count, what) in [
            (held, "an anchor the document holds"),
            (missed, "an anchor the document does not hold"),
            (folded, "a heading reached only by folding its text"),
            (
                marked_alone,
                "a heading reached by its marked reading alone",
            ),
            (slug_alone, "a heading reached by its slug alone"),
        ] {
            assert!(count >= 20, "the generator reached {what} {count} times");
        }
    }
}
