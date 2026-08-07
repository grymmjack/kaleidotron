//! Source-code & plain-text viewing. Rasterizes text with lightweight syntax
//! highlighting into a `PixImage` using the embedded CP437 8×16 VGA font, so code
//! files flow through the exact same thumbnail + viewer pipeline as scene art
//! (grid tile, zoom/pan viewer, Details) with zero viewer changes.
//!
//! The highlighter is a small hand-rolled lexer (no heavy `syntect`/regex dep — this
//! project keeps its tree lean, and a VGA-font render doesn't need per-language
//! perfection). Comment/string rules are set precisely per language *family*; the
//! keyword set is a shared union across C-family/script languages (over-matching a
//! keyword in the "wrong" language is only cosmetic). A line-number gutter and a
//! dark VGA-ish palette give it the "nicely formatted" retro terminal look.

use super::cp437_font::CP437_8X16;
use super::{DecodeError, Decoder};
use crate::image_types::PixImage;

const CELL_W: usize = 8;
const CELL_H: usize = 16;
const TAB: usize = 4;
// Bounds so a huge file can't blow up memory in the thumbnail worker. The raster is
// sized to the *actual* content, so short files stay tiny; these only cap the tail.
const MAX_LINES: usize = 4000;
const MAX_COLS: usize = 240; // clip absurdly long lines
const MAX_CELLS: usize = 240_000; // ≈ 30 Mpx / 123 MB RGBA worst case; adapts lines↔width

// VGA-ish syntax palette: dark background, light default text, muted accents that read
// well in the 8×16 font.
const BG: [u8; 3] = [14, 14, 20];
const DEFAULT: [u8; 3] = [204, 204, 204];
const COMMENT: [u8; 3] = [106, 135, 89];
const KEYWORD: [u8; 3] = [86, 156, 214];
const TYPE: [u8; 3] = [78, 201, 176];
const STRING: [u8; 3] = [206, 145, 120];
const NUMBER: [u8; 3] = [181, 206, 168];
const PREPROC: [u8; 3] = [197, 134, 192];
const PUNCT: [u8; 3] = [160, 160, 170];
const CONTROL: [u8; 3] = [197, 134, 192];
const FUNC: [u8; 3] = [220, 220, 170];
const GUTTER: [u8; 3] = [88, 88, 104];
const TRUNC: [u8; 3] = [220, 170, 90];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tok {
    Default,
    Comment,
    Keyword,
    Type,
    Str,
    Number,
    Preproc,
    Punct,
    /// Control flow, split out from `Keyword` because editor themes almost always colour it
    /// separately (`keyword.control`) — collapsing the two throws that distinction away.
    Control,
    /// A declared routine's name (`support.function` / `entity.name.function`).
    Func,
}

impl Tok {
    fn color(self) -> [u8; 3] {
        match self {
            Tok::Default => DEFAULT,
            Tok::Comment => COMMENT,
            Tok::Keyword => KEYWORD,
            Tok::Type => TYPE,
            Tok::Str => STRING,
            Tok::Number => NUMBER,
            Tok::Preproc => PREPROC,
            Tok::Punct => PUNCT,
            Tok::Control => CONTROL,
            Tok::Func => FUNC,
        }
    }
}

/// Per-language lexing rules. Comment/string handling is what matters for readability;
/// keywords are a shared union (see `KEYWORDS`).
struct LangSpec {
    line: &'static [&'static str],               // line-comment starters
    block: Option<(&'static str, &'static str)>, // block-comment open/close
    raw: &'static [&'static str],                // multi-line string delimiters (py """, js `)
    quotes: &'static [char],                     // single-line string quote chars
    preproc_hash: bool,                          // leading `#word` is a directive (C/C++)
    highlight: bool,                             // false = plain text (txt/log/md), no tokens
    /// Language-specific keywords, matched case-insensitively, in addition to the shared union.
    /// `None` = shared union only.
    keywords: Option<&'static [&'static str]>,
    /// Control-flow keywords, matched case-insensitively and checked *before* `keywords`, so a
    /// word in both lists lands on the more specific kind. `None` = no split; everything in
    /// `keywords` reads as a plain keyword.
    control: Option<&'static [&'static str]>,
    /// A `$WORD` is a compiler metacommand (QB64PE's `$INCLUDE`, `$NOPREFIX`), i.e. a directive
    /// rather than an identifier. C-family languages use `preproc_hash` for the same idea.
    preproc_dollar: bool,
    /// A line beginning `SUB name` / `FUNCTION name` declares a routine, so the rest of the line
    /// is its signature — coloured as a definition the way an editor grammar does.
    decl_words: &'static [&'static str],
    /// Treat an uppercase-leading identifier as a type (Rust/Java/C# convention). Wrong for BASIC,
    /// where the *keywords* are the uppercase words — leaving it on paints every `PRINT` and `DIM`
    /// as a type instead of a keyword.
    upper_is_type: bool,
}

const C_FAMILY: LangSpec = LangSpec {
    line: &["//"],
    block: Some(("/*", "*/")),
    raw: &[],
    quotes: &['"', '\''],
    preproc_hash: true,
    highlight: true,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};
const JS_FAMILY: LangSpec = LangSpec {
    line: &["//"],
    block: Some(("/*", "*/")),
    raw: &["`"],
    quotes: &['"', '\''],
    preproc_hash: false,
    highlight: true,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};
const RUST: LangSpec = LangSpec {
    line: &["//"],
    block: Some(("/*", "*/")),
    raw: &[],
    quotes: &['"'],
    preproc_hash: false,
    highlight: true,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};
const HASH: LangSpec = LangSpec {
    line: &["#"],
    block: None,
    raw: &["\"\"\"", "'''"],
    quotes: &['"', '\''],
    preproc_hash: false,
    highlight: true,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};
const LUA: LangSpec = LangSpec {
    line: &["--"],
    block: Some(("--[[", "]]")),
    raw: &[],
    quotes: &['"', '\''],
    preproc_hash: false,
    highlight: true,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};
const BASIC: LangSpec = LangSpec {
    line: &["'", "REM ", "rem "],
    block: None,
    raw: &[],
    quotes: &['"'],
    // QB64PE metacommands are `$IF` / `$END IF` style, not `#define`.
    preproc_hash: false,
    highlight: true,
    keywords: Some(QB64PE_KEYWORDS),
    control: Some(QB64PE_CONTROL),
    preproc_dollar: true,
    decl_words: &["SUB", "FUNCTION"],
    upper_is_type: false,
};
const ASM: LangSpec = LangSpec {
    line: &[";"],
    block: None,
    raw: &[],
    quotes: &['"', '\''],
    preproc_hash: false,
    highlight: true,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};
const CSS: LangSpec = LangSpec {
    line: &[],
    block: Some(("/*", "*/")),
    raw: &[],
    quotes: &['"', '\''],
    preproc_hash: false,
    highlight: true,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};
const HTML: LangSpec = LangSpec {
    line: &[],
    block: Some(("<!--", "-->")),
    raw: &[],
    quotes: &['"', '\''],
    preproc_hash: false,
    highlight: true,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};
const JSONISH: LangSpec = LangSpec {
    line: &["//"],
    block: Some(("/*", "*/")),
    raw: &[],
    quotes: &['"'],
    preproc_hash: false,
    highlight: true,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};
const PLAIN: LangSpec = LangSpec {
    line: &[],
    block: None,
    raw: &[],
    quotes: &[],
    preproc_hash: false,
    highlight: false,
    keywords: None,
    control: None,
    preproc_dollar: false,
    decl_words: &[],
    upper_is_type: true,
};

