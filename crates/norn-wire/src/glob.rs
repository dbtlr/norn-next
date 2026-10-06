//! The glob grammar: the one pattern language a `/`-separated name is matched
//! by.
//!
//! One grammar, three uses. A vault schema names path sets — the
//! ambiguity-ignore set — and tag sets — the patterns a declared tag facet
//! admits beyond its literal names — and a request's path part carries a glob
//! a document's path must match. All three are `/`-separated hierarchies
//! written by the same people, so all three are read by one grammar rather
//! than several that drift apart. The grammar lives here because the path
//! part is spelled here: a request's glob crosses the seam as its text, and
//! [`Pattern`] is how that text is read wherever it is matched.
//!
//! [`Pattern`] is a reading of text, not a value that crosses: the path part
//! carries the glob as a string, so the pattern carries none of the wire
//! derives.
//!
//! The grammar is the familiar one, stated exactly:
//!
//! - `?` matches one character that is not `/`.
//! - `*` matches any run of characters, empty included, that holds no `/`.
//! - `**` is a whole segment, and it matches any run of segments including no
//!   segments at all. `a**b` is two `*` inside one segment, which is the same
//!   set one `*` matches.
//! - Every other character matches itself. There is no escape and no character
//!   class: a pattern is a name with holes in it, not a regular expression.
//!
//! A pattern read with [`Pattern::parse_capturing`] also reads a **named
//! capture**: a whole segment spelled `<name>`. A capture matches exactly as a
//! whole-segment `*` does — one segment, whatever it holds — and the segment
//! it takes is bound to its name ([`Pattern::bind`]). [`Pattern::parse`] reads
//! no capture, so `<name>` there is the literal segment it spells; which
//! names a capture may carry, and where a capture may stand, is the reader's
//! to judge.
//!
//! A pattern is anchored at both ends. `archive` matches the subject `archive`
//! and nothing under it; `archive/*` matches exactly one level under it;
//! `archive/**` matches `archive` and everything under it, because `**` covers
//! the run of no segments as well as every longer one. One rule for `**` in
//! every position is what keeps `**/notes.md` matching `notes.md` at the root,
//! so the grammar states it once rather than special-casing the trailing
//! segment.
//!
//! # Case
//!
//! Whether a vault folds case is a filesystem fact, so a pattern never decides
//! it: the caller names a [`CaseFold`] at every match. Under
//! [`CaseFold::Exact`] every character compares as itself. Under
//! [`CaseFold::Ascii`] a literal ASCII letter matches either case of itself —
//! `A`–`Z` with `a`–`z` — and every other character, a letter outside ASCII
//! included, still compares as itself; `?`, `*` and `**` mean the same under
//! both, because none of them compares a character. The pattern stays the text
//! it was written as, so one grammar and one matcher answer both.
//!
//! # Its callers' fold
//!
//! Every glob over a path names the same fold, taken from one rule:
//! [`CaseFold::Ascii`] where the vault store's recorded path order folds ASCII
//! case, which is where the vault root was proven to treat two spellings as
//! one name, and [`CaseFold::Exact`] where it does not. The store states the
//! rule once (`norn_store::StoredPathOrder::glob_case`), and each path caller
//! reads it there:
//!
//! - the ambiguity-ignore set, where the store's resolver reads a class;
//! - the path part of a find, a count and a validate, where the store's read
//!   builders run the glob inside a statement, under the order the snapshot
//!   reads;
//! - a vault schema rule's path selectors and allowed paths, which the
//!   schema's selection is handed the fold for. A schema read judges a
//!   conflict between rules' globs on every root at once, under the wider
//!   [`CaseFold::Ascii`], so a refusal holds wherever the schema is pinned;
//!   a rule's route against its own allowed paths is one author's spelling
//!   against itself, judged under [`CaseFold::Exact`] on every root.
//!
//! A tag facet's patterns name tags, not paths, and no root's fold reaches
//! them: the facet folds the pattern and the tag by the tag fold
//! ([`crate::fold_tag`]) and matches the folded pair under
//! [`CaseFold::Exact`], on every root.
//!
//! # What matching costs
//!
//! **At most the pattern's length times the subject's length, for every
//! pattern and every subject.** A schema is authored by a person and read at
//! every attach and every re-pin, so the bound has to hold for the pattern
//! someone writes by accident as well as the ones they write on purpose: a
//! matcher that explored each wildcard's choices independently would cost
//! `2^stars` on a subject that nearly matches, and twelve stars against a
//! thirty-character tag is already seconds of a vault's attach. Both levels of
//! the walk therefore use the standard wildcard match, which remembers where
//! the last wildcard was taken and resumes one unit past it instead of
//! re-entering every earlier choice — see [`matches_units`].

use std::fmt;

/// A validated pattern over a `/`-separated name.
///
/// Its identity is the text it was written as, so two schemas that spell one
/// set two ways stay two declarations — the model is a projection of the bytes
/// and never a normalization of them.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Pattern {
    source: String,
    segments: Vec<Segment>,
}

/// One `/`-separated part of a pattern.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Segment {
    /// A whole segment spelled `**`: it matches any run of segments, none
    /// included.
    AnyDepth,
    /// A segment matched against one subject segment, with `*` and `?` holes.
    Within(String),
    /// A whole segment spelled `<name>`, read by [`Pattern::parse_capturing`]:
    /// it matches one subject segment as a whole-segment `*` does, and binds
    /// that segment to the name.
    Capture(String),
}

impl Segment {
    /// The segment's shape as the in-segment grammar reads it, where it
    /// matches one subject segment: a capture's is `*`.
    fn shape(&self) -> Option<&str> {
        match self {
            Segment::AnyDepth => None,
            Segment::Within(shape) => Some(shape),
            Segment::Capture(_) => Some("*"),
        }
    }
}

impl Pattern {
    /// Reads `source` as a pattern, or says why it is not one.
    pub fn parse(source: &str) -> Result<Self, PatternError> {
        if source.is_empty() {
            return Err(PatternError::Empty);
        }
        let segments = source
            .split('/')
            .map(|segment| {
                if segment == "**" {
                    Segment::AnyDepth
                } else {
                    Segment::Within(segment.to_string())
                }
            })
            .collect();
        Ok(Pattern {
            source: source.to_string(),
            segments,
        })
    }

