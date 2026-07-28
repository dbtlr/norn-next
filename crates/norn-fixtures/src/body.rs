//! Document bodies: how long, and made of what.
//!
//! A body is built by drawing a target length from the profile's mixture and
//! then emitting blocks until it is reached. Blocks are not all prose —
//! lists, fences and quotes are what a real document is made of, and a
//! generator emitting paragraphs alone measures a parser that never meets a
//! fence.

use crate::links::Links;
use crate::profile::BodyShape;
use crate::rng::Rng;
use crate::words::{CODE_LANGS, CODE_LINES, HEADINGS, MIDDLES, OPENERS, TAILS};

/// Block-kind weights, per mille: prose dominates, and the rest appear often
/// enough to be present in every document of any length.
const BLOCK_WEIGHTS: [u32; 5] = [550, 180, 90, 100, 80];

/// The remaining length below which the builder stops drawing blocks and
/// closes the body with single sentences. It sits above the largest block any
/// pool can produce, so a block is only ever drawn when the body still has
/// room for it and overshoot stays inside one sentence.
const TAIL_FILL_BYTES: usize = 600;

fn sentence(rng: &mut Rng) -> String {
    format!(
        "{} {} {}.",
        rng.pick(OPENERS),
        rng.pick(MIDDLES),
        rng.pick(TAILS)
    )
}

fn paragraph(rng: &mut Rng, sentences: usize, out: &mut String) {
    for i in 0..sentences {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&sentence(rng));
    }
    out.push_str("\n\n");
}

fn bullet_list(rng: &mut Rng, out: &mut String) {
    for _ in 0..rng.between(3, 7) {
        out.push_str("- ");
        out.push_str(&sentence(rng));
        out.push('\n');
    }
    out.push('\n');
}

fn numbered_list(rng: &mut Rng, out: &mut String) {
    let items = rng.between(3, 6);
    for item in 1..=items {
        out.push_str(&format!("{item}. "));
        out.push_str(&sentence(rng));
        out.push('\n');
    }
    out.push('\n');
}

fn code_fence(rng: &mut Rng, out: &mut String) {
    out.push_str("```");
    out.push_str(rng.pick(CODE_LANGS));
    out.push('\n');
    for _ in 0..rng.between(3, 9) {
        out.push_str(rng.pick(CODE_LINES));
        out.push('\n');
    }
    out.push_str("```\n\n");
}

fn block_quote(rng: &mut Rng, out: &mut String) {
    for _ in 0..rng.between(1, 3) {
        out.push_str("> ");
        out.push_str(&sentence(rng));
        out.push('\n');
    }
    out.push('\n');
}

fn block(rng: &mut Rng, out: &mut String) {
    match rng.weighted(&BLOCK_WEIGHTS) {
        0 => {
            let sentences = rng.between(2, 6);
            paragraph(rng, sentences, out);
        }
        1 => bullet_list(rng, out),
        2 => numbered_list(rng, out),
        3 => code_fence(rng, out),
        _ => block_quote(rng, out),
    }
}

/// Draw a target body length from `shape`'s mixture.
pub fn draw_length(rng: &mut Rng, shape: &BodyShape) -> usize {
    let weights: Vec<u32> = shape.length.iter().map(|b| b.weight_per_mille).collect();
    let bucket = &shape.length[rng.weighted(&weights)];
    rng.between(bucket.min_bytes, bucket.max_bytes)
}