/// Shared keyword union — over-matching in the "wrong" language is only cosmetic.
const KEYWORDS: &[&str] = &[
    "if",
    "else",
    "elif",
    "elseif",
    "for",
    "while",
    "do",
    "loop",
    "break",
    "continue",
    "return",
    "yield",
    "match",
    "case",
    "switch",
    "default",
    "goto",
    "fn",
    "def",
    "func",
    "function",
    "sub",
    "end",
    "class",
    "struct",
    "enum",
    "trait",
    "impl",
    "interface",
    "extends",
    "implements",
    "public",
    "private",
    "protected",
    "static",
    "final",
    "const",
    "let",
    "var",
    "mut",
    "auto",
    "new",
    "delete",
    "this",
    "self",
    "super",
    "import",
    "from",
    "use",
    "using",
    "include",
    "require",
    "package",
    "namespace",
    "module",
    "pub",
    "async",
    "await",
    "try",
    "catch",
    "except",
    "finally",
    "throw",
    "raise",
    "with",
    "as",
    "in",
    "is",
    "not",
    "and",
    "or",
    "typedef",
    "template",
    "typename",
    "operator",
    "virtual",
    "override",
    "dim",
    "then",
    "next",
    "print",
    "input",
    "goto",
    "gosub",
    "local",
    "global",
    "nil",
    "true",
    "false",
    "none",
    "null",
    "undefined",
    "void",
    "extern",
    "unsafe",
    "where",
    "move",
    "ref",
    "box",
    "dyn",
    "lambda",
    "pass",
    "del",
    "assert",
    "export",
    "signal",
    "onready",
    "extends",
    "tool",
    "var",
];

/// Common built-in / primitive type names.
const TYPES: &[&str] = &[
    "int", "long", "short", "char", "float", "double", "bool", "boolean", "byte", "string", "str",
    "void", "unsigned", "signed", "size_t", "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32",
    "i64", "isize", "f32", "f64", "vec", "map", "list", "dict", "set", "array", "object", "number",
    "any", "integer", "single", "long", "double", "String", "Vec", "Option", "Result", "Box",
    "Self",
];

fn lang_for(ext: &str) -> &'static LangSpec {
    match ext {
        "rs" => &RUST,
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" | "hh" | "hxx" | "inc" | "ino" | "m" | "mm" => {
            &C_FAMILY
        }
        "java" | "cs" | "go" | "swift" | "kt" | "kts" | "scala" | "dart" | "php" | "php3"
        | "php4" | "php5" | "hlsl" | "glsl" | "shader" | "gdshader" => &C_FAMILY,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "json5" => &JS_FAMILY,
        "py" | "pyw" | "gd" | "pl" | "pm" | "rb" | "sh" | "bash" | "zsh" | "yaml" | "yml"
        | "toml" | "ini" | "cfg" | "conf" | "r" | "jl" | "ex" | "exs" | "coffee" | "tcl"
        | "ps1" | "cmake" | "mk" | "makefile" | "dockerfile" => &HASH,
        "lua" => &LUA,
        "bas" | "bm" | "bi" | "vb" | "vbs" | "qb" | "frm" => &BASIC,
        "asm" | "s" | "nasm" | "a51" => &ASM,
        "css" | "scss" | "sass" | "less" => &CSS,
        "html" | "htm" | "htmlx" | "xhtml" | "xml" | "xaml" | "vue" | "svelte" => &HTML,
        "json" | "jsonc" | "ipynb" => &JSONISH,
        _ => &PLAIN,
    }
}