    /// Reads `source` as a pattern in which a whole segment spelled
    /// `<name>` is a named capture, or says why it is not one.
    ///
    /// Any text between the angle brackets is taken as the name, the empty
    /// text included; a reader that admits captures judges the names.
    pub fn parse_capturing(source: &str) -> Result<Self, PatternError> {
        let mut pattern = Self::parse(source)?;
        for segment in &mut pattern.segments {
            if let Segment::Within(shape) = segment
                && let Some(name) = shape
                    .strip_prefix('<')
                    .and_then(|rest| rest.strip_suffix('>'))
            {
                *segment = Segment::Capture(name.to_string());
            }
        }
        Ok(pattern)
    }

    /// The pattern as it was written.
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// The name of every capture the pattern holds, in the order written.
    pub fn captures(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().filter_map(|segment| match segment {
            Segment::Capture(name) => Some(name.as_str()),
            _ => None,
        })
    }

    /// The segments `subject` binds to this pattern's captures, and whether
    /// that binding is the only one.
    ///
    /// **A match can bind a capture several ways**, because `**` takes any
    /// run of segments: `**/<area>/**` matches `red/blue/a.md` with `area`
    /// bound to `red` and with it bound to `blue`. Two bindings differ where
    /// some capture takes a different segment text; two matches binding every
    /// capture to the same text are one binding. The answer is
    /// [`Binding::Unique`] where every match binds alike, and
    /// [`Binding::Several`] with two bindings that differ otherwise; the walk
    /// stops at the second binding it finds.
    ///
    /// **The cost is the matching bound.** The walk fills two tables — which
    /// prefixes of the pattern match which prefixes of the subject, and which
    /// suffixes match which suffixes — over one table of which pattern segment
    /// matches which subject segment, then reads each capture's admissible
    /// segments off them and traces at most two matches back through them. Each
    /// table has one cell per pattern segment and subject segment, and the
    /// segment table's cells cost the two segments' lengths together, so the
    /// whole answer costs a constant times this pattern's length times the
    /// subject's.
    pub fn bind(&self, subject: &str, case: CaseFold) -> Binding {
        let subject: Vec<&str> = subject.split('/').collect();
        let tables = Tables::new(&self.segments, &subject, case);
        if !tables.prefix(self.segments.len(), subject.len()) {
            return Binding::Unmatched;
        }
        for (at, segment) in self.segments.iter().enumerate() {
            if !matches!(segment, Segment::Capture(_)) {
                continue;
            }
            let mut first: Option<usize> = None;
            for taken in 0..subject.len() {
                if !tables.capture_takes(at, taken) {
                    continue;
                }
                match first {
                    None => first = Some(taken),
                    Some(held) if subject[held] != subject[taken] => {
                        return Binding::Several(Box::new([
                            tables.witness(&self.segments, &subject, at, held),
                            tables.witness(&self.segments, &subject, at, taken),
                        ]));
                    }
                    Some(_) => {}
                }
            }
        }
        Binding::Unique(tables.trace(&self.segments, &subject))
    }

    /// Whether `subject` is in the set this pattern names.
    ///
    /// The walk is over segments rather than characters, which is what makes
    /// `**` a segment quantifier instead of a wildcard that eats separators by
    /// accident. The segment walk and the walk inside one segment are the same
    /// linear match over two alphabets, so the whole answer costs at most this
    /// pattern's length times the subject's.
    ///
    /// `case` says how a literal letter compares with a subject's — see
    /// [`CaseFold`].
    pub fn matches(&self, subject: &str, case: CaseFold) -> bool {
        let subject: Vec<&str> = subject.split('/').collect();
        matches_units(
            &self.segments,
            &subject,
            |segment| matches!(segment, Segment::AnyDepth),
            |segment, subject| {
                // `**` is taken by the predicate above, which is read first.
                segment
                    .shape()
                    .is_some_and(|shape| matches_within(shape, subject, case))
            },
        )
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.source)
    }
}

/// The standard wildcard match, over whatever a level's units are.
///
/// `star` says which pattern units match any run of subject units; `unit` says
/// whether one pattern unit matches one subject unit. The walk is greedy and
/// remembers where the last star was taken: a mismatch after it resumes with
/// that star consuming one more unit, rather than re-entering the choices of
/// every star before it. Each `(pattern index, subject index)` pair is
/// therefore reached at most once, which is the `pattern × subject` bound this
/// module states — and the one thing that makes it hold is that a star can
/// consume anything, so giving one more unit to the *last* star is never worse
/// than revisiting an earlier one.
fn matches_units<P, S>(
    pattern: &[P],
    subject: &[S],
    star: impl Fn(&P) -> bool,
    unit: impl Fn(&P, &S) -> bool,
) -> bool {
    let (mut at_pattern, mut at_subject) = (0usize, 0usize);
    // Where the last star stands, and how much of the subject it has taken.
    let mut last_star: Option<(usize, usize)> = None;
    while at_subject < subject.len() {
        if at_pattern < pattern.len() && star(&pattern[at_pattern]) {
            last_star = Some((at_pattern, at_subject));
            at_pattern += 1;
        } else if at_pattern < pattern.len() && unit(&pattern[at_pattern], &subject[at_subject]) {
            at_pattern += 1;
            at_subject += 1;
        } else if let Some((star_at, taken)) = last_star {
            last_star = Some((star_at, taken + 1));
            at_pattern = star_at + 1;
            at_subject = taken + 1;
        } else {
            return false;
        }
    }
    // The subject is spent, so what is left of the pattern matches only if it
    // is stars: each of them takes the empty run.
    pattern[at_pattern..].iter().all(star)
}

/// Whether one subject segment matches one pattern segment.
///
/// `*` matches any run of characters and `?` exactly one, and neither crosses a
/// separator because neither side holds one at this level. Every other pattern
/// character is a literal, compared under `case`.
fn matches_within(shape: &str, subject: &str, case: CaseFold) -> bool {
    let shape: Vec<char> = shape.chars().collect();
    let subject: Vec<char> = subject.chars().collect();
    matches_units(
        &shape,
        &subject,
        |character| *character == '*',
        |shape, subject| *shape == '?' || case.equal(*shape, *subject),
    )
}

/// What a capturing pattern binds in a subject: see [`Pattern::bind`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Binding {
    /// The pattern does not match the subject.
    Unmatched,
    /// Every match binds each capture to the same segment text.
    Unique(Captures),
    /// Two matches bind some capture to different segment texts: the first
    /// two bindings the walk found.
    Several(Box<[Captures; 2]>),
}

