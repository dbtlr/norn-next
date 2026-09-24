//! What the full-text index reads as a word, stated in Rust.
//!
//! **A search term counts only where it holds a word, and a word is what the
//! index's tokenizer reads as one.** The tokenizer is `unicode61` (see
//! [`crate::ddl`]'s full-text pillar), which reads a word as a run of token
//! characters. A character begins a word where the Unicode tables the bundled
//! FTS5 carries class it a letter (`L*`), a number (`N*`) or private use
//! (`Co`), or class it not at all: a code point those tables hold no category
//! for is a token character. A combining diacritic continues a word and begins
//! none, and every other character — whitespace, punctuation, symbols, format
//! characters — separates words. Those tables are older than the Unicode
//! version Rust carries, so a character assigned since, as a symbol or a mark,
//! still begins a word here: `🙂` is a word to the index, and `😀`, which the
//! tables class as a symbol, is not.
//!
//! [`WORD_STARTS`] is that classification, read off the tokenizer itself. The
//! contract test in this module reads it again for every code point, through
//! FTS5 under the tokenizer the DDL declares, so a tokenizer that reads words
//! otherwise — another SQLite, other tokenizer options — fails it, and the
//! failure prints the table the tokenizer reads now.

use std::cmp::Ordering;

/// Whether `term` holds a word as the full-text index reads words: whether any
/// of its characters begins one.
///
/// This is the one rule a search's query is read by. A term holding no word is
/// one FTS5 reads no token in, and a phrase of no token is one it drops from a
/// conjunction, so the search builder drops such a term itself, by this rule,
/// and a query none of whose terms holds a word is reported.
pub(crate) fn holds_word(term: &str) -> bool {
    term.chars().any(begins_word)
}