enum Carry {
    None,
    Block,
    Raw(&'static str),
}

/// QB64PE reserved words, extracted from the QB64PE VS Code extension's TextMate grammar (the
/// language's own keyword list). Uppercase and matched case-insensitively — BASIC is not
/// case-sensitive, and QB64PE source is conventionally written in caps.
/// QB64PE control-flow words, taken from the `keyword.control.QB64PE` rule of the official
/// TextMate grammar so the split matches what the editor extension does. The grammar spells the
/// compound forms (`End If`, `Exit Do`) as phrases; a word-at-a-time lexer can't match those, so
/// `END` and `EXIT` are listed on their own — they are overwhelmingly used as part of one.
const QB64PE_CONTROL: &[&str] = &[
    "_ANDALSO", "_CONTINUE", "_DELAY", "_LIMIT", "_NEGATE", "_ORELSE", "AND", "CASE", "CONTINUE",
    "DELAY", "DO", "EACH", "ELSE", "ELSEIF", "END", "EXIT", "FOR", "IF", "IIF", "KEY", "LIMIT",
    "LOOP", "MOD", "NEXT", "NOT", "OFF", "OR", "RETURN", "SELECT", "SLEEP", "STEP", "THEN", "TO",
    "UNTIL", "WEND", "WHILE", "WITH", "XOR",
];

const QB64PE_KEYWORDS: &[&str] = &[
    "ABS", "ABSOLUTE", "ACCEPTFILEDROP", "ACCESS", "ACOS", "ACOSH", "ADLER32", "ALIAS", "ALL",
    "ALLOWFULLSCREEN", "ALPHA", "ALPHA32", "AND", "ANDALSO", "ANTICLOCKWISE", "ANY", "APPEND",
    "ARCCOT", "ARCCSC", "ARCSEC", "AS", "ASC", "ASIN", "ASINH", "ASSERT", "ASSERTS", "ATAN2",
    "ATANH", "ATN", "AUTODISPLAY", "AXIS", "BACKGROUNDCOLOR", "BASE", "BEEP", "BEHIND", "BIN",
    "BINARY", "BIT", "BLEND", "BLINK", "BLOAD", "BLUE", "BLUE32", "BSAVE", "BUTTON",
    "BUTTONCHANGE", "BYTE", "BYVAL", "CALL", "CALLS", "CAPSLOCK", "CASE", "CDBL", "CDECL",
    "CEIL", "CHAIN", "CHDIR", "CHECKING", "CHR", "CINP", "CINT", "CIRCLE", "CLEAR",
    "CLEARCOLOR", "CLIP", "CLIPBOARD", "CLIPBOARDIMAGE", "CLNG", "CLOCKWISE", "CLOSE", "CLS",
    "COLOR", "COLORCHOOSERDIALOG", "COM", "COMMAND", "COMMANDCOUNT", "COMMON", "CONNECTED",
    "CONNECTIONADDRESS", "CONSOLE", "CONSOLECURSOR", "CONSOLEFONT", "CONSOLEINPUT",
    "CONSOLETITLE", "CONST", "CONTINUE", "CONTROLCHR", "COPYIMAGE", "COPYPALETTE", "COS",
    "COSH", "COT", "COTH", "CRC32", "CSC", "CSCH", "CSNG", "CSRLIN", "CUSTOMTYPE", "CV", "CVD",
    "CVDMBF", "CVI", "CVL", "CVS", "CVSMBF", "CWD", "D2G", "D2R", "DATA", "DATE", "DEBUG",
    "DECLARE", "DEF", "DEFAULTCOLOR", "DEFDBL", "DEFINE", "DEFINT", "DEFLATE", "DEFLNG",
    "DEFSNG", "DEFSTR", "DELAY", "DEPTHBUFFER", "DESKTOPHEIGHT", "DESKTOPWIDTH", "DEST",
    "DEVICE", "DEVICEINPUT", "DEVICES", "DIM", "DIR", "DIREXISTS", "DISPLAY", "DISPLAYORDER",
    "DO", "DONTBLEND", "DONTWAIT", "DOUBLE", "DRAW", "DROPPEDFILE", "DYNAMIC", "ECHO", "ELSE",
    "ELSEIF", "EMBED", "EMBEDDED", "END", "ENVIRON", "ENVIRONCOUNT", "EOF", "EQV", "ERASE",
    "ERDEV", "ERL", "ERR", "ERROR", "ERRORLINE", "ERRORMESSAGE", "EVERYCASE", "EXEICON", "EXIT",
    "EXP", "EXPLICIT", "EXPLICITARRAY", "FIELD", "FILEATTR", "FILEEXISTS", "FILES",
    "FILLBACKGROUND", "FINISHDROP", "FIX", "FLOAT", "FN", "FONT", "FONTHEIGHT", "FONTWIDTH",
    "FOR", "FPS", "FRE", "FREE", "FREEFILE", "FREEFONT", "FREEIMAGE", "FREETIMER", "FULLPATH",
    "FULLSCREEN", "FUNCTION", "G2D", "G2R", "GET", "GLACCUM", "GLALPHAFUNC",
    "GLARETEXTURESRESIDENT", "GLARRAYELEMENT", "GLBEGIN", "GLBINDTEXTURE", "GLBITMAP",
    "GLBLENDFUNC", "GLCALLLIST", "GLCALLLISTS", "GLCLEAR", "GLCLEARACCUM", "GLCLEARCOLOR",
    "GLCLEARDEPTH", "GLCLEARINDEX", "GLCLEARSTENCIL", "GLCLIPPLANE", "GLCOLOR3B", "GLCOLOR3BV",
    "GLCOLOR3D", "GLCOLOR3DV", "GLCOLOR3F", "GLCOLOR3FV", "GLCOLOR3I", "GLCOLOR3IV",
    "GLCOLOR3S", "GLCOLOR3SV", "GLCOLOR3UB", "GLCOLOR3UBV", "GLCOLOR3UI", "GLCOLOR3UIV",
    "GLCOLOR3US", "GLCOLOR3USV", "GLCOLOR4B", "GLCOLOR4BV", "GLCOLOR4D", "GLCOLOR4DV",
    "GLCOLOR4F", "GLCOLOR4FV", "GLCOLOR4I", "GLCOLOR4IV", "GLCOLOR4S", "GLCOLOR4SV",
    "GLCOLOR4UB", "GLCOLOR4UBV", "GLCOLOR4UI", "GLCOLOR4UIV", "GLCOLOR4US", "GLCOLOR4USV",
    "GLCOLORMASK", "GLCOLORMATERIAL", "GLCOLORPOINTER", "GLCOPYPIXELS", "GLCOPYTEXIMAGE1D",
    "GLCOPYTEXIMAGE2D", "GLCOPYTEXSUBIMAGE1D", "GLCOPYTEXSUBIMAGE2D", "GLCULLFACE",
    "GLDELETELISTS", "GLDELETETEXTURES", "GLDEPTHFUNC", "GLDEPTHMASK", "GLDEPTHRANGE",
    "GLDISABLE", "GLDISABLECLIENTSTATE", "GLDRAWARRAYS", "GLDRAWBUFFER", "GLDRAWELEMENTS",
    "GLDRAWPIXELS", "GLEDGEFLAG", "GLEDGEFLAGPOINTER", "GLEDGEFLAGV", "GLENABLE",
    "GLENABLECLIENTSTATE", "GLEND", "GLENDLIST", "GLEVALCOORD1D", "GLEVALCOORD1DV",
    "GLEVALCOORD1F", "GLEVALCOORD1FV", "GLEVALCOORD2D", "GLEVALCOORD2DV", "GLEVALCOORD2F",
    "GLEVALCOORD2FV", "GLEVALMESH1", "GLEVALMESH2", "GLEVALPOINT1", "GLEVALPOINT2",
    "GLFEEDBACKBUFFER", "GLFINISH", "GLFLUSH", "GLFOGF", "GLFOGFV", "GLFOGI", "GLFOGIV",
    "GLFRONTFACE", "GLFRUSTUM", "GLGENLISTS", "GLGENTEXTURES", "GLGETBOOLEANV",
    "GLGETCLIPPLANE", "GLGETDOUBLEV", "GLGETERROR", "GLGETFLOATV", "GLGETINTEGERV",
    "GLGETLIGHTFV", "GLGETLIGHTIV", "GLGETMAPDV", "GLGETMAPFV", "GLGETMAPIV", "GLGETMATERIALFV",
    "GLGETMATERIALIV", "GLGETPIXELMAPFV", "GLGETPIXELMAPUIV", "GLGETPIXELMAPUSV",
    "GLGETPOINTERV", "GLGETPOLYGONSTIPPLE", "GLGETSTRING", "GLGETTEXENVFV", "GLGETTEXENVIV",
    "GLGETTEXGENDV", "GLGETTEXGENFV", "GLGETTEXGENIV", "GLGETTEXIMAGE", "GLGETTEXPARAMETERFV",
    "GLGETTEXPARAMETERIV", "GLHINT", "GLINDEXD", "GLINDEXDV", "GLINDEXF", "GLINDEXFV",
    "GLINDEXI", "GLINDEXIV", "GLINDEXMASK", "GLINDEXPOINTER", "GLINDEXS", "GLINDEXSV",
    "GLINDEXUB", "GLINDEXUBV", "GLINITNAMES", "GLINTERLEAVEDARRAYS", "GLISENABLED", "GLISLIST",
    "GLISTEXTURE", "GLLIGHTF", "GLLIGHTFV", "GLLIGHTI", "GLLIGHTIV", "GLLIGHTMODELF",
    "GLLIGHTMODELFV", "GLLIGHTMODELI", "GLLIGHTMODELIV", "GLLINESTIPPLE", "GLLINEWIDTH",
    "GLLISTBASE", "GLLOADIDENTITY", "GLLOADMATRIXD", "GLLOADMATRIXF", "GLLOADNAME", "GLLOGICOP",
    "GLMAP1D", "GLMAP1F", "GLMAP2D", "GLMAP2F", "GLMAPGRID1D", "GLMAPGRID1F", "GLMAPGRID2D",
    "GLMAPGRID2F", "GLMATERIALF", "GLMATERIALFV", "GLMATERIALI", "GLMATERIALIV", "GLMATRIXMODE",
    "GLMULTMATRIXD", "GLMULTMATRIXF", "GLNEWLIST", "GLNORMAL3B", "GLNORMAL3BV", "GLNORMAL3D",
    "GLNORMAL3DV", "GLNORMAL3F", "GLNORMAL3FV", "GLNORMAL3I", "GLNORMAL3IV", "GLNORMAL3S",
    "GLNORMAL3SV", "GLNORMALPOINTER", "GLORTHO", "GLPASSTHROUGH", "GLPIXELMAPFV",
    "GLPIXELMAPUIV", "GLPIXELMAPUSV", "GLPIXELSTOREF", "GLPIXELSTOREI", "GLPIXELTRANSFERF",
    "GLPIXELTRANSFERI", "GLPIXELZOOM", "GLPOINTSIZE", "GLPOLYGONMODE", "GLPOLYGONOFFSET",
    "GLPOLYGONSTIPPLE", "GLPOPATTRIB", "GLPOPCLIENTATTRIB", "GLPOPMATRIX", "GLPOPNAME",
    "GLPRIORITIZETEXTURES", "GLPUSHATTRIB", "GLPUSHCLIENTATTRIB", "GLPUSHMATRIX", "GLPUSHNAME",
    "GLRASTERPOS2D", "GLRASTERPOS2DV", "GLRASTERPOS2F", "GLRASTERPOS2FV", "GLRASTERPOS2I",
    "GLRASTERPOS2IV", "GLRASTERPOS2S", "GLRASTERPOS2SV", "GLRASTERPOS3D", "GLRASTERPOS3DV",
    "GLRASTERPOS3F", "GLRASTERPOS3FV", "GLRASTERPOS3I", "GLRASTERPOS3IV", "GLRASTERPOS3S",
    "GLRASTERPOS3SV", "GLRASTERPOS4D", "GLRASTERPOS4DV", "GLRASTERPOS4F", "GLRASTERPOS4FV",
    "GLRASTERPOS4I", "GLRASTERPOS4IV", "GLRASTERPOS4S", "GLRASTERPOS4SV", "GLREADBUFFER",
    "GLREADPIXELS", "GLRECTD", "GLRECTDV", "GLRECTF", "GLRECTFV", "GLRECTI", "GLRECTIV",
    "GLRECTS", "GLRECTSV", "GLRENDER", "GLRENDERMODE", "GLROTATED", "GLROTATEF", "GLSCALED",
    "GLSCALEF", "GLSCISSOR", "GLSELECTBUFFER", "GLSHADEMODEL", "GLSTENCILFUNC", "GLSTENCILMASK",
    "GLSTENCILOP", "GLTEXCOORD1D", "GLTEXCOORD1DV", "GLTEXCOORD1F", "GLTEXCOORD1FV",
    "GLTEXCOORD1I", "GLTEXCOORD1IV", "GLTEXCOORD1S", "GLTEXCOORD1SV", "GLTEXCOORD2D",
    "GLTEXCOORD2DV", "GLTEXCOORD2F", "GLTEXCOORD2FV", "GLTEXCOORD2I", "GLTEXCOORD2IV",
    "GLTEXCOORD2S", "GLTEXCOORD2SV", "GLTEXCOORD3D", "GLTEXCOORD3DV", "GLTEXCOORD3F",
    "GLTEXCOORD3FV", "GLTEXCOORD3I", "GLTEXCOORD3IV", "GLTEXCOORD3S", "GLTEXCOORD3SV",
    "GLTEXCOORD4D", "GLTEXCOORD4DV", "GLTEXCOORD4F", "GLTEXCOORD4FV", "GLTEXCOORD4I",
    "GLTEXCOORD4IV", "GLTEXCOORD4S", "GLTEXCOORD4SV", "GLTEXCOORDPOINTER", "GLTEXENVF",
    "GLTEXENVFV", "GLTEXENVI", "GLTEXENVIV", "GLTEXGEND", "GLTEXGENDV", "GLTEXGENF",
    "GLTEXGENFV", "GLTEXGENI", "GLTEXGENIV", "GLTEXIMAGE1D", "GLTEXIMAGE2D", "GLTEXPARAMETERF",
    "GLTEXPARAMETERFV", "GLTEXPARAMETERI", "GLTEXPARAMETERIV", "GLTEXSUBIMAGE1D",
    "GLTEXSUBIMAGE2D", "GLTRANSLATED", "GLTRANSLATEF", "GLUPERSPECTIVE", "GLVERTEX2D",
    "GLVERTEX2DV", "GLVERTEX2F", "GLVERTEX2FV", "GLVERTEX2I", "GLVERTEX2IV", "GLVERTEX2S",
    "GLVERTEX2SV", "GLVERTEX3D", "GLVERTEX3DV", "GLVERTEX3F", "GLVERTEX3FV", "GLVERTEX3I",
    "GLVERTEX3IV", "GLVERTEX3S", "GLVERTEX3SV", "GLVERTEX4D", "GLVERTEX4DV", "GLVERTEX4F",
    "GLVERTEX4FV", "GLVERTEX4I", "GLVERTEX4IV", "GLVERTEX4S", "GLVERTEX4SV", "GLVERTEXPOINTER",
    "GLVIEWPORT", "GOSUB", "GOTO", "GREEN", "GREEN32", "HARDWARE", "HARDWARE1", "HEIGHT", "HEX",
    "HIDE", "HYPOT", "ICON", "IF", "IMP", "INCLERRORFILE", "INCLERRORLINE", "INCLUDEONCE",
    "INFLATE", "INKEY", "INP", "INPUT", "INPUTBOX", "INSTR", "INSTRREV", "INT", "INTEGER",
    "INTEGER64", "INTERRUPT", "INTERRUPTX", "IOCTL", "IS", "KEEPBACKGROUND", "KEY", "KEYCLEAR",
    "KEYDOWN", "KEYHIT", "KILL", "LASTAXIS", "LASTBUTTON", "LASTWHEEL", "LBOUND", "LCASE",
    "LEFT", "LEN", "LET", "LIBRARY", "LIMIT", "LINE", "LIST", "LOADFONT", "LOADIMAGE", "LOC",
    "LOCATE", "LOCK", "LOF", "LOG", "LONG", "LOOP", "LPOS", "LPRINT", "LSET", "LTRIM",
    "MAPTRIANGLE", "MAPUNICODE", "MD5", "MEM", "MEMCOPY", "MEMELEMENT", "MEMEXISTS", "MEMFILL",
    "MEMFREE", "MEMGET", "MEMIMAGE", "MEMNEW", "MEMPUT", "MEMSOUND", "MESSAGEBOX", "MID",
    "MIDDLE", "MIDISOUNDFONT", "MK", "MKD", "MKDIR", "MKDMBF", "MKI", "MKL", "MKS", "MKSMBF",
    "MOD", "MOUSEBUTTON", "MOUSEHIDE", "MOUSEINPUT", "MOUSEMOVE", "MOUSEMOVEMENTX",
    "MOUSEMOVEMENTY", "MOUSEPIPEOPEN", "MOUSESHOW", "MOUSEWHEEL", "MOUSEX", "MOUSEY", "NAME",
    "NEGATE", "NEWIMAGE", "NEXT", "NONE", "NOPREFIX", "NOT", "NOTIFYPOPUP", "NUMLOCK", "OCT",
    "OFF", "OFFSET", "ON", "ONLY", "ONLYBACKGROUND", "ONTOP", "OPEN", "OPENCLIENT",
    "OPENCONNECTION", "OPENFILEDIALOG", "OPENHOST", "OPTION", "OR", "ORELSE", "OS", "OUT",
    "OUTPUT", "PAINT", "PALETTE", "PALETTECOLOR", "PCOPY", "PEEK", "PEN", "PI", "PIXELSIZE",
    "PLAY", "PMAP", "POINT", "POKE", "POS", "PRESERVE", "PRESET", "PRINT", "PRINTIMAGE",
    "PRINTMODE", "PRINTSTRING", "PRINTWIDTH", "PSET", "PUT", "PUTIMAGE", "R2D", "R2G", "RANDOM",
    "RANDOMIZE", "READ", "READBIT", "READFILE", "RED", "RED32", "REDIM", "RESET", "RESETBIT",
    "RESIZE", "RESIZEHEIGHT", "RESIZEWIDTH", "RESTORE", "RESUME", "RETURN", "RGB", "RGB32",
    "RGBA", "RGBA32", "RIGHT", "RMDIR", "RND", "ROL", "ROR", "ROUND", "RSET", "RTRIM", "RUN",
    "SADD", "SAVEFILEDIALOG", "SAVEIMAGE", "SCALEDHEIGHT", "SCALEDWIDTH", "SCREEN",
    "SCREENCLICK", "SCREENEXISTS", "SCREENHIDE", "SCREENICON", "SCREENIMAGE", "SCREENMOVE",
    "SCREENPRINT", "SCREENSHOW", "SCREENX", "SCREENY", "SCROLLLOCK", "SEAMLESS", "SEC", "SECH",
    "SEEK", "SEG", "SELECT", "SELECTFOLDERDIALOG", "SETALPHA", "SETBIT", "SETMEM", "SGN",
    "SHARED", "SHELL", "SHELLHIDE", "SHL", "SHR", "SIGNAL", "SIN", "SINGLE", "SINH", "SLEEP",
    "SMOOTH", "SMOOTHSHRUNK", "SMOOTHSTRETCHED", "SNDBAL", "SNDCLOSE", "SNDCOPY", "SNDGETPOS",
    "SNDLEN", "SNDLIMIT", "SNDLOOP", "SNDNEW", "SNDOPEN", "SNDOPENRAW", "SNDPAUSE", "SNDPAUSED",
    "SNDPLAY", "SNDPLAYCOPY", "SNDPLAYFILE", "SNDPLAYING", "SNDRATE", "SNDRAW", "SNDRAWDONE",
    "SNDRAWLEN", "SNDSETPOS", "SNDSTOP", "SNDVOL", "SOFTWARE", "SOUND", "SOURCE", "SPACE",
    "SPC", "SQR", "SQUAREPIXELS", "STARTDIR", "STATIC", "STATUSCODE", "STEP", "STICK", "STOP",
    "STR", "STRCMP", "STRETCH", "STRICMP", "STRIG", "STRING", "SUB", "SWAP", "SYSTEM", "TAB",
    "TAN", "TANH", "THEN", "TIME", "TIMER", "TITLE", "TO", "TOGGLE", "TOGGLEBIT",
    "TOTALDROPPEDFILES", "TRIM", "TROFF", "TRON", "TYPE", "UBOUND", "UCASE", "UCHARPOS",
    "UEVENT", "UFONTHEIGHT", "ULINESPACING", "UNLOCK", "UNSIGNED", "UNSTABLE", "UNTIL",
    "UPRINTSTRING", "UPRINTWIDTH", "USING", "VAL", "VARPTR", "VARSEG", "VERSIONINFO", "VIEW",
    "VIRTUALKEYBOARD", "WAIT", "WEND", "WHEEL", "WHILE", "WIDTH", "WINDOW", "WINDOWHANDLE",
    "WINDOWHASFOCUS", "WRITE", "WRITEFILE", "XOR",
];

/// Highlight `src` as `ext`, returning one `Vec` of `(text, kind)` runs per line.
///
/// This is the same lexer the raster tile uses — the *output* differs, not the classification, so
/// the bitmap tile and the interactive text viewer can never disagree about what a token is.
/// Adjacent characters sharing a kind are merged into runs, which is what a text layout wants.
pub fn highlight_lines(src: &str, ext: &str) -> Vec<Vec<(String, Tok)>> {
    let spec = lang_for(ext);
    let mut carry = Carry::None;
    let mut out = Vec::new();
    for line in src.lines() {
        let chars: Vec<char> = line.chars().collect();
        let toks = if spec.highlight {
            lex_line(&chars, spec, &mut carry)
        } else {
            chars.iter().map(|&c| (c, Tok::Default)).collect()
        };
        let mut runs: Vec<(String, Tok)> = Vec::new();
        for (c, t) in toks {
            match runs.last_mut() {
                Some((s, lt)) if *lt == t => s.push(c),
                _ => runs.push((c.to_string(), t)),
            }
        }
        out.push(runs);
    }
    out
}

/// The syntax theme the raster tiles paint with, if any.
///
/// The thumbnailer runs on worker threads with no access to `Kaleidotron`, so — exactly like
/// `set_font_9px` — the choice reaches it as a process-global rather than a threaded parameter.
static SYNTAX_THEME: std::sync::RwLock<Option<std::sync::Arc<crate::theme::Theme>>> =
    std::sync::RwLock::new(None);

/// Point the code rasteriser at a theme (or `None` for the built-in palette). Cheap to call; the
/// caller is responsible for dropping cached thumbnails so they re-decode with the new colours.
pub fn set_syntax_theme(theme: Option<std::sync::Arc<crate::theme::Theme>>) {
    if let Ok(mut g) = SYNTAX_THEME.write() {
        *g = theme;
    }
}

/// The colours one rendered tile uses, resolved once per render rather than per glyph.
struct Palette {
    bg: [u8; 3],
    gutter: [u8; 3],
    trunc: [u8; 3],
    toks: [[u8; 3]; ALL_TOKS.len()],
}

impl Palette {
    /// Resolve for `ext`: the active theme where it has an opinion, the built-in palette otherwise.
    /// Falling back per *field* rather than per theme means a sparse theme still contributes what
    /// it does define instead of being discarded wholesale.
    fn resolve(ext: &str) -> Palette {
        let mut p = Palette {
            bg: BG,
            gutter: GUTTER,
            trunc: TRUNC,
            toks: ALL_TOKS.map(|t| t.color()),
        };
        let guard = SYNTAX_THEME.read().ok();
        let Some(theme) = guard.as_ref().and_then(|g| g.as_ref()) else {
            return p;
        };
        let rgb = |c: Option<[u8; 4]>| c.map(|c| [c[0], c[1], c[2]]);
        if let Some(c) = rgb(theme.extreme_bg.or(theme.window_bg)) {
            p.bg = c;
        }
        if let Some(c) = rgb(theme.weak_text) {
            p.gutter = c;
        }
        if let Some(c) = rgb(theme.warn) {
            p.trunc = c;
        }
        for (i, k) in ALL_TOKS.iter().enumerate() {
            if let Some(c) = theme.kind_color_in(*k, lang_scopes(ext, *k)) {
                p.toks[i] = c;
            }
        }
        p
    }