/// The segment each capture of one match took, in the order the captures are
/// written.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Captures(Vec<(String, String)>);

impl Captures {
    /// Each capture's name and the segment it took, in the order written.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, segment)| (name.as_str(), segment.as_str()))
    }

    /// The segment the capture `name` took, where the pattern holds it.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, segment)| segment.as_str())
    }
}

/// The three tables [`Pattern::bind`] reads: which pattern segment matches
/// which subject segment, which pattern prefix matches which subject prefix,
/// and which suffix matches which suffix. Each holds one cell per pattern
/// position and subject position.
struct Tables {
    width: usize,
    unit: Vec<bool>,
    prefix: Vec<bool>,
    suffix: Vec<bool>,
}

impl Tables {
    fn new(pattern: &[Segment], subject: &[&str], case: CaseFold) -> Self {
        let (rows, width) = (pattern.len() + 1, subject.len() + 1);
        let mut unit = vec![false; rows * width];
        for (at, segment) in pattern.iter().enumerate() {
            if let Some(shape) = segment.shape() {
                for (taken, name) in subject.iter().enumerate() {
                    unit[at * width + taken] = matches_within(shape, name, case);
                }
            }
        }
        let any_depth = |at: usize| matches!(pattern[at], Segment::AnyDepth);
        // `prefix[i][j]`: the first `i` pattern segments match the first `j`
        // subject segments.
        let mut prefix = vec![false; rows * width];
        prefix[0] = true;
        for i in 1..rows {
            for j in 0..width {
                prefix[i * width + j] = if any_depth(i - 1) {
                    prefix[(i - 1) * width + j] || (j > 0 && prefix[i * width + j - 1])
                } else {
                    j > 0 && unit[(i - 1) * width + j - 1] && prefix[(i - 1) * width + j - 1]
                };
            }
        }
        // `suffix[i][j]`: the pattern from segment `i` matches the subject
        // from segment `j`.
        let mut suffix = vec![false; rows * width];
        suffix[rows * width - 1] = true;
        for i in (0..rows - 1).rev() {
            for j in (0..width).rev() {
                suffix[i * width + j] = if any_depth(i) {
                    suffix[(i + 1) * width + j] || (j + 1 < width && suffix[i * width + j + 1])
                } else {
                    j + 1 < width && unit[i * width + j] && suffix[(i + 1) * width + j + 1]
                };
            }
        }
        Tables {
            width,
            unit,
            prefix,
            suffix,
        }
    }

    fn prefix(&self, i: usize, j: usize) -> bool {
        self.prefix[i * self.width + j]
    }

    fn suffix(&self, i: usize, j: usize) -> bool {
        self.suffix[i * self.width + j]
    }

    /// Whether some whole match has the capture at pattern segment `at` take
    /// subject segment `taken`.
    fn capture_takes(&self, at: usize, taken: usize) -> bool {
        self.prefix(at, taken)
            && self.unit[at * self.width + taken]
            && self.suffix(at + 1, taken + 1)
    }

    /// The binding of one whole match: any match, traced back from its end.
    fn trace(&self, pattern: &[Segment], subject: &[&str]) -> Captures {
        let mut bound = Vec::new();
        self.trace_prefix(pattern, subject, pattern.len(), subject.len(), &mut bound);
        Captures(ordered(pattern, bound))
    }

    /// The binding of one whole match in which the capture at pattern
    /// segment `at` takes subject segment `taken`.
    fn witness(&self, pattern: &[Segment], subject: &[&str], at: usize, taken: usize) -> Captures {
        let mut bound = Vec::new();
        self.trace_prefix(pattern, subject, at, taken, &mut bound);
        bound.push((at, subject[taken].to_string()));
        let (mut i, mut j) = (at + 1, taken + 1);
        while (i, j) != (pattern.len(), subject.len()) {
            if matches!(pattern[i], Segment::AnyDepth) {
                if self.suffix(i + 1, j) {
                    i += 1;
                } else {
                    j += 1;
                }
            } else {
                if matches!(pattern[i], Segment::Capture(_)) {
                    bound.push((i, subject[j].to_string()));
                }
                (i, j) = (i + 1, j + 1);
            }
        }
        Captures(ordered(pattern, bound))
    }

    /// Walks a prefix match ending at `(i, j)` back to its start, recording
    /// the segment each capture on the way took.
    fn trace_prefix(
        &self,
        pattern: &[Segment],
        subject: &[&str],
        mut i: usize,
        mut j: usize,
        bound: &mut Vec<(usize, String)>,
    ) {
        while (i, j) != (0, 0) {
            if matches!(pattern[i - 1], Segment::AnyDepth) {
                if self.prefix(i - 1, j) {
                    i -= 1;
                } else {
                    j -= 1;
                }
            } else {
                if matches!(pattern[i - 1], Segment::Capture(_)) {
                    bound.push((i - 1, subject[j - 1].to_string()));
                }
                (i, j) = (i - 1, j - 1);
            }
        }
    }
}

/// Each capture's name beside the segment it took, in pattern order.
fn ordered(pattern: &[Segment], mut bound: Vec<(usize, String)>) -> Vec<(String, String)> {
    bound.sort_by_key(|(at, _)| *at);
    bound
        .into_iter()
        .map(|(at, segment)| match &pattern[at] {
            Segment::Capture(name) => (name.clone(), segment),
            _ => unreachable!("only a capture is recorded"),
        })
        .collect()
}

