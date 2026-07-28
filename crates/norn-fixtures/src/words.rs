//! Deterministic word pools. Plain `&[&str]` slices, indexed by the seeded
//! generator — no hashing, no iteration order that depends on anything.
//!
//! The pools exist to produce plausible *shape*, not plausible meaning: what
//! matters downstream is how long documents are, how they are punctuated by
//! headings and blocks, and how many links they carry.

/// Top-level directory names, in the order a vault tends to grow them. The
/// order is load-bearing when a profile's placement skew concentrates
/// documents into the earlier directories.
pub const TOP_LEVEL_DIRS: &[&str] = &[
    "notes",
    "projects",
    "journal",
    "references",
    "meetings",
    "areas",
    "people",
    "resources",
    "archive",
    "inbox",
];

/// Sub-directory names, cycled to build each nesting level.
pub const SUB_DIRS: &[&str] = &[
    "drafts",
    "active",
    "reading",
    "specs",
    "reviews",
    "threads",
    "backlog",
    "assets",
    "topics",
    "clippings",
    "weekly",
    "later",
];

/// Directory names for the clutter-only directories a profile asks for —
/// directories that hold no Markdown at all and exist to make a walk cost
/// what a real walk costs.
pub const EMPTY_DIRS: &[&str] = &[
    "attachments/raw",
    "attachments/screens",
    "exports",
    "exports/pdf",
    "scratch",
    ".trash",
    ".obsidian/plugins",
    "media/audio",
    "media/video",
    "backup/2023",
    "backup/2024",
    "templates/unused",
];

/// Document-stem words, combined with an index suffix so every non-ambiguous
/// stem is unique across a whole tree.
pub const DOC_WORDS: &[&str] = &[
    "sprout", "acorn", "lantern", "compass", "ember", "harbor", "ridge", "brook", "meridian",
    "quartz", "cinder", "orchard", "beacon", "furrow", "hearth", "kestrel", "linden", "marrow",
    "nimbus", "opal", "pebble", "quill", "ripple", "sable", "tundra", "umber", "vale", "wisp",
    "yarrow", "zephyr", "cobble", "drift", "fathom", "gable", "hollow", "ingot", "juniper",
    "kindle", "lattice", "mantle",
];

/// Stems that repeat across directories to form ambiguity classes. Every one
/// is a name a real vault carries in many folders at once, which is why
/// resolving a bare stem is genuinely ambiguous rather than artificially so.
pub const AMBIGUOUS_STEMS: &[&str] = &[
    "index",
    "readme",
    "overview",
    "summary",
    "notes",
    "log",
    "todo",
    "ideas",
    "questions",
    "scratch",
    "agenda",
    "retro",
    "plan",
    "roadmap",
    "glossary",
    "references",
    "inbox",
    "backlog",
    "decisions",
    "meeting",
    "standup",
    "review",
    "changelog",
    "setup",
];

/// Non-ASCII stem fragments, for the share of documents whose names are not
/// plain ASCII. Real vaults have them; a walk, a path comparison and a stem
/// index all have to survive them.
pub const UNICODE_STEMS: &[&str] = &[
    "über-notiz",
    "café-plan",
    "naïve-draft",
    "résumé",
    "señal",
    "日次ノート",
    "заметка",
    "τόμος",
];

/// Multi-word stem fragments, for the share of documents whose names carry
/// interior spaces.
pub const SPACED_STEMS: &[&str] = &[
    "Wide Open Spaces",
    "Reading List",
    "Weekly Review",
    "Open Questions",
    "Meeting Notes",
    "Trip Planning",
];

/// Section heading phrases.
pub const HEADINGS: &[&str] = &[
    "Overview",
    "Background",
    "Context",
    "Notes",
    "Details",
    "Findings",
    "Next Steps",
    "Open Questions",
    "References",
    "Summary",
    "Constraints",
    "What changed",
    "Prior art",
    "Risks",
    "Decision",
    "Follow-ups",
];

/// Frontmatter `type` values.
pub const DOC_TYPES: &[&str] = &["note", "task", "phase", "log", "reference", "meeting"];

/// Frontmatter `status` values, for the document types that carry one.
pub const STATUS_VALUES: &[&str] = &["backlog", "active", "blocked", "done"];