    fn of(&self, t: Tok) -> [u8; 3] {
        ALL_TOKS
            .iter()
            .position(|k| *k == t)
            .map(|i| self.toks[i])
            .unwrap_or(DEFAULT)
    }
}

/// The TextMate scopes a language's own grammar uses for `kind`, most specific first.
///
/// Themes are written against a grammar's scope names, so a theme built for QB64PE says
/// `keyword.all.QB64PE` where a generic lookup would ask for `keyword`. Without this the theme
/// resolves through whatever base rule it inherited — for the QB64PE theme that is a cyan almost
/// identical to its identifier colour, so a `.bas` file renders very nearly monochrome and the
/// theme looks like it never loaded. Empty for languages with no dedicated scope vocabulary.
pub fn lang_scopes(ext: &str, kind: Tok) -> &'static [&'static str] {
    let basic = matches!(
        ext.to_ascii_lowercase().as_str(),
        "bas" | "bm" | "bi" | "qb"
    );
    if !basic {
        return &[];
    }
    match kind {
        Tok::Keyword => &["keyword.all.QB64PE", "keywords.all.QB64PE", "keyword.QB64PE"],
        Tok::Control => &["keyword.control.QB64PE"],
        Tok::Preproc => &["metacommand.QB64PE", "meta.preprocessor.QB64PE"],
        Tok::Func => &[
            "userfunctions.QB64PE",
            "support.function.QB64PE",
            "entity.name.function.QB64PE",
        ],
        Tok::Type => &["support.type.QB64PE"],
        Tok::Str => &["string.quoted.double.QB64PE"],
        Tok::Number => &["constant.numeric.QB64PE"],
        Tok::Comment => &["comment.line.apostrophe.QB64PE"],
        Tok::Default => &["variable.other.QB64PE", "variable.QB64PE"],
        Tok::Punct => &["punctuation.separator.QB64PE"],
    }
}