/// Whether one document path is matched, for every set in `sets`, by some
/// pattern of that set: whether the sets' unions share a document.
///
/// **Only a document path is a witness.** A path the sets share counts where
/// the store could hold a document at it: one or more segments, none empty,
/// `.` or `..`, none holding a backslash or a control character, and a last
/// segment that is a document's file name — a stem that is not empty, `.` or
/// `..`, then `.md` in any ASCII case ([`crate::PathProblem::of_document`] and
/// [`crate::DOCUMENT_EXTENSION`]). Sets meeting only at `shared`, at
/// `area/../x.md` or at `a/../x.md` share no document. The grammar's last
/// refusal, a path starting at `/`, needs an empty first segment, which no
/// witness holds; so the walk encodes every refusal [`crate::PathProblem`]
/// makes and none is a declared gap. The `.md` extension compares in any
/// ASCII case whatever `case` says, as the vault reads a file as a document.
///
/// A capture matches as a whole-segment `*`. The answer is exact — it neither
/// misses a shared document nor reports one where there is none — under
/// `case`, which says how literal letters compare, between the patterns and
/// with the path alike.
///
/// **The walk is the product of the patterns' automata and the document
/// grammar's.** Each set is read as one automaton whose states are the
/// positions between its patterns' segments, and the walk explores tuples of
/// one state per set, consuming one path segment at a time as a folder name
/// or, once, as the file name that ends the path. Whether one segment can
/// stand where several in-segment shapes each match it is the same product
/// one level down, over characters and the document grammar's states for one
/// segment — four for a folder name, fourteen for a file name — asked once
/// per tuple of shapes and kind of segment. Write a set's weight as the sum
/// over its patterns of each pattern's length in characters plus one. For
/// patterns holding no empty segment, the segment-level walk visits at most
/// twice the product of the sets' weights in states, and the character-level
/// walks together at most eighteen times that product, each state taking a
/// step per set: the document grammar adds a constant factor and nothing
/// that grows with the patterns. The product is what a caller bounds before
/// asking.
pub fn sets_share_a_document_path(sets: &[&[Pattern]], case: CaseFold) -> bool {
    if sets.iter().any(|set| set.is_empty()) {
        return false;
    }
    // Each set's positions, numbered across its patterns: a position is a
    // pattern and the number of its segments already matched.
    let positions: Vec<Vec<(usize, usize)>> = sets
        .iter()
        .map(|set| {
            set.iter()
                .enumerate()
                .flat_map(|(at, pattern)| {
                    (0..=pattern.segments.len()).map(move |matched| (at, matched))
                })
                .collect()
        })
        .collect();
    let index_of = |set: usize, at: usize, matched: usize| {
        positions[set]
            .iter()
            .position(|held| *held == (at, matched))
            .expect("every position is numbered")
    };
    let segment = |set: usize, state: usize| {
        let (at, matched) = positions[set][state];
        sets[set][at].segments.get(matched)
    };

    let mut starts: Vec<Vec<usize>> = vec![Vec::new()];
    for (set, patterns) in sets.iter().enumerate() {
        starts = starts
            .into_iter()
            .flat_map(|start| {
                (0..patterns.len()).map(move |at| {
                    let mut next = start.clone();
                    next.push(index_of(set, at, 0));
                    next
                })
            })
            .collect();
    }

    // A state is a position per set and whether the file name ending the
    // path is already taken: after it no segment is consumed.
    let mut seen: std::collections::HashSet<(Vec<usize>, bool)> = std::collections::HashSet::new();
    let mut feasible: std::collections::HashMap<(Vec<(usize, usize)>, SegmentKind), bool> =
        std::collections::HashMap::new();
    let mut pending: Vec<(Vec<usize>, bool)> = Vec::new();
    for start in starts {
        if seen.insert((start.clone(), false)) {
            pending.push((start, false));
        }
    }
    while let Some((state, ended)) = pending.pop() {
        if ended
            && state
                .iter()
                .enumerate()
                .all(|(set, held)| segment(set, *held).is_none())
        {
            return true;
        }
        let mut next_states = Vec::new();
        // A `**` may match no segment: step past it without consuming one.
        for (set, held) in state.iter().enumerate() {
            if matches!(segment(set, *held), Some(Segment::AnyDepth)) {
                let mut next = state.clone();
                next[set] = held + 1;
                next_states.push((next, ended));
            }
        }
        // Consume one segment, a folder name or the file name: a `**` keeps
        // its place, every other segment shape must match the segment taken,
        // and a spent pattern takes none.
        let mut next = state.clone();
        let mut shapes = Vec::new();
        let mut live = !ended;
        for (set, held) in state.iter().enumerate() {
            if !live {
                break;
            }
            match segment(set, *held) {
                None => live = false,
                Some(Segment::AnyDepth) => {}
                Some(_) => {
                    shapes.push((set, *held));
                    next[set] = held + 1;
                }
            }
        }
        if live {
            for kind in [SegmentKind::Folder, SegmentKind::FileName] {
                let fits = *feasible.entry((shapes.clone(), kind)).or_insert_with(|| {
                    let shapes: Vec<&str> = shapes
                        .iter()
                        .map(|(set, held)| {
                            segment(*set, *held)
                                .and_then(Segment::shape)
                                .expect("a shape is recorded only where one stands")
                        })
                        .collect();
                    shapes_share_a_segment(&shapes, kind, case)
                });
                if fits {
                    next_states.push((next.clone(), kind == SegmentKind::FileName));
                }
            }
        }
        for next in next_states {
            if seen.insert(next.clone()) {
                pending.push(next);
            }
        }
    }
    false
}

/// Which segment of a document path one segment is: a folder name, or the
/// file name that ends the path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum SegmentKind {
    Folder,
    FileName,
}

/// What the document grammar reads one character as. A backslash and a
/// control character have no class: no document path holds one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CharClass {
    Dot,
    /// `m` or `M`, the extension's first letter.
    M,
    /// `d` or `D`, the extension's second letter.
    D,
    Other,
}

impl CharClass {
    /// Every class some character a wildcard takes may have.
    const ALL: [CharClass; 4] = [CharClass::Dot, CharClass::M, CharClass::D, CharClass::Other];

    /// The class of `character`, or nothing where no document path holds it.
    /// Every class is closed under ASCII case, so a literal and any character
    /// its case compares equal with share one.
    fn of(character: char) -> Option<Self> {
        if crate::is_refused_character(character) {
            return None;
        }
        Some(match character {
            '.' => CharClass::Dot,
            'm' | 'M' => CharClass::M,
            'd' | 'D' => CharClass::D,
            _ => CharClass::Other,
        })
    }
}

/// The document grammar's automaton over one segment's characters.
///
/// A folder name is any text but empty, `.` and `..`: its states are the
/// text so far being empty, `.`, `..` or anything else. A file name ends in
/// `.md`, in any ASCII case, after a stem that is not empty, `.` or `..` —
/// which are exactly the names `.md`, `..md` and `...md` — so its state is how
/// much of `.md` the text so far ends with, and, while the text is still a
/// prefix of one of those three names, how many dots it opened with. Fourteen
/// of those states are reachable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum SegmentState {
    /// A folder name: 0 empty, 1 `.`, 2 `..`, 3 anything else.
    Folder(u8),
    /// A file name: how much of `.md` the text ends with, and how it opens.
    FileName { tail: u8, opening: Opening },
}

