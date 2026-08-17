//! SAUCE — the "Standard Architecture for Universal Comment Extensions" record
//! that scene art (ANSI/ASCII/XBin/BIN/…) appends as the **last 128 bytes** of a
//! file. It carries the title/author/group/date plus rendering hints (canvas
//! width, iCE colors, font). Spec: <http://www.acid.org/info/sauce/sauce.html>.

/// A parsed SAUCE record. Strings are already trimmed of the format's space/NUL
/// padding. Only the fields we use are kept.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sauce {
    pub title: String,
    pub author: String,
    pub group: String,
    pub date: String, // raw CCYYMMDD
    pub data_type: u8,
    pub file_type: u8,
    pub tinfo1: u16, // character art: canvas width
    pub tinfo2: u16, // character art: number of lines
    pub comments: u8,
    pub ice: bool,       // TFlags bit 0 — non-blink / iCE colors
    pub font: String,    // TInfoS — font name
    pub comment: String, // SAUCE comment text (the COMNT block / API `Comments`), if any
}

impl Sauce {
    /// Authoritative character width for Character-type art, if specified.
    pub fn char_width(&self) -> Option<usize> {
        match self.data_type {
            // Character-type art (ANSi/ASCII/TundraDraw/…) carries its canvas width in TInfo1.
            1 if self.tinfo1 > 0 => Some(self.tinfo1 as usize),
            // BinaryText (.BIN) has no header width; SAUCE stores it as FileType × 2
            // (the spec's encoding, e.g. an 80-column screen → FileType 40). Without this
            // a non-160-wide .BIN falls back to the 160 default and shears (33-N1.BIN: 500).
            5 if self.file_type > 0 => Some(self.file_type as usize * 2),
            _ => None,
        }
    }

    /// A short human label for the art kind, e.g. "ANSi", "XBin", "TundraDraw".
    pub fn kind_label(&self) -> &'static str {
        match self.data_type {
            0 => "None",
            1 => match self.file_type {
                0 => "ASCII",
                1 => "ANSi",
                2 => "ANSiMation",
                3 => "RIPScript",
                4 => "PCBoard",
                5 => "Avatar",
                6 => "HTML",
                7 => "Source",
                8 => "TundraDraw",
                _ => "Character",
            },
            2 => "Bitmap",
            3 => "Vector",
            4 => "Audio",
            5 => "BinaryText",
            6 => "XBin",
            7 => "Archive",
            8 => "Executable",
            _ => "Unknown",
        }
    }

    /// `CCYYMMDD` → `YYYY-MM-DD`, or the raw string if it isn't 8 digits.
    pub fn date_pretty(&self) -> String {
        let d = &self.date;
        if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) {
            format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8])
        } else {
            d.clone()
        }
    }
}

/// Byte offset of the SAUCE record's start, if the file has one. The spec puts it
/// in the **last 128 bytes**, but some editors/transfers append a stray EOL or EOF
/// after it, so we scan a few trailing bytes back for the `SAUCE00` magic — but only
/// accept it when everything *after* the 128-byte record is whitespace/control, so a
/// literal "SAUCE00" inside art content can't be mistaken for the record.
fn sauce_offset(data: &[u8]) -> Option<usize> {
    const MAX_TRAIL: usize = 8; // tolerate up to this many trailing CR/LF/NUL/EOF/space
    let len = data.len();
    if len < 128 {
        return None;
    }
    let max_k = MAX_TRAIL.min(len - 128);
    (0..=max_k).map(|k| len - 128 - k).find(|&start| {
        data[start..].starts_with(b"SAUCE00") && data[start + 128..].iter().all(|&b| b <= 0x20)
    })
}