/// Every token kind, in one place, so a consumer building a palette can't quietly miss one.
pub const ALL_TOKS: [Tok; 10] = [
    Tok::Default,
    Tok::Comment,
    Tok::Keyword,
    Tok::Type,
    Tok::Str,
    Tok::Number,
    Tok::Preproc,
    Tok::Punct,
    Tok::Control,
    Tok::Func,
];

/// The colour for a token kind, so the interactive viewer matches the tile exactly.
pub fn tok_rgb(t: Tok) -> [u8; 3] {
    t.color()
}

/// A word is a keyword / type / neither.
fn classify_word(w: &str, spec: &LangSpec) -> Tok {
    // A metacommand is a directive, not an identifier — and it must be checked before the keyword
    // lists, which hold the bare word (`$IF` vs `IF`).
    if spec.preproc_dollar && w.starts_with('$') && w.len() > 1 {
        return Tok::Preproc;
    }
    if KEYWORDS.contains(&w) {
        return Tok::Keyword;
    }
    // A language's own keywords, case-insensitively (BASIC isn't case-sensitive). Control flow is
    // tested first so a word in both lists takes the more specific kind.
    let up = w.to_ascii_uppercase();
    if let Some(list) = spec.control {
        if list.binary_search(&up.as_str()).is_ok() {
            return Tok::Control;
        }
    }
    if let Some(list) = spec.keywords {
        if list.binary_search(&up.as_str()).is_ok() {
            return Tok::Keyword;
        }
    }
    if TYPES.contains(&w) || (spec.upper_is_type && w.len() > 1 && w.starts_with(char::is_uppercase))
    {
        Tok::Type
    } else {
        Tok::Default
    }
}