/// How a file name opens while it may still be one of `.md`, `..md` and
/// `...md`, whose stems are no document's.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Opening {
    /// Only dots so far, this many.
    Dots(u8),
    /// That many dots, then `m`.
    DotsM(u8),
    /// That many dots, then `md`: a name with no document stem.
    DotsMd(u8),
    /// Anything else: no longer one of those names.
    Other,
}

impl SegmentState {
    fn start(kind: SegmentKind) -> Self {
        match kind {
            SegmentKind::Folder => SegmentState::Folder(0),
            SegmentKind::FileName => SegmentState::FileName {
                tail: 0,
                opening: Opening::Dots(0),
            },
        }
    }

    fn step(self, class: CharClass) -> Self {
        match self {
            SegmentState::Folder(held) => SegmentState::Folder(match (held, class) {
                (0..=1, CharClass::Dot) => held + 1,
                _ => 3,
            }),
            SegmentState::FileName { tail, opening } => SegmentState::FileName {
                tail: match (tail, class) {
                    (_, CharClass::Dot) => 1,
                    (1, CharClass::M) => 2,
                    (2, CharClass::D) => 3,
                    _ => 0,
                },
                opening: match (opening, class) {
                    (Opening::Dots(dots), CharClass::Dot) if dots < 3 => Opening::Dots(dots + 1),
                    (Opening::Dots(dots), CharClass::M) if dots > 0 => Opening::DotsM(dots),
                    (Opening::DotsM(dots), CharClass::D) => Opening::DotsMd(dots),
                    _ => Opening::Other,
                },
            },
        }
    }

    fn accepts(self) -> bool {
        match self {
            SegmentState::Folder(held) => held == 3,
            SegmentState::FileName { tail, opening } => {
                tail == 3 && !matches!(opening, Opening::DotsMd(_))
            }
        }
    }
}

/// Whether one segment of `kind` matches every shape in `shapes`, each read
/// by the in-segment grammar: the product of their automata over characters
/// with the document grammar's for the segment.
fn shapes_share_a_segment(shapes: &[&str], kind: SegmentKind, case: CaseFold) -> bool {
    let shapes: Vec<Vec<char>> = shapes.iter().map(|shape| shape.chars().collect()).collect();
    let start = (vec![0usize; shapes.len()], SegmentState::start(kind));
    let mut seen = std::collections::HashSet::new();
    seen.insert(start.clone());
    let mut pending = vec![start];
    while let Some((state, grammar)) = pending.pop() {
        if grammar.accepts()
            && state
                .iter()
                .zip(&shapes)
                .all(|(held, shape)| *held == shape.len())
        {
            return true;
        }
        let mut next_states = Vec::new();
        // A `*` may match no character.
        for (at, held) in state.iter().enumerate() {
            if shapes[at].get(*held) == Some(&'*') {
                let mut next = state.clone();
                next[at] = held + 1;
                next_states.push((next, grammar));
            }
        }
        // Consume one character: a `*` keeps its place, a `?` takes any, a
        // literal takes one its case compares equal with, and a spent shape
        // takes none. One character fits every literal at once only where the
        // literals compare equal with one another, which the case keeps an
        // equivalence; where no literal stands, the character is free, and
        // each class of the document grammar is tried.
        let mut next = state.clone();
        let mut literal: Option<char> = None;
        let mut live = true;
        for (at, held) in state.iter().enumerate() {
            match shapes[at].get(*held) {
                None => {
                    live = false;
                    break;
                }
                Some('*') => {}
                Some('?') => next[at] = held + 1,
                Some(character) => {
                    match literal {
                        Some(held_literal) if !case.equal(held_literal, *character) => {
                            live = false;
                            break;
                        }
                        Some(_) => {}
                        None => literal = Some(*character),
                    }
                    next[at] = held + 1;
                }
            }
        }
        if live {
            let classes: Vec<CharClass> = match literal {
                Some(character) => CharClass::of(character).into_iter().collect(),
                None => CharClass::ALL.to_vec(),
            };
            for class in classes {
                next_states.push((next.clone(), grammar.step(class)));
            }
        }
        for next in next_states {
            if seen.insert(next.clone()) {
                pending.push(next);
            }
        }
    }
    false
}

/// How a pattern's literal characters compare with a subject's.
///
/// The caller names it at every match, because whether two spellings are one
/// name is the vault root's fact, not the pattern's.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CaseFold {
    /// Every character compares as itself.
    Exact,
    /// An ASCII letter compares with its other ASCII case — `A`–`Z` with
    /// `a`–`z` — and every other character, a letter outside ASCII included,
    /// compares as itself. It is the filesystem seam's fold: the store's
    /// suite pins it to the fold contract sample the seam's fold is pinned to.
    Ascii,
}

impl CaseFold {
    /// Whether a literal pattern character and a subject character are one
    /// under this case.
    fn equal(self, literal: char, subject: char) -> bool {
        match self {
            CaseFold::Exact => literal == subject,
            CaseFold::Ascii => literal.eq_ignore_ascii_case(&subject),
        }
    }
}

/// Why a string is not a pattern.
///
/// One variant today, and an enum rather than a unit so that a grammar that
/// grows a second refusal does not change the shape every caller matches on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatternError {
    /// The empty string names no set: it matches one empty segment, which is
    /// no name any subject has.
    Empty,
}

impl fmt::Display for PatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternError::Empty => formatter.write_str("a pattern cannot be empty"),
        }
    }
}