/// Frontmatter tag values.
pub const TAGS: &[&str] = &[
    "research",
    "writing",
    "infra",
    "reading",
    "personal",
    "planning",
    "review",
    "archive",
    "draft",
    "reference",
];

/// Sentence openers. Combined with a middle and a tail below, the three pools
/// span several thousand distinct sentences, which keeps a long body from
/// reading as one line repeated.
pub const OPENERS: &[&str] = &[
    "The current approach",
    "Each pass over the tree",
    "This section",
    "The measured result",
    "A second reading",
    "The remaining work",
    "Every entry in the table",
    "The earlier draft",
    "Most of the surface",
    "The narrow case",
    "One open question",
    "The follow-up",
    "Nothing in the present shape",
    "The simplest reading",
    "A later revision",
    "The whole set",
];

/// Sentence middles.
pub const MIDDLES: &[&str] = &[
    "holds up under",
    "needs rework before",
    "was reconsidered during",
    "still depends on",
    "should be split from",
    "reads cleanly against",
    "adds nothing to",
    "conflicts with",
    "was measured against",
    "carries the weight of",
    "is bounded by",
    "gets rewritten alongside",
    "leaves room for",
    "argues for",
    "settles",
    "narrows",
];

/// Sentence tails, each ending the sentence.
pub const TAILS: &[&str] = &[
    "the second pass",
    "a wider audit",
    "the smaller profile",
    "the original constraint",
    "everything downstream",
    "the recorded baseline",
    "a later cleanup",
    "the boundary it sits on",
    "one more round of review",
    "the shape agreed earlier",
    "the cost of the walk",
    "a single obvious path",
    "the numbers in the table above",
    "what the index already knows",
    "the parts that never changed",
    "a decision taken elsewhere",
];

/// Fenced-code languages.
pub const CODE_LANGS: &[&str] = &["rust", "sql", "bash", "json", "yaml", "text"];

/// Fenced-code lines. None begins with `#`, so a heading meter reading raw
/// bytes is not fooled by a comment inside a fence.
pub const CODE_LINES: &[&str] = &[
    "let total = entries.iter().map(|e| e.len()).sum::<usize>();",
    "select path, count(*) from documents group by path;",
    "cargo test --workspace --all-targets",
    "{ \"kind\": \"file\", \"path\": \"notes/index.md\" }",
    "walk: { root: ., follow_links: false }",
    "for entry in read_dir(root)? { visit(entry?)?; }",
    "assert_eq!(observed, expected, \"tree digests differ\");",
    "explain query plan select * from links where target = ?;",
    "rg --files --glob '*.md' | wc -l",
    "retention: 30d",
    "match kind { Kind::File => read(path), Kind::Dir => walk(path) }",
    "update documents set body_bytes = ? where id = ?;",
];

/// Attachment and clutter file stems.
pub const CLUTTER_STEMS: &[&str] = &[
    "screenshot",
    "diagram",
    "scan",
    "export",
    "chart",
    "photo",
    "clip",
    "handout",
    "receipt",
    "whiteboard",
];

/// Clutter extensions whose bytes are not valid UTF-8, paired with the magic
/// bytes a real file of that kind opens with.
pub const BINARY_CLUTTER: &[(&str, &[u8])] = &[
    ("png", b"\x89PNG\r\n\x1a\n"),
    ("pdf", b"%PDF-1.7\n\xff\xfe"),
    ("jpg", b"\xff\xd8\xff\xe0"),
    ("zip", b"PK\x03\x04\xff"),
];

/// Clutter extensions whose bytes are text.
pub const TEXT_CLUTTER: &[&str] = &["txt", "json", "csv", "canvas", "excalidraw", "webloc"];

/// Text-clutter line templates, cycled to fill a file to its drawn size.
pub const CLUTTER_LINES: &[&str] = &[
    "{\"id\": 41, \"kind\": \"node\", \"x\": 120, \"y\": 480}",
    "date,label,value",
    "2024-04-17,throughput,1288",
    "captured from the reading queue; no further processing",
    "URL=https://example.invalid/reference/9214",
    "column_a\tcolumn_b\tcolumn_c",
];