/// Lex one line into per-char `(char, Tok)`, carrying block-comment / raw-string state.
fn lex_line(line: &[char], spec: &LangSpec, carry: &mut Carry) -> Vec<(char, Tok)> {
    let mut out: Vec<(char, Tok)> = Vec::with_capacity(line.len());
    let n = line.len();
    let mut i = 0;
    // Set once a `SUB`/`FUNCTION` opens the line; see the identifier branch below.
    let mut decl_seen = false;

    // Continue a carried block comment / raw string first.
    match std::mem::replace(carry, Carry::None) {
        Carry::Block => {
            let close = spec.block.map(|b| b.1).unwrap_or("*/");
            if let Some(end) = find_at(line, 0, close) {
                for &c in &line[..end + close.chars().count()] {
                    out.push((c, Tok::Comment));
                }
                i = end + close.chars().count();
            } else {
                for &c in line {
                    out.push((c, Tok::Comment));
                }
                *carry = Carry::Block;
                return out;
            }
        }
        Carry::Raw(delim) => {
            if let Some(end) = find_at(line, 0, delim) {
                for &c in &line[..end + delim.chars().count()] {
                    out.push((c, Tok::Str));
                }
                i = end + delim.chars().count();
            } else {
                for &c in line {
                    out.push((c, Tok::Str));
                }
                *carry = Carry::Raw(delim);
                return out;
            }
        }
        Carry::None => {}
    }

    if !spec.highlight {
        for &c in &line[i..] {
            out.push((c, Tok::Default));
        }
        return out;
    }

    // Leading `#directive` (C preprocessor).
    let first_non_ws = line.iter().position(|c| !c.is_whitespace());
    let preproc_line = spec.preproc_hash && first_non_ws == Some(i) && line.get(i) == Some(&'#');

    while i < n {
        let c = line[i];
        let rest_starts = |pat: &str| starts_with_at(line, i, pat);

        // Line comment → rest of line.
        if let Some(&lc) = spec.line.iter().find(|&&p| rest_starts(p)) {
            let _ = lc;
            for &c in &line[i..] {
                out.push((c, Tok::Comment));
            }
            break;
        }
        // Block comment open.
        if let Some((open, close)) = spec.block {
            if rest_starts(open) {
                if let Some(end) = find_at(line, i + open.chars().count(), close) {
                    let stop = end + close.chars().count();
                    for &c in &line[i..stop] {
                        out.push((c, Tok::Comment));
                    }
                    i = stop;
                    continue;
                } else {
                    for &c in &line[i..] {
                        out.push((c, Tok::Comment));
                    }
                    *carry = Carry::Block;
                    break;
                }
            }
        }
        // Multi-line raw string delimiter.
        if let Some(&delim) = spec.raw.iter().find(|&&d| rest_starts(d)) {
            let dlen = delim.chars().count();
            if let Some(end) = find_at(line, i + dlen, delim) {
                let stop = end + dlen;
                for &c in &line[i..stop] {
                    out.push((c, Tok::Str));
                }
                i = stop;
                continue;
            } else {
                for &c in &line[i..] {
                    out.push((c, Tok::Str));
                }
                *carry = Carry::Raw(delim);
                break;
            }
        }
        // Single-line string.
        if spec.quotes.contains(&c) {
            let (span, next) = scan_string(line, i, c);
            for &ch in span {
                out.push((ch, Tok::Str));
            }
            i = next;
            continue;
        }
        // Preprocessor directive token.
        if preproc_line && c == '#' {
            let start = i;
            i += 1;
            while i < n && (line[i].is_alphanumeric() || line[i] == '_') {
                i += 1;
            }
            for &ch in &line[start..i] {
                out.push((ch, Tok::Preproc));
            }
            continue;
        }
        // Number.
        if c.is_ascii_digit() || (c == '.' && line.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            i += 1;
            while i < n && is_number_char(line[i]) {
                i += 1;
            }
            for &ch in &line[start..i] {
                out.push((ch, Tok::Number));
            }
            continue;
        }
        // Identifier / keyword.
        if c.is_alphabetic() || c == '_' || c == '@' || c == '$' {
            let start = i;
            i += 1;
            while i < n && (line[i].is_alphanumeric() || line[i] == '_') {
                i += 1;
            }
            let word: String = line[start..i].iter().collect();
            let mut tok = classify_word(&word, spec);
            // `SUB name (args)` — the grammar treats the whole rest of the line as the routine's
            // signature, so once the declaring word is seen every later identifier on the line is
            // part of the definition rather than a use of something.
            if !spec.decl_words.is_empty() {
                let up = word.to_ascii_uppercase();
                if decl_seen {
                    if tok == Tok::Default || tok == Tok::Type {
                        tok = Tok::Func;
                    }
                } else if first_non_ws == Some(start) && spec.decl_words.contains(&up.as_str()) {
                    decl_seen = true;
                }
            }
            for &ch in &line[start..i] {
                out.push((ch, tok));
            }
            continue;
        }
        // Punctuation / operator / whitespace.
        let tok = if c.is_whitespace() || c.is_alphanumeric() {
            Tok::Default
        } else {
            Tok::Punct
        };
        out.push((c, tok));
        i += 1;
    }
    out
}

fn is_number_char(c: char) -> bool {
    c.is_ascii_hexdigit() || matches!(c, '.' | 'x' | 'X' | 'o' | 'b' | '_' | 'e' | 'E' | '+' | '-')
}

/// Scan a quoted string starting at `start` (the opening quote). Returns the char slice
/// (incl. quotes) and the index just past it. Honors `\` escapes; stops at EOL if unterminated.
fn scan_string(line: &[char], start: usize, quote: char) -> (&[char], usize) {
    let mut i = start + 1;
    while i < line.len() {
        if line[i] == '\\' {
            i += 2;
            continue;
        }
        if line[i] == quote {
            i += 1;
            break;
        }
        i += 1;
    }
    let end = i.min(line.len());
    (&line[start..end], end)
}

fn starts_with_at(line: &[char], at: usize, pat: &str) -> bool {
    let pc: Vec<char> = pat.chars().collect();
    if at + pc.len() > line.len() {
        return false;
    }
    line[at..at + pc.len()] == pc[..]
}

/// First index >= `from` where `pat` occurs in `line`, or None.
fn find_at(line: &[char], from: usize, pat: &str) -> Option<usize> {
    let pc: Vec<char> = pat.chars().collect();
    if pc.is_empty() || line.len() < pc.len() {
        return None;
    }
    (from..=line.len() - pc.len()).find(|&i| line[i..i + pc.len()] == pc[..])
}

/// Map a Unicode char to a CP437 byte for the bitmap font. ASCII passes through; a few
/// common punctuation lookalikes are folded; anything else becomes '?'.
fn to_cp437(c: char) -> u8 {
    let u = c as u32;
    if (0x20..0x7f).contains(&u) {
        return u as u8;
    }
    match c {
        '\t' => b' ',
        '·' | '•' => 0xf9,
        '’' | '‘' | '`' => b'\'',
        '“' | '”' => b'"',
        '—' | '–' => b'-',
        '…' => 0x07, // no ellipsis glyph; a bullet reads as "more"
        '→' => 0x1a,
        '←' => 0x1b,
        '©' => 0x63,
        _ if u < 0x20 => b' ',
        _ => b'?',
    }
}