impl std::error::Error for PatternError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, subject: &str, case: CaseFold) -> bool {
        Pattern::parse(pattern)
            .expect("a pattern")
            .matches(subject, case)
    }

    /// **Under the fold, a literal letter matches either ASCII case of
    /// itself; without it, only itself.** Every other literal — a digit, a
    /// dot, a separator — compares as itself either way.
    #[test]
    fn a_literal_letter_matches_its_other_ascii_case_only_under_the_fold() {
        for (pattern, subject) in [
            ("archive/**", "Archive/norn/glossary.md"),
            ("Archive/**", "archive/deep/term.md"),
            ("NOTES.MD", "notes.md"),
            ("v1.md", "V1.MD"),
        ] {
            assert!(
                matches(pattern, subject, CaseFold::Ascii),
                "{pattern} vs {subject}"
            );
            assert!(
                !matches(pattern, subject, CaseFold::Exact),
                "{pattern} vs {subject}"
            );
        }
        // The fold is a case fold and nothing wider: a different letter, digit
        // or separator still refuses.
        assert!(!matches("v1.md", "v2.md", CaseFold::Ascii));
        assert!(!matches("a-b", "a_b", CaseFold::Ascii));
        assert!(!matches("a/b", "a\\b", CaseFold::Ascii));
        // `@` and `` ` `` sit one byte beside `A` and `a`, and are not letters.
        assert!(!matches("@", "`", CaseFold::Ascii));
        assert!(!matches("[", "{", CaseFold::Ascii));
    }

    /// **A letter outside ASCII keeps its case under the fold.** `É` and `é`
    /// are two characters to a glob on every root.
    #[test]
    fn a_letter_outside_ascii_never_folds() {
        for case in [CaseFold::Exact, CaseFold::Ascii] {
            assert!(!matches("Été/**", "été/x.md", case), "{case:?}");
            assert!(!matches("ÉTÉ", "été", case), "{case:?}");
            assert!(matches("Été/**", "Été/x.md", case), "{case:?}");
        }
        // The ASCII letters beside them still fold.
        assert!(matches("Été/**", "ÉTé/x.md", CaseFold::Ascii));
        assert!(!matches("Été/**", "ÉTé/x.md", CaseFold::Exact));
    }

    /// **The wildcards mean the same under either case.** `?` takes one
    /// character that is not `/`, `*` a run within a segment, and `**` a run
    /// of segments; the fold changes only how the literals around them
    /// compare.
    #[test]
    fn the_wildcards_mean_the_same_under_either_case() {
        let cases: &[(&str, &str, bool, bool)] = &[
            // (pattern, subject, exact, folded)
            ("note?.md", "NoteX.MD", false, true),
            ("note?.md", "note/.md", false, false),
            ("note?.md", "Note.md", false, false),
            ("*.md", "Notes.MD", false, true),
            ("*.md", "a/Notes.md", false, false),
            ("A*Z", "abcz", false, true),
            ("A*Z", "abc/z", false, false),
            ("**/Drafts/**", "notes/drafts/x.md", false, true),
            ("**/Drafts/**", "Drafts", true, true),
            ("**/drafts/**", "notes/draftsx/x.md", false, false),
            ("Archive/*", "ARCHIVE/x.md", false, true),
            ("Archive/*", "ARCHIVE/deep/x.md", false, false),
        ];
        for (pattern, subject, exact, folded) in cases {
            assert_eq!(
                matches(pattern, subject, CaseFold::Exact),
                *exact,
                "{pattern} vs {subject}, exact"
            );
            assert_eq!(
                matches(pattern, subject, CaseFold::Ascii),
                *folded,
                "{pattern} vs {subject}, folded"
            );
        }
    }

    fn capturing(pattern: &str) -> Pattern {
        Pattern::parse_capturing(pattern).expect("a capturing pattern")
    }

    fn bound(pairs: &[(&str, &str)]) -> Captures {
        Captures(
            pairs
                .iter()
                .map(|(name, segment)| (name.to_string(), segment.to_string()))
                .collect(),
        )
    }

    /// **A capture is a whole segment `<name>`, read only where captures
    /// are.** It matches one segment as a whole-segment `*` does; the plain
    /// reading keeps `<name>` the literal segment it spells.
    #[test]
    fn a_capture_matches_one_segment_as_a_whole_segment_star_does() {
        let pattern = capturing("projects/<project>/**");
        assert_eq!(pattern.captures().collect::<Vec<_>>(), ["project"]);
        for subject in ["projects/norn/a.md", "projects/norn", "projects/x/y/z.md"] {
            assert!(pattern.matches(subject, CaseFold::Exact), "{subject}");
            assert_eq!(
                pattern.matches(subject, CaseFold::Exact),
                matches("projects/*/**", subject, CaseFold::Exact),
                "{subject}"
            );
        }
        assert!(!pattern.matches("projects", CaseFold::Exact));
        assert!(!pattern.matches("archive/norn/a.md", CaseFold::Exact));
        // An empty segment is matched as `*` matches one; no document path
        // holds one.
        assert_eq!(
            capturing("a/<x>/b").matches("a//b", CaseFold::Exact),
            matches("a/*/b", "a//b", CaseFold::Exact)
        );
        let plain = Pattern::parse("projects/<project>/**").expect("a pattern");
        assert_eq!(plain.captures().count(), 0);
        assert!(plain.matches("projects/<project>/a.md", CaseFold::Exact));
        assert!(!plain.matches("projects/norn/a.md", CaseFold::Exact));
        // Only a whole segment captures.
        assert_eq!(capturing("p-<id>.md").captures().count(), 0);
    }

    /// **A binding every match agrees on is unique; one two matches differ
    /// on is several, and both are named.**
    #[test]
    fn a_capture_binds_uniquely_or_names_two_bindings() {
        assert_eq!(
            capturing("projects/<project>/**").bind("projects/norn/tasks/a.md", CaseFold::Exact),
            Binding::Unique(bound(&[("project", "norn")]))
        );
        assert_eq!(
            capturing("<area>/<file>").bind("notes/a.md", CaseFold::Exact),
            Binding::Unique(bound(&[("area", "notes"), ("file", "a.md")]))
        );
        assert_eq!(
            capturing("**/<area>/**").bind("red/blue/a.md", CaseFold::Exact),
            Binding::Several(Box::new([
                bound(&[("area", "red")]),
                bound(&[("area", "blue")]),
            ]))
        );
        // Two matches binding one text are one binding.
        assert_eq!(
            capturing("**/<area>/**/x.md").bind("same/same/x.md", CaseFold::Exact),
            Binding::Unique(bound(&[("area", "same")]))
        );
        // Each of several bindings is a whole match: every capture bound.
        let Binding::Several(several) =
            capturing("<root>/**/<leaf>/**").bind("r/a/b/c.md", CaseFold::Exact)
        else {
            panic!("several bindings");
        };
        for captures in several.iter() {
            assert_eq!(captures.get("root"), Some("r"));
            assert!(captures.get("leaf").is_some());
        }
        assert_ne!(several[0], several[1]);
        assert_eq!(
            capturing("projects/<p>/**").bind("archive/a.md", CaseFold::Exact),
            Binding::Unmatched
        );
        assert_eq!(
            capturing("Projects/<p>").bind("projects/norn", CaseFold::Ascii),
            Binding::Unique(bound(&[("p", "norn")]))
        );
    }

    /// **Binding keeps the matching bound.** Captures between runs of `**`
    /// against a long subject are decided at once, stopping at the second
    /// binding.
    #[test]
    fn binding_keeps_the_matching_bound() {
        let pattern = capturing("**/<a>/<last>/**");
        let subject = vec!["s"; 2_000].join("/");
        let started = std::time::Instant::now();
        assert!(matches!(
            pattern.bind(&subject, CaseFold::Exact),
            Binding::Unique(_)
        ));
        let varied: Vec<String> = (0..2_000).map(|at| format!("s{at}")).collect();
        assert!(matches!(
            pattern.bind(&varied.join("/"), CaseFold::Exact),
            Binding::Several(_)
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    fn share(sets: &[&[&str]], case: CaseFold) -> bool {
        let sets: Vec<Vec<Pattern>> = sets
            .iter()
            .map(|set| {
                set.iter()
                    .map(|glob| Pattern::parse(glob).expect("a pattern"))
                    .collect()
            })
            .collect();
        let borrowed: Vec<&[Pattern]> = sets.iter().map(Vec::as_slice).collect();
        sets_share_a_document_path(&borrowed, case)
    }

    /// **Sets share a document exactly where some document path every set
    /// admits exists**: over `**`, `*`, `?` and literals, alone and together,
    /// with each set read as the union of its patterns.
    #[test]
    fn sets_share_a_document_exactly_where_one_document_path_meets_them_all() {
        let cases: &[(&[&[&str]], bool)] = &[
            (&[&["a/*"], &["*/b.md"]], true),
            (&[&["a/**"], &["b/**"]], false),
            (&[&["**"], &["x.md"]], true),
            (&[&["**"]], true),
            (&[&["**/x/**"], &["**/y/**"]], true),
            (&[&["a/b.md"], &["a/b.md/c.md"]], false),
            (&[&["?.md"], &["ab.md"]], false),
            (&[&["?b.md"], &["a?.md"]], true),
            (&[&["a*"], &["*z.md"]], true),
            (&[&["a*"], &["*z.md"], &["?.md"]], false),
            (&[&["projects/*/tasks/**"], &["projects/norn/**"]], true),
            (&[&["projects/*/tasks/**"], &["archive/**"]], false),
            (&[&["a/**", "b/**"], &["b/x.md"]], true),
            (&[&["a/**", "b/**"], &["c/x.md", "d/**"]], false),
            (&[&["a.md"], &[]], false),
            // A path's segments are not empty, so `*` alone needs one
            // character and `**` one segment where a path stands.
            (&[&["*"], &["**"]], true),
            (&[&["x/**"], &["x/a.md"]], true),
            (&[&["x/*/y.md"], &["x/y.md"]], false),
        ];
        for (sets, shared) in cases {
            assert_eq!(share(sets, CaseFold::Exact), *shared, "{sets:?}");
        }
        assert!(!share(&[&["A.md"], &["a.md"]], CaseFold::Exact));
        assert!(share(&[&["A.md"], &["a.md"]], CaseFold::Ascii));
        assert!(!share(&[&["É.md"], &["é.md"]], CaseFold::Ascii));
    }

    /// **A path no document could stand at is no witness.** Sets meeting only
    /// at a path the document grammar refuses — no `.md` file name, a `.` or
    /// `..` segment, a stem of nothing, `.` or `..`, a backslash or a control
    /// character — share no document, under either fold; the extension
    /// compares in any ASCII case under both, as the vault reads a document.
    #[test]
    fn sets_meeting_only_where_no_document_stands_share_none() {
        let disjoint: &[&[&[&str]]] = &[
            // No document file name: only `shared`, a folder or a non-`.md` leaf.
            &[&["a/*.md", "shared"], &["b/*.md", "shared"]],
            &[&["notes"], &["*"]],
            &[&["*.md"], &["notes"]],
            &[&["a/*.txt"], &["a/**"]],
            // A `.` or `..` segment, spelled or forced by wildcards.
            &[&["area/../*.md"], &["area/**"]],
            &[&["a/.?/x.md"], &["a/?./x.md"]],
            &[&["a/./x.md"], &["**"]],
            &[&["?/x.md"], &[".*/x.md"], &["*./x.md"]],
            // A file name whose stem is nothing, `.` or `..`.
            &[&["*"], &[".md"]],
            &[&["*"], &["..md"]],
            &[&["*"], &["...md"]],
            &[&["**/?.md"], &["**/..md"]],
            &[&["*.md"], &["?"], &["??"], &["???"]],
            // A refused character.
            &[&["a\\b.md"], &["**"]],
            &[&["a\u{1}.md"], &["*"]],
        ];
        for sets in disjoint {
            for case in [CaseFold::Exact, CaseFold::Ascii] {
                assert!(!share(sets, case), "{sets:?} {case:?}");
            }
        }
        let shared: &[&[&[&str]]] = &[
            &[&["*"], &["....md"]],
            &[&["*"], &[".a.md"]],
            &[&["**/*"], &["notes/a.MD", "x"]],
            &[&["*.Md"], &["?.*"]],
            &[&["**/.*/*"], &["**/*./*.md"]],
        ];
        for sets in shared {
            for case in [CaseFold::Exact, CaseFold::Ascii] {
                assert!(share(sets, case), "{sets:?} {case:?}");
            }
        }
    }

    /// A small deterministic generator for the differential cases.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next() % u64::try_from(bound).expect("a small bound"))
                .expect("below a usize bound")
        }

        fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
            items[self.below(items.len())]
        }
    }

    /// Whether `path` is a document path: the grammar's own judgment and the
    /// vault's document extension.
    fn is_document(path: &str) -> bool {
        crate::PathProblem::of_document(path).is_none()
            && path
                .rsplit('/')
                .next()
                .and_then(|leaf| {
                    leaf.rfind('.')
                        .filter(|dot| *dot > 0)
                        .map(|dot| &leaf[dot + 1..])
                })
                .is_some_and(|extension| extension.eq_ignore_ascii_case(crate::DOCUMENT_EXTENSION))
    }

    /// Every string of `alphabet` from one character to `longest`.
    fn words(alphabet: &[char], longest: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut layer = vec![String::new()];
        for _ in 0..longest {
            layer = layer
                .iter()
                .flat_map(|prefix| alphabet.iter().map(move |c| format!("{prefix}{c}")))
                .collect();
            out.extend(layer.iter().cloned());
        }
        out
    }

    /// **The walk agrees with brute force over document paths**: random sets
    /// of globs over the shapes the document grammar turns on, against every
    /// path of up to three segments over a small alphabet, under both folds.
    #[test]
    fn sets_share_a_document_path_agrees_with_brute_force() {
        const SHAPES: &[&str] = &[
            "**", "**", "*", "?", "a", "A", ".", "..", ".?", "?.", "a*", "*a", "*.md", "?.md",
            "*.MD", "a.md", "..md", "*.*", "??",
        ];
        let leaves: Vec<String> = words(&['a', 'A', '.'], 2)
            .into_iter()
            .flat_map(|stem| [format!("{stem}.md"), format!("{stem}.MD")])
            .collect();
        let mut folders = words(&['a', '.', 'm'], 2);
        folders.extend(leaves.iter().cloned());
        let mut paths: Vec<String> = leaves.clone();
        for folder in &folders {
            for leaf in &leaves {
                paths.push(format!("{folder}/{leaf}"));
                for inner in &folders {
                    paths.push(format!("{folder}/{inner}/{leaf}"));
                }
            }
        }
        // The universe holds non-documents too, so the filter is what is
        // under test, not the universe.
        let documents: Vec<&String> = paths.iter().filter(|path| is_document(path)).collect();
        assert!(documents.len() < paths.len());

        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut disagreements = Vec::new();
        for case in [CaseFold::Exact, CaseFold::Ascii] {
            for _ in 0..150 {
                let sets: Vec<Vec<Pattern>> = (0..2 + rng.below(2))
                    .map(|_| {
                        (0..1 + rng.below(2))
                            .map(|_| {
                                let glob: Vec<&str> =
                                    (0..1 + rng.below(3)).map(|_| rng.pick(SHAPES)).collect();
                                Pattern::parse(&glob.join("/")).expect("a pattern")
                            })
                            .collect()
                    })
                    .collect();
                let borrowed: Vec<&[Pattern]> = sets.iter().map(Vec::as_slice).collect();
                let fast = sets_share_a_document_path(&borrowed, case);
                let brute = documents.iter().find(|path| {
                    sets.iter()
                        .all(|set| set.iter().any(|p| p.matches(path, case)))
                });
                if fast != brute.is_some() {
                    disagreements.push(format!(
                        "{case:?} {:?} fast={fast} brute={brute:?}",
                        sets.iter()
                            .map(|set| set.iter().map(Pattern::as_str).collect::<Vec<_>>())
                            .collect::<Vec<_>>()
                    ));
                }
            }
        }
        assert!(
            disagreements.is_empty(),
            "{:#?}",
            &disagreements[..disagreements.len().min(10)]
        );
    }

    /// Every capture tuple some whole match binds, by brute force.
    fn all_bindings(
        pattern: &[&str],
        subject: &[&str],
        bound: &mut Vec<String>,
        out: &mut std::collections::BTreeSet<Vec<String>>,
    ) {
        match pattern.first() {
            None => {
                if subject.is_empty() {
                    out.insert(bound.clone());
                }
            }
            Some(&"**") => {
                for take in 0..=subject.len() {
                    all_bindings(&pattern[1..], &subject[take..], bound, out);
                }
            }
            Some(segment) => {
                let Some(first) = subject.first() else {
                    return;
                };
                if segment.starts_with('<') {
                    bound.push((*first).to_string());
                    all_bindings(&pattern[1..], &subject[1..], bound, out);
                    bound.pop();
                } else if matches_within(segment, first, CaseFold::Exact) {
                    all_bindings(&pattern[1..], &subject[1..], bound, out);
                }
            }
        }
    }

    /// **Binding agrees with brute force**: unmatched where no match exists,
    /// unique where every match binds alike, and otherwise two different
    /// bindings some match makes.
    #[test]
    fn bind_agrees_with_brute_force() {
        const PIECES: &[&str] = &["**", "**", "<x>", "<y>", "<z>", "a", "*", "b*", "?"];
        const NAMES: &[&str] = &["a", "b", "ab", "ba", "c"];
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let mut problems = Vec::new();
        for _ in 0..5_000 {
            let mut used = std::collections::BTreeSet::new();
            let mut pattern: Vec<&str> = Vec::new();
            while pattern.len() < 1 + rng.below(5) {
                let piece = rng.pick(PIECES);
                if piece.starts_with('<') && !used.insert(piece) {
                    continue;
                }
                pattern.push(piece);
            }
            let subject: Vec<&str> = (0..1 + rng.below(5)).map(|_| rng.pick(NAMES)).collect();
            let mut brute = std::collections::BTreeSet::new();
            all_bindings(&pattern, &subject, &mut Vec::new(), &mut brute);
            let parsed = capturing(&pattern.join("/"));
            let names: Vec<&str> = parsed.captures().collect();
            let tuple = |captures: &Captures| -> Vec<String> {
                names
                    .iter()
                    .map(|name| captures.get(name).expect("every capture bound").to_string())
                    .collect()
            };
            let got = parsed.bind(&subject.join("/"), CaseFold::Exact);
            let agrees = match &got {
                Binding::Unmatched => brute.is_empty(),
                Binding::Unique(captures) => brute.len() == 1 && brute.contains(&tuple(captures)),
                Binding::Several(two) => {
                    brute.len() >= 2
                        && brute.contains(&tuple(&two[0]))
                        && brute.contains(&tuple(&two[1]))
                        && tuple(&two[0]) != tuple(&two[1])
                }
            };
            if !agrees {
                problems.push(format!(
                    "{} / {}: {got:?}, brute {brute:?}",
                    pattern.join("/"),
                    subject.join("/")
                ));
            }
        }
        assert!(
            problems.is_empty(),
            "{:#?}",
            &problems[..problems.len().min(10)]
        );
    }

    /// **The fold keeps the matching bound.** A pattern whose stars would each
    /// be an independent choice answers at once under the fold as without it.
    #[test]
    fn the_fold_keeps_the_matching_bound() {
        let pattern = Pattern::parse(&format!("{}B", "A*".repeat(12))).expect("a pattern");
        let subject = "a".repeat(64);
        let started = std::time::Instant::now();
        assert!(!pattern.matches(&subject, CaseFold::Ascii));
        assert!(pattern.matches(&format!("{subject}b"), CaseFold::Ascii));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