/// Parse the trailing SAUCE record, if present. `data` is the whole file.
pub fn parse(data: &[u8]) -> Option<Sauce> {
    let start = sauce_offset(data)?;
    let s = &data[start..start + 128];
    // Fields are CP437 text, space- (or NUL-) padded; trim that trailing padding.
    let field = |off: usize, len: usize| -> String {
        let raw = &s[off..off + len];
        let end = raw
            .iter()
            .rposition(|&b| b != b' ' && b != 0)
            .map(|p| p + 1)
            .unwrap_or(0);
        String::from_utf8_lossy(&raw[..end]).trim().to_string()
    };
    // The COMNT block (`comments` × 64-char lines, preceded by "COMNT") sits just before
    // the SAUCE record; join its lines into one description string.
    let comments = s[104];
    let comment = (comments > 0)
        .then(|| {
            let len = 5 + comments as usize * 64;
            start
                .checked_sub(len)
                .filter(|&o| data[o..].starts_with(b"COMNT"))
                .map(|o| {
                    data[o + 5..o + len]
                        .chunks(64)
                        .map(|line| String::from_utf8_lossy(line).trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
        })
        .flatten()
        .unwrap_or_default();
    Some(Sauce {
        title: field(7, 35),
        author: field(42, 20),
        group: field(62, 20),
        date: field(82, 8),
        data_type: s[94],
        file_type: s[95],
        tinfo1: u16::from_le_bytes([s[96], s[97]]),
        tinfo2: u16::from_le_bytes([s[98], s[99]]),
        comments,
        ice: s[105] & 0x01 != 0,
        font: field(106, 22),
        comment,
    })
}

/// The art data with any SAUCE record + COMNT block (+ a preceding DOS EOF)
/// stripped, so the trailer isn't decoded as image content. Unchanged when there's
/// no SAUCE — truncating at an arbitrary `0x1A` would cut art that uses it as a
/// CP437 glyph (→).
pub fn strip(data: &[u8]) -> &[u8] {
    let Some(sauce_start) = sauce_offset(data) else {
        return data;
    };
    let comments = data[sauce_start + 104] as usize; // byte 104 = comment-line count
    let mut cut = sauce_start;
    if comments > 0 {
        let comnt_len = 5 + comments * 64;
        if cut >= comnt_len && data[cut - comnt_len..].starts_with(b"COMNT") {
            cut -= comnt_len;
        }
    }
    // Drop the DOS EOF marker(s) — sometimes a run of them — before the record.
    while cut > 0 && data[cut - 1] == 0x1A {
        cut -= 1;
    }
    &data[..cut]
}

/// Build a 128-byte SAUCE record for an exported textmode file. `title` is the piece
/// name (source stem); `cols`/`rows` the canvas size; `format` selects the DataType/
/// FileType pair — 0 = ANSi (1/1), 1 = XBin (6/0), 2 = TundraDraw (1/8); `ice` sets
/// the non-blink TFlag; `font_8x8` picks the 8×8 (VGA50) letter-spacing + font name;
/// `date_ccyymmdd` is today as `CCYYMMDD` (or non-8-char → left blank); `filesize` is
/// the byte length of the file content BEFORE the EOF + this record. Mirrors the
/// offsets [`parse`] reads back.
#[allow(clippy::too_many_arguments)]
pub fn sauce_record(
    title: &str,
    cols: u16,
    rows: u16,
    format: u8,
    ice: bool,
    font_8x8: bool,
    date_ccyymmdd: &str,
    filesize: u32,
) -> [u8; 128] {
    let mut s = [b' '; 128]; // text fields are space-padded
    s[..5].copy_from_slice(b"SAUCE");
    s[5..7].copy_from_slice(b"00");
    // Write a space-padded string field (truncated to `len`).
    let put = |s: &mut [u8; 128], off: usize, len: usize, val: &str| {
        let b = val.as_bytes();
        let n = b.len().min(len);
        s[off..off + n].copy_from_slice(&b[..n]);
        for i in off + n..off + len {
            s[i] = b' ';
        }
    };
    put(&mut s, 7, 35, title); // Title
    put(&mut s, 42, 20, "kaleidotron"); // Author
    put(&mut s, 62, 20, ""); // Group
    // Date[8] = CCYYMMDD; leave the space padding if we couldn't format one.
    let d = date_ccyymmdd.as_bytes();
    if d.len() == 8 {
        s[82..90].copy_from_slice(d);
    }
    s[90..94].copy_from_slice(&filesize.to_le_bytes()); // FileSize u32 LE
    let (data_type, file_type) = match format {
        1 => (6u8, 0u8), // XBin
        2 => (1u8, 8u8), // TundraDraw (Character / TundraDraw)
        _ => (1u8, 1u8), // ANSi
    };
    s[94] = data_type;
    s[95] = file_type;
    s[96..98].copy_from_slice(&cols.to_le_bytes()); // TInfo1 = width
    s[98..100].copy_from_slice(&rows.to_le_bytes()); // TInfo2 = height
    s[100..104].copy_from_slice(&[0, 0, 0, 0]); // TInfo3 / TInfo4
    s[104] = 0; // Comments
    // TFlags: bit0 iCE/non-blink; bits1–2 letter-spacing (10 = 9px VGA, 01 = 8px VGA50).
    let mut tflags = 0u8;
    if ice {
        tflags |= 0x01;
    }
    tflags |= if font_8x8 { 0b01 << 1 } else { 0b10 << 1 };
    s[105] = tflags;
    // TInfoS[22] = font name, NUL-padded.
    for b in s[106..128].iter_mut() {
        *b = 0;
    }
    let font = if font_8x8 { "IBM VGA50" } else { "IBM VGA" };
    let fb = font.as_bytes();
    let n = fb.len().min(22);
    s[106..106 + n].copy_from_slice(&fb[..n]);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(set: impl Fn(&mut [u8])) -> Vec<u8> {
        let mut s = vec![0u8; 128];
        s[..7].copy_from_slice(b"SAUCE00");
        set(&mut s);
        s
    }

    #[test]
    fn parses_fields_and_hints() {
        let s = rec(|s| {
            s[7..14].copy_from_slice(b"My Art ");
            s[42..47].copy_from_slice(b"Adam ");
            s[62..67].copy_from_slice(b"ACiD ");
            s[82..90].copy_from_slice(b"19941005");
            s[94] = 1; // Character
            s[95] = 1; // ANSi
            s[96] = 80; // width
            s[105] = 0x01; // iCE
        });
        let p = parse(&s).unwrap();
        assert_eq!(p.title, "My Art");
        assert_eq!(p.author, "Adam");
        assert_eq!(p.group, "ACiD");
        assert_eq!(p.kind_label(), "ANSi");
        assert_eq!(p.char_width(), Some(80));
        assert!(p.ice);
        assert_eq!(p.date_pretty(), "1994-10-05");
    }

    #[test]
    fn binarytext_width_comes_from_filetype_times_two() {
        // A .BIN is DataType 5 (BinaryText); there's no header width, so SAUCE encodes it
        // as FileType × 2. 33-N1.BIN has FileType 250 → a 500-column canvas (not the 160
        // default, which sheared it). Guards the BinaryText branch of char_width().
        let s = rec(|s| {
            s[94] = 5; // BinaryText
            s[95] = 250; // FileType → width / 2
        });
        let p = parse(&s).unwrap();
        assert_eq!(p.kind_label(), "BinaryText");
        assert_eq!(p.char_width(), Some(500));
    }

    #[test]
    fn none_without_record() {
        assert_eq!(parse(b"just some text"), None);
    }

    #[test]
    fn parses_comnt_block_into_comment() {
        // A 2-line COMNT block (each 64 chars, space-padded) sits immediately before the
        // SAUCE record; byte 104 holds the line count. parse() should join the lines.
        let mut comnt = b"COMNT".to_vec();
        for line in ["a fine piece of work", "by yours truly"] {
            let mut buf = [b' '; 64];
            buf[..line.len()].copy_from_slice(line.as_bytes());
            comnt.extend_from_slice(&buf);
        }
        let mut file = b"ART\x1a".to_vec();
        file.extend(comnt);
        file.extend(rec(|s| {
            s[7..14].copy_from_slice(b"Titled ");
            s[94] = 1; // Character
            s[104] = 2; // two comment lines
        }));
        let p = parse(&file).expect("SAUCE with COMNT parses");
        assert_eq!(p.comments, 2);
        assert_eq!(p.comment, "a fine piece of work by yours truly");
        // strip() drops the COMNT block + record + EOF, leaving just the art.
        assert_eq!(strip(&file), b"ART");
    }

    #[test]
    fn tolerates_trailing_eol_after_record() {
        // Some editors append a stray CRLF (or EOF) AFTER the 128-byte SAUCE record
        // (gj-borg.ans does). The record is then at len-130, not len-128 — parse and
        // strip must still find it, else the SAUCE renders as on-screen text.
        let mut file = b"ART\x1a".to_vec(); // art + DOS EOF
        file.extend(rec(|s| {
            s[7..14].copy_from_slice(b"Borg   ");
            s[94] = 1; // Character
        }));
        file.extend_from_slice(b"\r\n"); // <-- the stray trailing EOL

        let p = parse(&file).expect("SAUCE found despite trailing CRLF");
        assert_eq!(p.title, "Borg");
        // strip drops the EOF + SAUCE + trailing junk, leaving just the art.
        assert_eq!(strip(&file), b"ART");
    }

    #[test]
    fn rejects_relaxed_match_when_trailing_bytes_are_real_data() {
        // "SAUCE00" sits before len-128, but the two trailing bytes are real content
        // ("XY"), not a stray EOL — the guard must reject it, else we'd strip art that
        // merely contains the string.
        let mut file = b"SAUCE00".to_vec();
        file.extend(vec![0u8; 121]); // complete a 128-byte region after the magic
        file.extend_from_slice(b"XY"); // non-whitespace trailing → not a stray EOL
        assert_eq!(file.len(), 130);
        assert_eq!(parse(&file), None);
        assert_eq!(strip(&file), &file[..]);
    }

    #[test]
    fn writer_round_trips_through_parser() {
        // A record built by sauce_record must read back with the same fields — guards
        // the byte offsets against the parser (the source of truth for reading).
        let s = sauce_record("MyPiece", 80, 25, 0, true, false, "20260817", 4096);
        let mut file = b"ART\x1a".to_vec();
        file.extend_from_slice(&s);
        let p = parse(&file).expect("written SAUCE parses");
        assert_eq!(p.title, "MyPiece");
        assert_eq!(p.author, "kaleidotron");
        assert_eq!(p.kind_label(), "ANSi");
        assert_eq!(p.char_width(), Some(80));
        assert_eq!(p.tinfo2, 25);
        assert!(p.ice);
        assert_eq!(p.date_pretty(), "2026-08-17");
        assert_eq!(p.font, "IBM VGA");
        // XBin variant (format 1): DataType 6, 8×8 font name.
        let x = sauce_record("X", 40, 12, 1, false, true, "20260817", 10);
        let xf = x.to_vec();
        let px = parse(&xf).expect("xbin SAUCE parses");
        assert_eq!(px.kind_label(), "XBin");
        assert_eq!(px.font, "IBM VGA50");
        assert!(!px.ice);
        // TundraDraw variant (format 2): DataType 1 / FileType 8 → "TundraDraw".
        let t = sauce_record("T", 100, 50, 2, false, false, "20260817", 20);
        let tf = t.to_vec();
        let pt = parse(&tf).expect("tundra SAUCE parses");
        assert_eq!(pt.data_type, 1);
        assert_eq!(pt.file_type, 8);
        assert_eq!(pt.kind_label(), "TundraDraw");
        assert_eq!(pt.char_width(), Some(100));
        assert_eq!(pt.tinfo2, 50);
    }
}