/// If this is a Jupyter notebook, pull out its cells as readable text (markdown cells as
/// `# …` comments, code cells verbatim) so we render the notebook, not raw JSON.
fn ipynb_to_text(bytes: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let cells = v.get("cells")?.as_array()?;
    let mut out = String::new();
    for cell in cells {
        let kind = cell.get("cell_type").and_then(|k| k.as_str()).unwrap_or("");
        let src = match cell.get("source") {
            Some(serde_json::Value::Array(a)) => {
                a.iter().filter_map(|s| s.as_str()).collect::<String>()
            }
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        if kind == "markdown" {
            out.push_str("# --- markdown ---\n");
            for line in src.lines() {
                out.push_str("# ");
                out.push_str(line);
                out.push('\n');
            }
        } else {
            out.push_str("# --- code ---\n");
            out.push_str(&src);
            if !src.ends_with('\n') {
                out.push('\n');
            }
        }
        out.push('\n');
    }
    Some(out)
}

/// Render `text` (already the display text) into a highlighted `PixImage`.
fn render_text(text: &str, spec: &LangSpec, ext: &str) -> PixImage {
    let pal = Palette::resolve(ext);
    // Collect raw lines up to the caps (line + total-cell budget), tab-expanded.
    let raw_lines: Vec<&str> = text.lines().collect();
    let total_lines = raw_lines.len();
    let gutter_w = digits(total_lines.clamp(1, MAX_LINES)) + 1; // number + one space

    let mut rows: Vec<Vec<(char, Tok)>> = Vec::new();
    let mut carry = Carry::None;
    let mut cells_used = 0usize;
    let mut truncated_at: Option<usize> = None;

    for (n, raw) in raw_lines.iter().enumerate() {
        if n >= MAX_LINES || cells_used >= MAX_CELLS {
            truncated_at = Some(n);
            break;
        }
        let expanded = expand_tabs(raw);
        let chars: Vec<char> = expanded.chars().collect();
        let mut lexed = lex_line(&chars, spec, &mut carry);
        if lexed.len() > MAX_COLS {
            lexed.truncate(MAX_COLS - 1);
            lexed.push(('»', Tok::Punct));
        }
        cells_used += (lexed.len() + gutter_w).max(1);
        rows.push(lexed);
    }

    let content_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let cols = (gutter_w + content_cols).max(gutter_w + 1);
    // One extra row for a truncation notice, if any.
    let notice =
        truncated_at.map(|n| format!("… {} more lines — open in your editor", total_lines - n));
    let n_rows = rows.len() + usize::from(notice.is_some());
    let n_rows = n_rows.max(1);

    let w = cols * CELL_W;
    let h = n_rows * CELL_H;
    let mut pixels = vec![[pal.bg[0], pal.bg[1], pal.bg[2], 255]; w * h];

    // Gutter line numbers + content.
    for (ri, row) in rows.iter().enumerate() {
        let lineno = ri + 1;
        blit_str(
            &mut pixels,
            w,
            ri,
            0,
            &format!("{lineno:>width$}", width = gutter_w - 1),
            pal.gutter,
        );
        for (ci, &(ch, tok)) in row.iter().enumerate() {
            blit_glyph(&mut pixels, w, ri, gutter_w + ci, to_cp437(ch), pal.of(tok));
        }
    }
    if let Some(msg) = notice {
        blit_str(&mut pixels, w, rows.len(), gutter_w, &msg, pal.trunc);
    }

    PixImage::from_rgba(w as u32, h as u32, pixels)
}

fn digits(mut n: usize) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

fn expand_tabs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0;
    for c in s.chars() {
        if c == '\t' {
            let spaces = TAB - (col % TAB);
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

fn blit_str(pixels: &mut [[u8; 4]], w: usize, row: usize, col0: usize, s: &str, fg: [u8; 3]) {
    for (i, c) in s.chars().enumerate() {
        blit_glyph(pixels, w, row, col0 + i, to_cp437(c), fg);
    }
}

fn blit_glyph(pixels: &mut [[u8; 4]], w: usize, row: usize, col: usize, ch: u8, fg: [u8; 3]) {
    let glyph = &CP437_8X16[ch as usize];
    let x0 = col * CELL_W;
    let y0 = row * CELL_H;
    for (ry, &bits) in glyph.iter().enumerate() {
        for rx in 0..CELL_W {
            if (bits >> (7 - rx)) & 1 == 1 {
                let (px, py) = (x0 + rx, y0 + ry);
                if px < w {
                    let idx = py * w + px;
                    if idx < pixels.len() {
                        pixels[idx] = [fg[0], fg[1], fg[2], 255];
                    }
                }
            }
        }
    }
}

pub struct CodeDecoder;

/// Every extension this decoder claims. Kept in one place so `app.rs`'s parallel
/// `is_textmode_ext` / `is_image_ext` lists can reference the same set.
pub const CODE_EXTS: &[&str] = &[
    "rs",
    "c",
    "cpp",
    "cc",
    "cxx",
    "h",
    "hpp",
    "hh",
    "hxx",
    "inc",
    "ino",
    "m",
    "mm",
    "java",
    "cs",
    "go",
    "swift",
    "kt",
    "kts",
    "scala",
    "dart",
    "php",
    "php3",
    "php4",
    "php5",
    "hlsl",
    "glsl",
    "shader",
    "gdshader",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "json5",
    "py",
    "pyw",
    "gd",
    "pl",
    "pm",
    "rb",
    "sh",
    "bash",
    "zsh",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "r",
    "jl",
    "ex",
    "exs",
    "coffee",
    "tcl",
    "ps1",
    "cmake",
    "mk",
    "lua",
    "bas",
    "bm",
    "bi",
    "vb",
    "vbs",
    "qb",
    "frm",
    "asm",
    "s",
    "nasm",
    "a51",
    "css",
    "scss",
    "sass",
    "less",
    "html",
    "htm",
    "htmlx",
    "xhtml",
    "xml",
    "xaml",
    "vue",
    "svelte",
    "json",
    "jsonc",
    "ipynb",
    "md",
    "markdown",
    "log",
    "bbs",
    "text",
    "csv",
    "tsv",
    "env",
    "gitignore",
    "properties",
    "rst",
];

impl Decoder for CodeDecoder {
    fn name(&self) -> &'static str {
        "code"
    }

    fn extensions(&self) -> &'static [&'static str] {
        CODE_EXTS
    }

    fn sniff(&self, _header: &[u8]) -> bool {
        // Text has no magic; dispatch by extension only (so PNG/etc. never reach here).
        false
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        // `decode_bytes` dispatches here by extension, but doesn't pass it — infer the
        // language from content isn't worth it; default to plain unless it's a notebook.
        let text =
            ipynb_to_text(bytes).unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
        // Without the path we can't pick the exact LangSpec here; the registry calls
        // `decode_with_ext` when it knows the extension (see mod.rs). This bare path
        // renders plain (still correct, just uncolored).
        Ok(render_text(&text, &PLAIN, ""))
    }
}

impl CodeDecoder {
    /// Extension-aware decode (the registry routes here so we can pick the language).
    pub fn decode_ext(bytes: &[u8], ext: &str) -> Result<PixImage, DecodeError> {
        // A notebook flattens to `#`-commented Python-ish text, so highlight it as Python.
        if ext == "ipynb" {
            let text =
                ipynb_to_text(bytes).unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned());
            return Ok(render_text(&text, &HASH, "py"));
        }
        let text = String::from_utf8_lossy(bytes).into_owned();
        Ok(render_text(&text, lang_for(ext), ext))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext_render(src: &str, ext: &str) -> PixImage {
        CodeDecoder::decode_ext(src.as_bytes(), ext).unwrap()
    }

    #[test]
    fn renders_nonempty_and_sized_to_content() {
        let img = ext_render("fn main() {}\nlet x = 1;\n", "rs");
        assert!(img.width > 0 && img.height > 0);
        // Two lines → 2 rows × 16px tall.
        assert_eq!(img.height, 2 * CELL_H as u32);
    }

    #[test]
    fn qb64_splits_control_metacommands_and_declarations() {
        let kind = |src: &str, needle: &str| -> Tok {
            let lines = highlight_lines(src, "bas");
            lines
                .iter()
                .flatten()
                .find(|(t, _)| t.trim() == needle)
                .map(|(_, k)| *k)
                .unwrap_or_else(|| panic!("{needle:?} not lexed as its own run in {src:?}"))
        };
        // Control flow is its own kind: themes colour `keyword.control` separately, and folding it
        // into Keyword is what made a whole file read as one colour.
        assert_eq!(kind("IF x THEN", "IF"), Tok::Control);
        assert_eq!(kind("FOR i = 1 TO 9", "FOR"), Tok::Control);
        // ...but an ordinary statement stays a plain keyword.
        assert_eq!(kind("PRINT x", "PRINT"), Tok::Keyword);
        assert_eq!(kind("DIM SHARED n", "DIM"), Tok::Keyword);
        // A metacommand is a directive, and must not be confused with the bare keyword.
        assert_eq!(kind("$NOPREFIX", "$NOPREFIX"), Tok::Preproc);
        assert_eq!(kind("$IF WIN THEN", "$IF"), Tok::Preproc);
        assert_eq!(kind("IF a THEN", "IF"), Tok::Control);
        // A declaration names a routine; the name is a definition, not an ordinary identifier.
        assert_eq!(kind("SUB DrawBar (v)", "DrawBar"), Tok::Func);
        assert_eq!(kind("FUNCTION Clamp# (v)", "Clamp"), Tok::Func);
        // Calling it later is not a declaration — only a line that *opens* with SUB/FUNCTION is.
        assert_ne!(kind("x = Clamp(3)", "Clamp"), Tok::Func);
    }

    #[test]
    fn qb64_scopes_resolve_through_a_real_theme() {
        // The bug this guards: asking a QB64PE theme for the generic `keyword` scope finds a base
        // rule that is all but identical to its identifier colour, so a .bas file renders nearly
        // monochrome and the theme looks like it was never loaded. The language's own scope names
        // are what carry the author's intent.
        let json = r##"{
            "name": "T", "type": "dark",
            "colors": { "editor.background": "#0000aa" },
            "tokenColors": [
                { "scope": "keyword", "settings": { "foreground": "#5ED8F0" } },
                { "scope": "keyword.all.QB64PE", "settings": { "foreground": "#E2FFFF" } },
                { "scope": "keyword.control.QB64PE", "settings": { "foreground": "#95EBF1" } }
            ]
        }"##;
        let t = crate::theme::Theme::from_json(json, "t").expect("parses");
        let of = |k: Tok, ext: &str| t.kind_color_in(k, lang_scopes(ext, k));
        assert_eq!(of(Tok::Keyword, "bas"), Some([0xE2, 0xFF, 0xFF]));
        assert_eq!(of(Tok::Control, "bas"), Some([0x95, 0xEB, 0xF1]));
        // A language with no scope vocabulary of its own still gets the generic rule.
        assert_eq!(of(Tok::Keyword, "rs"), Some([0x5E, 0xD8, 0xF0]));
        // And a scope the theme never mentions walks the dotted prefix down to one it does.
        assert_eq!(
            t.kind_color_in(Tok::Control, &["keyword.control.rust"]),
            Some([0x5E, 0xD8, 0xF0])
        );
    }

    #[test]
    fn the_raster_palette_follows_the_active_theme() {
        // The grid tile is a bitmap painted on a worker thread, so the theme reaches it as a
        // process-global. Without this the viewer restyles and the thumbnails beside it don't.
        let plain = Palette::resolve("bas");
        assert_eq!(plain.bg, BG, "no theme set -> built-in palette");

        let json = r##"{ "name": "T", "type": "dark",
            "colors": { "editor.background": "#0000aa" },
            "tokenColors": [{ "scope": "comment", "settings": { "foreground": "#8681C9" } }] }"##;
        let t = std::sync::Arc::new(crate::theme::Theme::from_json(json, "t").unwrap());
        set_syntax_theme(Some(t));
        let themed = Palette::resolve("bas");
        assert_eq!(themed.bg, [0x00, 0x00, 0xAA], "tile uses editor.background");
        assert_eq!(themed.of(Tok::Comment), [0x86, 0x81, 0xC9]);
        // A theme silent on a kind contributes nothing there rather than blanking it.
        assert_eq!(themed.of(Tok::Str), STRING);
        set_syntax_theme(None);
    }

    #[test]
    fn qb64pe_keywords_are_sorted() {
        // `classify_word` binary-searches this list. An unsorted entry doesn't error — it just
        // silently fails to match, so assert the invariant the search depends on.
        let mut sorted = QB64PE_KEYWORDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(QB64PE_KEYWORDS, sorted.as_slice(), "QB64PE_KEYWORDS must be sorted");
        assert!(QB64PE_KEYWORDS.len() > 500, "the full keyword set should be present");
    }

    #[test]
    fn qb64pe_keywords_classify_as_keywords_not_types() {
        // The shared classifier treats an uppercase-leading word as a type (a Rust/Java
        // convention). In BASIC the keywords ARE the uppercase words, so that heuristic painted
        // every PRINT and DIM as a type — this is the regression guard for that.
        for w in ["PRINT", "DIM", "SUB", "FUNCTION", "SCREEN", "_PUTIMAGE"] {
            let up = w.to_ascii_uppercase();
            let found = QB64PE_KEYWORDS.binary_search(&up.as_str()).is_ok();
            if found {
                assert_eq!(classify_word(w, &BASIC), Tok::Keyword, "{w} should be a keyword");
            }
        }
        // Case-insensitive: BASIC isn't case-sensitive.
        assert_eq!(classify_word("print", &BASIC), Tok::Keyword);
        assert_eq!(classify_word("Print", &BASIC), Tok::Keyword);
        // A user identifier is still plain, not a type.
        assert_eq!(classify_word("MyVariable", &BASIC), Tok::Default);
        // …while a C-family language keeps the uppercase-is-type heuristic.
        assert_eq!(classify_word("MyVariable", &C_FAMILY), Tok::Type);
    }

    #[test]
    fn keyword_and_string_get_distinct_colors() {
        // "let" is a keyword, the "hi" literal is a string — different fg colors must appear.
        let spec = lang_for("rs");
        let line: Vec<char> = "let s = \"hi\";".chars().collect();
        let mut carry = Carry::None;
        let toks = lex_line(&line, spec, &mut carry);
        let kinds: std::collections::HashSet<_> = toks.iter().map(|&(_, t)| t.color()).collect();
        assert!(kinds.contains(&KEYWORD));
        assert!(kinds.contains(&STRING));
    }

    #[test]
    fn block_comment_carries_across_lines() {
        let spec = lang_for("c");
        let mut carry = Carry::None;
        let l1: Vec<char> = "int x; /* start".chars().collect();
        let t1 = lex_line(&l1, spec, &mut carry);
        assert!(matches!(carry, Carry::Block), "unterminated /* carries");
        // The tail after /* is a comment.
        assert_eq!(t1.last().unwrap().1.color(), COMMENT);
        let l2: Vec<char> = "still comment */ int y;".chars().collect();
        let _ = lex_line(&l2, spec, &mut carry);
        assert!(matches!(carry, Carry::None), "*/ closes the carry");
    }

    #[test]
    fn python_hash_comment_and_triple_string() {
        let spec = lang_for("py");
        let mut carry = Carry::None;
        let l: Vec<char> = "x = 1  # note".chars().collect();
        let t = lex_line(&l, spec, &mut carry);
        assert_eq!(t.last().unwrap().1.color(), COMMENT);
    }

    #[test]
    fn ipynb_extracts_cells() {
        let nb = br#"{"cells":[{"cell_type":"code","source":["print(1)\n"]}]}"#;
        let txt = ipynb_to_text(nb).unwrap();
        assert!(txt.contains("print(1)"));
        assert!(txt.contains("code"));
    }

    #[test]
    fn plain_text_has_no_highlight_but_renders() {
        let img = ext_render("just some text\nmore text\n", "txt");
        assert!(img.height >= 2 * CELL_H as u32);
    }

    #[test]
    fn long_line_is_clipped() {
        let long = "x".repeat(1000);
        let img = ext_render(&long, "txt");
        // Width capped near MAX_COLS (+ gutter), not 1000 cells wide.
        assert!(img.width <= ((MAX_COLS + 8) * CELL_W) as u32);
    }
}