/// Build a body of about `target` bytes, placing `links` inline as it goes or
/// gathering them into a trailing list.
pub fn build(rng: &mut Rng, shape: &BodyShape, target: usize, links: &Links) -> String {
    let mut out = String::with_capacity(target + 512);
    let mut pending: usize = 0;
    let mut since_heading = usize::MAX;

    while out.len() < target {
        if since_heading >= shape.bytes_per_heading {
            out.push_str("## ");
            out.push_str(rng.pick(HEADINGS));
            out.push_str("\n\n");
            since_heading = 0;
        }
        let before = out.len();
        if target.saturating_sub(out.len()) <= TAIL_FILL_BYTES {
            paragraph(rng, 1, &mut out);
        } else {
            block(rng, &mut out);
        }
        if !links.trailing_list && pending < links.markup.len() && rng.per_mille(450) {
            out.push_str("Related: ");
            out.push_str(&links.markup[pending]);
            out.push_str("\n\n");
            pending += 1;
        }
        since_heading += out.len() - before;
    }

    if pending < links.markup.len() {
        out.push_str("## Links\n\n");
        for link in &links.markup[pending..] {
            out.push_str("- ");
            out.push_str(link);
            out.push('\n');
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{LengthBucket, Profile};

    fn no_links() -> Links {
        Links {
            markup: Vec::new(),
            dangling: 0,
            trailing_list: false,
        }
    }

    const SPAN_SHORT_TO_LONG: &[LengthBucket] = &[LengthBucket {
        weight_per_mille: 1000,
        min_bytes: 200,
        max_bytes: 12_000,
    }];
    const SPAN_MEDIUM: &[LengthBucket] = &[LengthBucket {
        weight_per_mille: 1000,
        min_bytes: 300,
        max_bytes: 6_000,
    }];
    const SPAN_LONG: &[LengthBucket] = &[LengthBucket {
        weight_per_mille: 1000,
        min_bytes: 20_000,
        max_bytes: 20_000,
    }];

    fn fixed_shape(length: &'static [LengthBucket]) -> BodyShape {
        BodyShape {
            length,
            bytes_per_heading: 700,
            title_heading_per_mille: 0,
        }
    }

    #[test]
    fn a_body_reaches_its_target_without_running_far_past_it() {
        let shape = fixed_shape(SPAN_SHORT_TO_LONG);
        let mut rng = Rng::new(13);
        for _ in 0..200 {
            let target = draw_length(&mut rng, &shape);
            let body = build(&mut rng, &shape, target, &no_links());
            assert!(body.len() >= target, "body fell short of its target");
            assert!(
                body.len() < target + 200,
                "body overshot {target} by {}",
                body.len() - target
            );
        }
    }

    #[test]
    fn every_link_appears_exactly_once() {
        let shape = fixed_shape(SPAN_MEDIUM);
        let mut rng = Rng::new(17);
        for trailing_list in [false, true] {
            let links = Links {
                markup: (0..9).map(|i| format!("[[target-{i}]]")).collect(),
                dangling: 0,
                trailing_list,
            };
            let target = draw_length(&mut rng, &shape);
            let body = build(&mut rng, &shape, target, &links);
            for link in &links.markup {
                assert_eq!(body.matches(link.as_str()).count(), 1, "link {link}");
            }
        }
    }

    #[test]
    fn a_realistic_body_carries_more_than_one_kind_of_block() {
        let shape = fixed_shape(SPAN_LONG);
        let mut rng = Rng::new(23);
        let body = build(&mut rng, &shape, 20_000, &no_links());
        assert!(body.contains("\n- "), "no bullet list");
        assert!(body.contains("\n1. "), "no numbered list");
        assert!(body.contains("```"), "no code fence");
        assert!(body.contains("\n> "), "no block quote");
        assert!(body.contains("\n## "), "no heading");
    }

    #[test]
    fn no_code_line_opens_with_a_heading_marker() {
        // The calibration probe counts heading lines by their leading bytes,
        // so a fenced line starting with `#` would be miscounted as one.
        for line in CODE_LINES {
            assert!(!line.starts_with('#'), "code line opens with `#`: {line}");
        }
    }

    #[test]
    fn heading_spacing_tracks_the_declared_interval() {
        let profile = Profile::by_name("realistic").expect("realistic profile");
        let mut rng = Rng::new(29);
        let body = build(&mut rng, &profile.body, 40_000, &no_links());
        let headings = body.lines().filter(|l| l.starts_with("## ")).count();
        let expected = 40_000 / profile.body.bytes_per_heading;
        assert!(
            headings >= expected / 2 && headings <= expected * 2,
            "{headings} headings against an expected ~{expected}"
        );
    }
}