/// Whether the tokenizer begins a word at `character`.
fn begins_word(character: char) -> bool {
    let code = u32::from(character);
    WORD_STARTS
        .binary_search_by(|&(first, last)| {
            if last < code {
                Ordering::Less
            } else if first > code {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        })
        .is_ok()
}

/// The code points the tokenizer begins a word at, as inclusive ranges in
/// ascending order, none adjacent to the next. A range may span the surrogate
/// code points, which are no character.
const WORD_STARTS: &[(u32, u32)] = &[
    (0x0030, 0x0039),
    (0x0041, 0x005A),
    (0x0061, 0x007A),
    (0x00AA, 0x00AA),
    (0x00B2, 0x00B3),
    (0x00B5, 0x00B5),
    (0x00B9, 0x00BA),
    (0x00BC, 0x00BE),
    (0x00C0, 0x00D6),
    (0x00D8, 0x00F6),
    (0x00F8, 0x02C1),
    (0x02C6, 0x02D1),
    (0x02E0, 0x02E4),
    (0x02EC, 0x02EC),
    (0x02EE, 0x02EE),
    (0x0370, 0x0374),
    (0x0376, 0x037D),
    (0x037F, 0x0383),
    (0x0386, 0x0386),
    (0x0388, 0x03F5),
    (0x03F7, 0x0481),
    (0x048A, 0x0559),
    (0x0560, 0x0588),
    (0x058B, 0x058E),
    (0x0590, 0x0590),
    (0x05C8, 0x05F2),
    (0x05F5, 0x05FF),
    (0x0605, 0x0605),
    (0x061C, 0x061D),
    (0x0620, 0x064A),
    (0x0660, 0x0669),
    (0x066E, 0x066F),
    (0x0671, 0x06D3),
    (0x06D5, 0x06D5),
    (0x06E5, 0x06E6),
    (0x06EE, 0x06FC),
    (0x06FF, 0x06FF),
    (0x070E, 0x070E),
    (0x0710, 0x0710),
    (0x0712, 0x072F),
    (0x074B, 0x07A5),
    (0x07B1, 0x07EA),
    (0x07F4, 0x07F5),
    (0x07FA, 0x0815),
    (0x081A, 0x081A),
    (0x0824, 0x0824),
    (0x0828, 0x0828),
    (0x082E, 0x082F),
    (0x083F, 0x0858),
    (0x085C, 0x085D),
    (0x085F, 0x08E3),
    (0x08FF, 0x08FF),
    (0x0904, 0x0939),
    (0x093D, 0x093D),
    (0x0950, 0x0950),
    (0x0958, 0x0961),
    (0x0966, 0x096F),
    (0x0971, 0x0980),
    (0x0984, 0x09BB),
    (0x09BD, 0x09BD),
    (0x09C5, 0x09C6),
    (0x09C9, 0x09CA),
    (0x09CE, 0x09D6),
    (0x09D8, 0x09E1),
    (0x09E4, 0x09F1),
    (0x09F4, 0x09F9),
    (0x09FC, 0x0A00),
    (0x0A04, 0x0A3B),
    (0x0A3D, 0x0A3D),
    (0x0A43, 0x0A46),
    (0x0A49, 0x0A4A),
    (0x0A4E, 0x0A50),
    (0x0A52, 0x0A6F),
    (0x0A72, 0x0A74),
    (0x0A76, 0x0A80),
    (0x0A84, 0x0ABB),
    (0x0ABD, 0x0ABD),
    (0x0AC6, 0x0AC6),
    (0x0ACA, 0x0ACA),
    (0x0ACE, 0x0AE1),
    (0x0AE4, 0x0AEF),
    (0x0AF2, 0x0B00),
    (0x0B04, 0x0B3B),
    (0x0B3D, 0x0B3D),
    (0x0B45, 0x0B46),
    (0x0B49, 0x0B4A),
    (0x0B4E, 0x0B55),
    (0x0B58, 0x0B61),
    (0x0B64, 0x0B6F),
    (0x0B71, 0x0B81),
    (0x0B83, 0x0BBD),
    (0x0BC3, 0x0BC5),
    (0x0BC9, 0x0BC9),
    (0x0BCE, 0x0BD6),
    (0x0BD8, 0x0BF2),
    (0x0BFB, 0x0C00),
    (0x0C04, 0x0C3D),
    (0x0C45, 0x0C45),
    (0x0C49, 0x0C49),
    (0x0C4E, 0x0C54),
    (0x0C57, 0x0C61),
    (0x0C64, 0x0C7E),
    (0x0C80, 0x0C81),
    (0x0C84, 0x0CBB),
    (0x0CBD, 0x0CBD),
    (0x0CC5, 0x0CC5),
    (0x0CC9, 0x0CC9),
    (0x0CCE, 0x0CD4),
    (0x0CD7, 0x0CE1),
    (0x0CE4, 0x0D01),
    (0x0D04, 0x0D3D),
    (0x0D45, 0x0D45),
    (0x0D49, 0x0D49),
    (0x0D4E, 0x0D56),
    (0x0D58, 0x0D61),
    (0x0D64, 0x0D78),
    (0x0D7A, 0x0D81),
    (0x0D84, 0x0DC9),
    (0x0DCB, 0x0DCE),
    (0x0DD5, 0x0DD5),
    (0x0DD7, 0x0DD7),
    (0x0DE0, 0x0DF1),
    (0x0DF5, 0x0E30),
    (0x0E32, 0x0E33),
    (0x0E3B, 0x0E3E),
    (0x0E40, 0x0E46),
    (0x0E50, 0x0E59),
    (0x0E5C, 0x0EB0),
    (0x0EB2, 0x0EB3),
    (0x0EBA, 0x0EBA),
    (0x0EBD, 0x0EC7),
    (0x0ECE, 0x0F00),
    (0x0F20, 0x0F33),
    (0x0F40, 0x0F70),
    (0x0F88, 0x0F8C),
    (0x0F98, 0x0F98),
    (0x0FBD, 0x0FBD),
    (0x0FCD, 0x0FCD),
    (0x0FDB, 0x102A),
    (0x103F, 0x1049),
    (0x1050, 0x1055),
    (0x105A, 0x105D),
    (0x1061, 0x1061),
    (0x1065, 0x1066),
    (0x106E, 0x1070),
    (0x1075, 0x1081),
    (0x108E, 0x108E),
    (0x1090, 0x1099),
    (0x10A0, 0x10FA),
    (0x10FC, 0x135C),
    (0x1369, 0x138F),
    (0x139A, 0x13FF),
    (0x1401, 0x166C),
    (0x166F, 0x167F),
    (0x1681, 0x169A),
    (0x169D, 0x16EA),
    (0x16EE, 0x1711),
    (0x1715, 0x1731),
    (0x1737, 0x1751),
    (0x1754, 0x1771),
    (0x1774, 0x17B3),
    (0x17D7, 0x17D7),
    (0x17DC, 0x17DC),
    (0x17DE, 0x17FF),
    (0x180F, 0x18A8),
    (0x18AA, 0x191F),
    (0x192C, 0x192F),
    (0x193C, 0x193F),
    (0x1941, 0x1943),
    (0x1946, 0x19AF),
    (0x19C1, 0x19C7),
    (0x19CA, 0x19DD),
    (0x1A00, 0x1A16),
    (0x1A1C, 0x1A1D),
    (0x1A20, 0x1A54),
    (0x1A5F, 0x1A5F),
    (0x1A7D, 0x1A7E),
    (0x1A80, 0x1A9F),
    (0x1AA7, 0x1AA7),
    (0x1AAE, 0x1AFF),
    (0x1B05, 0x1B33),
    (0x1B45, 0x1B59),
    (0x1B7D, 0x1B7F),
    (0x1B83, 0x1BA0),
    (0x1BAE, 0x1BE5),
    (0x1BF4, 0x1BFB),
    (0x1C00, 0x1C23),
    (0x1C38, 0x1C3A),
    (0x1C40, 0x1C7D),
    (0x1C80, 0x1CBF),
    (0x1CC8, 0x1CCF),
    (0x1CE9, 0x1CEC),
    (0x1CEE, 0x1CF1),
    (0x1CF5, 0x1DBF),
    (0x1DE7, 0x1DFB),
    (0x1E00, 0x1FBC),
    (0x1FBE, 0x1FBE),
    (0x1FC2, 0x1FCC),
    (0x1FD0, 0x1FDC),
    (0x1FE0, 0x1FEC),
    (0x1FF0, 0x1FFC),
    (0x1FFF, 0x1FFF),
    (0x2065, 0x2069),
    (0x2070, 0x2079),
    (0x207F, 0x2089),
    (0x208F, 0x209F),
    (0x20BA, 0x20CF),
    (0x20F1, 0x20FF),
    (0x2102, 0x2102),
    (0x2107, 0x2107),
    (0x210A, 0x2113),
    (0x2115, 0x2115),
    (0x2119, 0x211D),
    (0x2124, 0x2124),
    (0x2126, 0x2126),
    (0x2128, 0x2128),
    (0x212A, 0x212D),
    (0x212F, 0x2139),
    (0x213C, 0x213F),
    (0x2145, 0x2149),
    (0x214E, 0x214E),
    (0x2150, 0x218F),
    (0x23F4, 0x23FF),
    (0x2427, 0x243F),
    (0x244B, 0x249B),
    (0x24EA, 0x24FF),
    (0x2700, 0x2700),
    (0x2776, 0x2793),
    (0x2B4D, 0x2B4F),
    (0x2B5A, 0x2CE4),
    (0x2CEB, 0x2CEE),
    (0x2CF2, 0x2CF8),
    (0x2CFD, 0x2CFD),
    (0x2D00, 0x2D6F),
    (0x2D71, 0x2D7E),
    (0x2D80, 0x2DDF),
    (0x2E2F, 0x2E2F),
    (0x2E3C, 0x2E7F),
    (0x2E9A, 0x2E9A),
    (0x2EF4, 0x2EFF),
    (0x2FD6, 0x2FEF),
    (0x2FFC, 0x2FFF),
    (0x3005, 0x3007),
    (0x3021, 0x3029),
    (0x3031, 0x3035),
    (0x3038, 0x303C),
    (0x3040, 0x3098),
    (0x309D, 0x309F),
    (0x30A1, 0x30FA),
    (0x30FC, 0x318F),
    (0x3192, 0x3195),
    (0x31A0, 0x31BF),
    (0x31E4, 0x31FF),
    (0x321F, 0x3229),
    (0x3248, 0x324F),
    (0x3251, 0x325F),
    (0x3280, 0x3289),
    (0x32B1, 0x32BF),
    (0x32FF, 0x32FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0xA48F),
    (0xA4C7, 0xA4FD),
    (0xA500, 0xA60C),
    (0xA610, 0xA66E),
    (0xA67F, 0xA69E),
    (0xA6A0, 0xA6EF),
    (0xA6F8, 0xA6FF),
    (0xA717, 0xA71F),
    (0xA722, 0xA788),
    (0xA78B, 0xA801),
    (0xA803, 0xA805),
    (0xA807, 0xA80A),
    (0xA80C, 0xA822),
    (0xA82C, 0xA835),
    (0xA83A, 0xA873),
    (0xA878, 0xA87F),
    (0xA882, 0xA8B3),
    (0xA8C5, 0xA8CD),
    (0xA8D0, 0xA8DF),
    (0xA8F2, 0xA8F7),
    (0xA8FB, 0xA925),
    (0xA930, 0xA946),
    (0xA954, 0xA95E),
    (0xA960, 0xA97F),
    (0xA984, 0xA9B2),
    (0xA9CE, 0xA9DD),
    (0xA9E0, 0xAA28),
    (0xAA37, 0xAA42),
    (0xAA44, 0xAA4B),
    (0xAA4E, 0xAA5B),
    (0xAA60, 0xAA76),
    (0xAA7A, 0xAA7A),
    (0xAA7C, 0xAAAF),
    (0xAAB1, 0xAAB1),
    (0xAAB5, 0xAAB6),
    (0xAAB9, 0xAABD),
    (0xAAC0, 0xAAC0),
    (0xAAC2, 0xAADD),
    (0xAAE0, 0xAAEA),
    (0xAAF2, 0xAAF4),
    (0xAAF7, 0xABE2),
    (0xABEE, 0xFB1D),
    (0xFB1F, 0xFB28),
    (0xFB2A, 0xFBB1),
    (0xFBC2, 0xFD3D),
    (0xFD40, 0xFDFB),
    (0xFDFE, 0xFDFF),
    (0xFE1A, 0xFE1F),
    (0xFE27, 0xFE2F),
    (0xFE53, 0xFE53),
    (0xFE67, 0xFE67),
    (0xFE6C, 0xFEFE),
    (0xFF00, 0xFF00),
    (0xFF10, 0xFF19),
    (0xFF21, 0xFF3A),
    (0xFF41, 0xFF5A),
    (0xFF66, 0xFFDF),
    (0xFFE7, 0xFFE7),
    (0xFFEF, 0xFFF8),
    (0x10000, 0x100FF),
    (0x10103, 0x10136),
    (0x10140, 0x10178),
    (0x1018A, 0x1018F),
    (0x1019C, 0x101CF),
    (0x101FE, 0x1039E),
    (0x103A0, 0x103CF),
    (0x103D1, 0x10856),
    (0x10858, 0x1091E),
    (0x10920, 0x1093E),
    (0x10940, 0x10A00),
    (0x10A04, 0x10A04),
    (0x10A07, 0x10A0B),
    (0x10A10, 0x10A37),
    (0x10A3B, 0x10A3E),
    (0x10A40, 0x10A4F),
    (0x10A59, 0x10A7E),
    (0x10A80, 0x10B38),
    (0x10B40, 0x10FFF),
    (0x11003, 0x11037),
    (0x1104E, 0x1107F),
    (0x11083, 0x110AF),
    (0x110C2, 0x110FF),
    (0x11103, 0x11126),
    (0x11135, 0x1113F),
    (0x11144, 0x1117F),
    (0x11183, 0x111B2),
    (0x111C1, 0x111C4),
    (0x111C9, 0x116AA),
    (0x116B8, 0x1246F),
    (0x12474, 0x16F50),
    (0x16F7F, 0x16F8E),
    (0x16F93, 0x1CFFF),
    (0x1D0F6, 0x1D0FF),
    (0x1D127, 0x1D128),
    (0x1D1DE, 0x1D1FF),
    (0x1D246, 0x1D2FF),
    (0x1D357, 0x1D6C0),
    (0x1D6C2, 0x1D6DA),
    (0x1D6DC, 0x1D6FA),
    (0x1D6FC, 0x1D714),
    (0x1D716, 0x1D734),
    (0x1D736, 0x1D74E),
    (0x1D750, 0x1D76E),
    (0x1D770, 0x1D788),
    (0x1D78A, 0x1D7A8),
    (0x1D7AA, 0x1D7C2),
    (0x1D7C4, 0x1EEEF),
    (0x1EEF2, 0x1EFFF),
    (0x1F02C, 0x1F02F),
    (0x1F094, 0x1F09F),
    (0x1F0AF, 0x1F0B0),
    (0x1F0BF, 0x1F0C0),
    (0x1F0D0, 0x1F0D0),
    (0x1F0E0, 0x1F10F),
    (0x1F12F, 0x1F12F),
    (0x1F16C, 0x1F16F),
    (0x1F19B, 0x1F1E5),
    (0x1F203, 0x1F20F),
    (0x1F23B, 0x1F23F),
    (0x1F249, 0x1F24F),
    (0x1F252, 0x1F2FF),
    (0x1F321, 0x1F32F),
    (0x1F336, 0x1F336),
    (0x1F37D, 0x1F37F),
    (0x1F394, 0x1F39F),
    (0x1F3C5, 0x1F3C5),
    (0x1F3CB, 0x1F3DF),
    (0x1F3F1, 0x1F3FF),
    (0x1F43F, 0x1F43F),
    (0x1F441, 0x1F441),
    (0x1F4F8, 0x1F4F8),
    (0x1F4FD, 0x1F4FF),
    (0x1F53E, 0x1F53F),
    (0x1F544, 0x1F54F),
    (0x1F568, 0x1F5FA),
    (0x1F641, 0x1F644),
    (0x1F650, 0x1F67F),
    (0x1F6C6, 0x1F6FF),
    (0x1F774, 0xE0000),
    (0xE0002, 0xE001F),
    (0xE0080, 0xE00FF),
    (0xE01F0, 0x10FFFF),
];

#[cfg(test)]
mod tests {
    use norn_testkit::scratch::Scratch;

    use super::{WORD_STARTS, holds_word};
    use crate::{Store, StoredPathOrder};

    /// The tokenizer the full-text pillar's DDL declares.
    fn declared_tokenizer() -> String {
        let statement = crate::ddl::statements()
            .into_iter()
            .find(|statement| statement.starts_with("CREATE VIRTUAL TABLE documents_fts"))
            .expect("the full-text pillar");
        let (_, rest) = statement
            .split_once("tokenize = '")
            .expect("the pillar declares its tokenizer");
        let (tokenizer, _) = rest.split_once('\'').expect("a quoted tokenizer");
        tokenizer.to_string()
    }

    /// A one-document index under the declared tokenizer, holding the word
    /// `anchor`, and whether FTS5 reads a word in a string: a query of
    /// `anchor` and the string as a phrase answers the document exactly where
    /// FTS5 dropped the phrase, which it does where it reads no token in it.
    ///
    /// The index is a temporary table on a throwaway store's own connection,
    /// so it runs on the SQLite the store runs on and leaves the store's
    /// schema as it was.
    struct Probe {
        _scratch: Scratch,
        store: Store,
    }

    impl Probe {
        fn new() -> Self {
            let scratch = Scratch::new("norn-store-word-probe");
            let store = Store::open_throwaway(
                scratch.join("derived").join("store.sqlite3"),
                StoredPathOrder::Sensitive,
            )
            .expect("a store opens");
            store
                .connection()
                .execute_batch(&format!(
                    "CREATE VIRTUAL TABLE temp.probe USING fts5(body, tokenize = '{}');
                     INSERT INTO temp.probe(body) VALUES ('anchor');",
                    declared_tokenizer()
                ))
                .expect("a one-document index under the declared tokenizer");
            Probe {
                _scratch: scratch,
                store,
            }
        }

        fn reads_a_word(&self, text: &str) -> bool {
            let mut statement = self
                .store
                .connection()
                .prepare_cached("SELECT count(*) FROM temp.probe WHERE probe MATCH ?1")
                .expect("the probe");
            let expression = format!("\"anchor\" \"{}\"", text.replace('"', "\"\""));
            let matched: i64 = statement
                .query_row([expression], |row| row.get(0))
                .expect("a match against the probe");
            matched == 0
        }
    }

    /// **The table is the tokenizer's, code point for code point.** Every
    /// character from U+0001 to U+10FFFF, alone, is run through FTS5 under the
    /// declared tokenizer, and the ranges it reads a word in are exactly
    /// [`WORD_STARTS`]. NUL is no character a term holds: a query splits at
    /// it.
    #[test]
    fn the_word_table_is_the_tokenizers_for_every_code_point() {
        let probe = Probe::new();
        let mut read: Vec<(u32, u32)> = Vec::new();
        for character in (1..=0x10_FFFF).filter_map(char::from_u32) {
            if !probe.reads_a_word(&character.to_string()) {
                continue;
            }
            let code = u32::from(character);
            match read.last_mut() {
                Some((_, last)) if follows(*last, code) => {
                    *last = code;
                }
                _ => read.push((code, code)),
            }
        }
        let table: String = read
            .iter()
            .map(|(first, last)| format!("    ({first:#06X}, {last:#06X}),\n"))
            .collect();
        assert!(
            read == WORD_STARTS,
            "the tokenizer reads words at other code points than `WORD_STARTS` states; the table \
             it reads now is:\n{table}"
        );
    }

    /// **A term holds a word exactly where FTS5 reads one in it.** Every
    /// string of the corpus is judged by [`holds_word`] and by FTS5 under the
    /// declared tokenizer as the corpus states: each ASCII character alone,
    /// which is a word exactly where it is a letter or a digit; ASCII
    /// punctuation together; spaces Unicode reads as whitespace; zero-width
    /// and format characters; emoji the tokenizer's tables class as symbols
    /// and emoji they hold no category for; CJK; combining marks alone and on
    /// a letter; digits and numerals of several scripts; and private-use
    /// characters.
    #[test]
    fn a_term_holds_a_word_exactly_where_fts5_reads_one() {
        let probe = Probe::new();
        let mut corpus: Vec<(String, bool)> = (1_u8..=127)
            .map(char::from)
            .map(|character| (character.to_string(), character.is_ascii_alphanumeric()))
            .collect();
        corpus.extend(
            [
                ("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~", false),
                ("--", false),
                ("a-b", true),
                ("\u{00A0}", false),
                ("\u{2003}", false),
                ("\u{3000}", false),
                ("\u{200B}", false),
                ("\u{200C}\u{200D}", false),
                ("\u{FEFF}", false),
                ("\u{00AD}", false),
                ("\u{1F600}", false),
                ("\u{2764}\u{FE0F}", false),
                ("\u{1F469}\u{200D}\u{1F4BB}", false),
                ("\u{1F642}", true),
                ("\u{1F44D}\u{1F3FD}", true),
                ("中文", true),
                ("한국어", true),
                ("\u{0301}", false),
                ("\u{0301}\u{0302}", false),
                ("e\u{0301}", true),
                ("\u{0301}e", true),
                ("é", true),
                ("42", true),
                ("\u{0663}", true),
                ("\u{2460}", true),
                ("\u{216B}", true),
                ("\u{00BD}", true),
                ("\u{E000}", true),
                ("\u{F8FF}", true),
                ("\u{F0000}", true),
                ("\u{10FFFD}", true),
            ]
            .map(|(text, word)| (text.to_string(), word)),
        );
        for (text, word) in &corpus {
            assert_eq!(
                probe.reads_a_word(text),
                *word,
                "FTS5 reads {text:?} otherwise than the corpus states"
            );
            assert_eq!(
                holds_word(text),
                *word,
                "the word rule reads {text:?} otherwise than FTS5 does"
            );
        }
    }

    /// Whether `code` is the next character after `last`: the next code point,
    /// or the first past the surrogates.
    fn follows(last: u32, code: u32) -> bool {
        code == last + 1 || (last == 0xD7FF && code == 0xE000)
    }
}