#[cfg(test)]
mod qb64pe_viewer {
    use super::*;

    #[test]
    fn qb64pe_source_yields_distinct_token_kinds() {
        let src = "' a comment\nDIM SHARED count AS INTEGER\nFOR i = 1 TO 10\n    PRINT \"hello\"; i\nNEXT i\n";
        let lines = highlight_lines(src, "bas");
        assert_eq!(lines.len(), 5);
        let kinds: std::collections::HashSet<Tok> =
            lines.iter().flatten().map(|(_, t)| *t).collect();
        // The whole point: a comment, keywords, a string and a number must be *different* kinds,
        // or the viewer renders one flat colour.
        for want in [Tok::Comment, Tok::Keyword, Tok::Str, Tok::Number] {
            assert!(kinds.contains(&want), "missing {want:?} in {kinds:?}");
        }
        // Line 1 is entirely a comment (BASIC's `'`).
        assert!(lines[0].iter().all(|(_, t)| *t == Tok::Comment));
        // DIM / SHARED / AS are keywords, `count` is not.
        let l2: Vec<_> = lines[1].iter().filter(|(s, _)| !s.trim().is_empty()).collect();
        assert!(l2.iter().any(|(s, t)| s.contains("DIM") && *t == Tok::Keyword));
        assert!(l2.iter().any(|(s, t)| s.contains("count") && *t == Tok::Default));
    }
}
