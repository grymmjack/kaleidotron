//! XMind (`.xmind`) mind-map renderer.
//!
//! An `.xmind` file is a ZIP. Modern XMind (Zen / 2020+) — and the files produced
//! by the `xmind-mindmap` skill's SDK — keep the authoritative content in
//! **`content.json`**: a JSON array of *sheets*, each with a `rootTopic` whose
//! `children.attached[]` recurse. (The `content.xml` some files also carry is a
//! legacy "open me in XMind 8" placeholder stub — we ignore it.)
//!
//! We don't reproduce every XMind feature (styles, markers, notes, relationships,
//! boundaries, images). The goal is to render the *topic tree* so it reads like an
//! XMind map: a central topic, main branches radiating left/right in per-branch
//! colours, sub-topics as a tidy logic tree, joined by smooth connectors.
//!
//! Pipeline: **unzip → parse tree → lay it out → emit an SVG string → rasterize**.
//! Rasterization reuses the same `resvg`/`usvg`/`tiny-skia` stack as `svg.rs`, but
//! loads the embedded DejaVu Sans into usvg's font database so text actually
//! renders (usvg's default `Options` ship no fonts).

use super::{DecodeError, Decoder};
use crate::image_types::PixImage;
use resvg::{tiny_skia, usvg};
use std::io::Read;

/// UI font, embedded so text renders identically on any machine (no system-font
/// dependency). The family name inside the file is "DejaVu Sans".
const FONT: &[u8] = include_bytes!("../../assets/DejaVuSans.ttf");
const FONT_FAMILY: &str = "DejaVu Sans";

// ---- layout tuning -------------------------------------------------------
const H_GAP: f32 = 44.0; // horizontal space between a parent and its children
const V_GAP: f32 = 12.0; // vertical space between sibling subtrees
const CANVAS_MARGIN: f32 = 40.0; // padding around the whole map
const MAX_NODES: usize = 2500; // guard against a pathological file
const RASTER_TARGET: f32 = 3200.0; // aim the longest side here: supersamples small maps so
                                   // text stays crisp, downscales huge ones (memory bound)

/// Fallback per-branch colour cycle when the file's theme doesn't supply one — a
/// pleasant, XMind-ish rainbow. One colour per top-level branch, inherited by its
/// whole subtree.
const DEFAULT_PALETTE: &[&str] = &[
    "#e0574a", "#f0932b", "#f6c445", "#6ab04c", "#22a6a0", "#3498db", "#6c5ce7", "#b452cd",
    "#e056a0", "#8d6e63",
];
const ROOT_FILL: &str = "#2b3a4a"; // central topic background when the theme gives none
const DEFAULT_BG: &str = "#ffffff";

/// The sheet's own visual theme, read from `content.json`'s `sheet.theme`, so the map
/// renders in the colours the file was authored with (falling back to sane defaults).
struct Theme {
    canvas_bg: String,
    palette: Vec<String>, // per-branch colours
    colorful: bool,       // palette came from `multi-line-colors` (→ filled branch topics)
    central: TStyle,
    main: TStyle,
    sub: TStyle,
    boundary: BStyle,
    rel: RStyle,
}

/// Per-role topic style pulled from a theme entry's `properties`.
#[derive(Default, Clone)]
struct TStyle {
    fill: Option<String>,   // `svg:fill` (None = "none"/"inherited"/absent)
    text: Option<String>,   // `fo:color`
    border: Option<String>, // `border-line-color`
    border_w: f32,          // `border-line-width` in pt (≈px)
    line: Option<String>,   // `line-color` (branch connector)
    rect: bool,             // shape-class is a plain rectangle
}

/// Boundary (a translucent outline grouping some children) style.
struct BStyle {
    line: String,
    fill: String,
    opacity: f32,
    dashed: bool,
    text: String,
}

/// Relationship (a labelled arrow between two topics) style.
struct RStyle {
    line: String,
    text: String,
    dashed: bool,
}

impl Theme {
    /// The colour of top-level branch `i`.
    fn branch(&self, i: usize) -> &str {
        &self.palette[i % self.palette.len()]
    }
}

/// A concrete `#RRGGBB` if `props[key]` is one, else None (`"none"`/`"inherited"`/absent).
fn color_opt(props: &serde_json::Value, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| s.starts_with('#') && (s.len() == 7 || s.len() == 4))
        .map(|s| s.to_string())
}

/// Parse a `"5pt"` / `"3"` width to px (pt≈px at our scale).
fn pt(props: &serde_json::Value, key: &str, default: f32) -> f32 {
    props
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| s.trim_end_matches("pt").trim().parse::<f32>().ok())
        .unwrap_or(default)
}

/// Read one theme role's `properties` into a [`TStyle`].
fn parse_tstyle(theme: &serde_json::Value, key: &str) -> TStyle {
    let Some(p) = theme.get(key).and_then(|t| t.get("properties")) else {
        return TStyle::default();
    };
    let rect = p
        .get("shape-class")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.contains("rectangle") || s.ends_with(".rect"));
    TStyle {
        fill: color_opt(p, "svg:fill"),
        text: color_opt(p, "fo:color"),
        border: color_opt(p, "border-line-color"),
        border_w: pt(p, "border-line-width", 2.0),
        line: color_opt(p, "line-color"),
        rect,
    }
}

/// Build a [`Theme`] from a sheet object. Absent/partial themes fall back to defaults.
fn parse_theme(sheet: &serde_json::Value) -> Theme {
    let theme = sheet
        .get("theme")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let map_props = theme
        .get("map")
        .and_then(|m| m.get("properties"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let canvas_bg = color_opt(&map_props, "svg:fill").unwrap_or_else(|| DEFAULT_BG.to_string());

    // Branch palette: prefer the theme's `multi-line-colors`, then `color-list`, else default.
    let multi = map_props
        .get("multi-line-colors")
        .and_then(|v| v.as_str())
        .filter(|s| s.starts_with('#'));
    let (palette, colorful) = if let Some(list) = multi {
        (
            list.split_whitespace()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
            true,
        )
    } else {
        (Vec::new(), false)
    };

    let central = parse_tstyle(&theme, "centralTopic");
    let main = parse_tstyle(&theme, "mainTopic");
    let sub = parse_tstyle(&theme, "subTopic");

    // Fall back the palette to the theme's single line colour, then the default rainbow.
    let palette = if !palette.is_empty() {
        palette
    } else if let Some(c) = main
        .line
        .clone()
        .or_else(|| main.border.clone())
        .or_else(|| central.line.clone())
    {
        vec![c]
    } else {
        DEFAULT_PALETTE.iter().map(|s| s.to_string()).collect()
    };

    let bprops = theme.get("boundary").and_then(|t| t.get("properties"));
    let boundary = BStyle {
        line: bprops
            .and_then(|p| color_opt(p, "line-color"))
            .unwrap_or_else(|| "#77933C".into()),
        fill: bprops
            .and_then(|p| color_opt(p, "svg:fill"))
            .unwrap_or_else(|| "#C3D69B".into()),
        opacity: bprops
            .and_then(|p| p.get("svg:opacity"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.28),
        dashed: bprops
            .and_then(|p| p.get("line-pattern"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| s != "solid"),
        text: bprops
            .and_then(|p| color_opt(p, "fo:color"))
            .unwrap_or_else(|| "#555555".into()),
    };

    let rprops = theme.get("relationship").and_then(|t| t.get("properties"));
    let rel = RStyle {
        line: rprops
            .and_then(|p| color_opt(p, "line-color"))
            .unwrap_or_else(|| "#8a8a8a".into()),
        text: rprops
            .and_then(|p| color_opt(p, "fo:color"))
            .unwrap_or_else(|| "#595959".into()),
        dashed: rprops
            .and_then(|p| p.get("line-pattern"))
            .and_then(|v| v.as_str())
            .map(|s| s != "solid")
            .unwrap_or(true),
    };

    Theme {
        canvas_bg,
        palette,
        colorful,
        central,
        main,
        sub,
        boundary,
        rel,
    }
}

/// A parsed topic: title + attached children, plus the semantic extras XMind draws
/// (markers, a note indicator, labels, an embedded image, and boundaries grouping
/// some of its children). Styles beyond the theme are dropped.
struct Node {
    id: String,
    title: String,
    markers: Vec<String>, // marker ids, e.g. "priority-1", "task-done", "flag-red"
    has_note: bool,       // a notes block → a small note indicator
    labels: Vec<String>,  // short labels shown under the topic
    image: Option<ImageRef>, // an embedded resource image, above the title
    boundaries: Vec<(usize, usize)>, // child index ranges to outline
    children: Vec<Node>,
}

/// An embedded image resolved to a data URI + its (already display-scaled) size.
#[derive(Clone)]
struct ImageRef {
    data_uri: String,
    w: f32,
    h: f32,
}

/// A relationship line between two topics (by id).
struct Relationship {
    end1: String,
    end2: String,
    title: String,
}

/// Base64-encode (standard alphabet) — for embedding resource images as data URIs.
fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Resolve a topic's `image` object (`{src:"xap:resources/<hash>.png", width, height}`)
/// to an embedded data URI, scaled to a sensible max width.
fn resolve_image(bytes: &[u8], img: &serde_json::Value) -> Option<ImageRef> {
    let src = img.get("src")?.as_str()?;
    let name = src.strip_prefix("xap:").unwrap_or(src); // → "resources/<hash>.png"
    let data = {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
        let mut f = zip.by_name(name).ok()?;
        let mut v = Vec::new();
        f.read_to_end(&mut v).ok()?;
        v
    };
    let lower = name.to_ascii_lowercase();
    let mime = if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "jpeg"
    } else if lower.ends_with(".gif") {
        "gif"
    } else {
        "png"
    };
    let (mut w, mut h) = (
        img.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
        img.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
    );
    if w <= 0.0 || h <= 0.0 {
        let d = image::load_from_memory(&data).ok()?;
        w = d.width() as f32;
        h = d.height() as f32;
    }
    const MAX_W: f32 = 200.0;
    if w > MAX_W {
        let s = MAX_W / w;
        w *= s;
        h *= s;
    }
    Some(ImageRef {
        data_uri: format!("data:image/{mime};base64,{}", base64(&data)),
        w,
        h,
    })
}

/// Parse a `"(a,b)"` child-index range.
fn parse_range(s: &str) -> Option<(usize, usize)> {
    let s = s.trim().trim_start_matches('(').trim_end_matches(')');
    let mut it = s.split(',');
    let a = it.next()?.trim().parse().ok()?;
    let b = it.next()?.trim().parse().ok()?;
    Some((a, b))
}

pub struct XMindDecoder;

impl Decoder for XMindDecoder {
    fn name(&self) -> &'static str {
        "xmind"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["xmind"]
    }

    fn sniff(&self, _header: &[u8]) -> bool {
        // A `.xmind` is a ZIP (`PK\x03\x04`) — magic shared with every other zip
        // (archives, .pvkit, .xrns). Matching it here would hijack those, so we
        // rely purely on the `.xmind` extension fallback in `decode_bytes`.
        false
    }

    fn decode(&self, bytes: &[u8]) -> Result<PixImage, DecodeError> {
        // The grid tile / prev-next path renders the first sheet (what XMind opens to).
        render_xmind_sheet(bytes, 0)
    }
}

/// Parse `content.json` into its array of sheet objects.
fn parse_content(bytes: &[u8]) -> Result<Vec<serde_json::Value>, DecodeError> {
    let json = read_zip_text(bytes, "content.json")
        .ok_or_else(|| DecodeError::Malformed("no content.json in .xmind".into()))?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| DecodeError::Malformed(e.to_string()))?;
    match value {
        serde_json::Value::Array(a) if !a.is_empty() => Ok(a),
        _ => Err(DecodeError::Malformed("content.json has no sheets".into())),
    }
}

/// Display title for sheet `i`: its root topic's title, then the sheet title, then
/// a generic `Sheet N`.
fn sheet_title(sheet: &serde_json::Value, i: usize) -> String {
    let t = sheet
        .get("rootTopic")
        .and_then(|r| r.get("title"))
        .and_then(|t| t.as_str())
        .or_else(|| sheet.get("title").and_then(|t| t.as_str()))
        .unwrap_or("");
    if t.trim().is_empty() {
        format!("Sheet {}", i + 1)
    } else {
        t.to_string()
    }
}

/// The title of every sheet, for the in-app sheet selector. Empty on parse failure
/// (the caller then treats the file as a single sheet).
pub fn xmind_sheet_titles(bytes: &[u8]) -> Vec<String> {
    parse_content(bytes)
        .map(|sheets| {
            sheets
                .iter()
                .enumerate()
                .map(|(i, s)| sheet_title(s, i))
                .collect()
        })
        .unwrap_or_default()
}

/// Render sheet `idx` of the `.xmind`, falling back to XMind's embedded thumbnail if
/// the tree can't be rendered — so the viewer always shows *something* XMind-shaped.
pub fn render_xmind_sheet(bytes: &[u8], idx: usize) -> Result<PixImage, DecodeError> {
    match render_sheet_inner(bytes, idx) {
        Ok(img) => Ok(img),
        Err(primary) => embedded_thumbnail(bytes).map_err(|_| primary),
    }
}

/// Parse sheet `idx`'s topic tree, lay it out, and rasterize.
fn render_sheet_inner(bytes: &[u8], idx: usize) -> Result<PixImage, DecodeError> {
    let sheets = parse_content(bytes)?;
    let sheet = sheets
        .get(idx)
        .or_else(|| sheets.first())
        .ok_or_else(|| DecodeError::Malformed("sheet index out of range".into()))?;
    let root_topic = sheet
        .get("rootTopic")
        .ok_or_else(|| DecodeError::Malformed("sheet has no rootTopic".into()))?;

    let mut budget = MAX_NODES;
    let mut rels = Vec::new();
    let mut root = parse_topic(root_topic, &mut budget, bytes, &mut rels);
    if root.title.trim().is_empty() {
        root.title = sheet_title(sheet, idx);
    }

    // Relationships also live at the sheet level (not just on topics).
    for r in sheet
        .get("relationships")
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
    {
        if let (Some(e1), Some(e2)) = (
            r.get("end1Id").and_then(|x| x.as_str()),
            r.get("end2Id").and_then(|x| x.as_str()),
        ) {
            rels.push(Relationship {
                end1: e1.to_string(),
                end2: e2.to_string(),
                title: r
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }

    let structure = root_topic
        .get("structureClass")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let theme = parse_theme(sheet);

    let svg = build_svg(&root, structure, &theme, &rels);
    rasterize(&svg)
}

/// Recursively build a [`Node`] from an XMind topic object. `budget` caps the total
/// node count so a huge file can't blow up the canvas; `rels` accumulates every
/// relationship found; `bytes` is the zip (for resolving embedded images).
fn parse_topic(
    v: &serde_json::Value,
    budget: &mut usize,
    bytes: &[u8],
    rels: &mut Vec<Relationship>,
) -> Node {
    let id = v
        .get("id")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    let markers = v
        .get("markers")
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("markerId").and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let has_note = v.get("notes").is_some();
    let labels = v
        .get("labels")
        .and_then(|l| l.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let image = v.get("image").and_then(|im| resolve_image(bytes, im));
    let boundaries = v
        .get("boundaries")
        .and_then(|b| b.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|b| {
                    b.get("range")
                        .and_then(|r| r.as_str())
                        .and_then(parse_range)
                })
                .collect()
        })
        .unwrap_or_default();

    for r in v
        .get("relationships")
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
    {
        if let (Some(e1), Some(e2)) = (
            r.get("end1Id").and_then(|x| x.as_str()),
            r.get("end2Id").and_then(|x| x.as_str()),
        ) {
            rels.push(Relationship {
                end1: e1.to_string(),
                end2: e2.to_string(),
                title: r
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }

    // XMind topics carry `attached` children (the normal tree) and `detached` children
    // (free-floating topics). We render both as branches so a "notes board" sheet (which
    // lives entirely in `detached`) shows its content instead of a lone central topic.
    let mut children = Vec::new();
    if let Some(kids) = v.get("children").and_then(|c| c.as_object()) {
        for key in ["attached", "detached"] {
            for child in kids
                .get(key)
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
            {
                if *budget == 0 {
                    break;
                }
                *budget -= 1;
                children.push(parse_topic(child, budget, bytes, rels));
            }
        }
    }
    Node {
        id,
        title,
        markers,
        has_note,
        labels,
        image,
        boundaries,
        children,
    }
}

/// Extract one named entry from the `.xmind` zip as UTF-8 text.
fn read_zip_text(bytes: &[u8], name: &str) -> Option<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

/// Fallback: decode XMind's own embedded `Thumbnails/thumbnail.png`.
fn embedded_thumbnail(bytes: &[u8]) -> Result<PixImage, DecodeError> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let mut f = zip
        .by_name("Thumbnails/thumbnail.png")
        .map_err(|_| DecodeError::Unsupported)?;
    let mut png = Vec::new();
    f.read_to_end(&mut png)
        .map_err(|e| DecodeError::Io(e.to_string()))?;
    let img = image::load_from_memory(&png)
        .map_err(|e| DecodeError::Malformed(e.to_string()))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let pixels = img.pixels().map(|p| p.0).collect();
    Ok(PixImage::from_rgba(w, h, pixels))
}

// ---- layout --------------------------------------------------------------

/// A topic prepared for layout: wrapped text + its measured box size and the sizes of
/// its stacked content (marker row, image, text, labels), plus the semantic data to
/// draw. Built once so `measure`/`place` don't re-wrap text repeatedly.
struct Prepared {
    id: String,
    lines: Vec<String>,
    markers: Vec<String>,
    has_note: bool,
    labels: Vec<String>,
    image: Option<ImageRef>,
    boundaries: Vec<(usize, usize)>,
    w: f32,
    h: f32,
    text_h: f32,
    marker_h: f32,
    img_h: f32, // includes the gap below the image
    depth: usize,
    children: Vec<Prepared>,
}

impl Prepared {
    /// Distance from the box top to where connectors attach: the vertical centre for
    /// a root/main topic, the text underline for a sub-topic.
    fn attach_offset(&self) -> f32 {
        if self.depth <= 1 {
            self.h / 2.0
        } else {
            pad_y(self.depth) + self.marker_h + self.img_h + self.text_h + 2.0
        }
    }
}

/// A rectangle in layout space (used for subtree bounds → boundary outlines).
#[derive(Clone, Copy)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}
impl Rect {
    fn of(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect {
            x0: x,
            y0: y,
            x1: x + w,
            y1: y + h,
        }
    }
    fn union(self, o: Rect) -> Rect {
        Rect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }
}

/// A boundary outline (grouping a range of a topic's children).
struct BoundOut {
    r: Rect,
}

/// Marker glyph size at a given depth.
fn marker_size(depth: usize) -> f32 {
    (font_size(depth) * 0.95).max(11.0)
}
const LABEL_FS: f32 = 11.0;

/// A positioned topic box, in final (already-offset) canvas coordinates, with the
/// content to draw inside it.
struct BoxOut {
    id: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    lines: Vec<String>,
    markers: Vec<String>,
    has_note: bool,
    labels: Vec<String>,
    image: Option<ImageRef>,
    text_h: f32,
    marker_h: f32,
    img_h: f32,
    depth: usize,
    color: usize, // branch index → Theme::branch (ignored for the root)
    is_root: bool,
}

/// A connector from a parent anchor to a child anchor, drawn as a tapered filled
/// ribbon (`hw1`/`hw2` = half-width where it meets the parent / child).
struct EdgeOut {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    hw1: f32,
    hw2: f32,
    color: usize,
}

/// Connector half-width where it meets a node at `depth` — thick trunks at the
/// centre tapering to thin twigs at the leaves.
fn attach_half_w(depth: usize) -> f32 {
    match depth {
        0 => 4.0,
        1 => 2.0,
        _ => 0.9,
    }
}

fn font_size(depth: usize) -> f32 {
    match depth {
        0 => 21.0,
        1 => 16.0,
        _ => 13.0,
    }
}

/// Max text width (px) before wrapping, by depth.
fn max_content_w(depth: usize) -> f32 {
    match depth {
        0 => 320.0,
        1 => 260.0,
        _ => 210.0,
    }
}

fn pad_x(depth: usize) -> f32 {
    if depth <= 1 {
        13.0
    } else {
        11.0
    }
}
fn pad_y(depth: usize) -> f32 {
    if depth <= 1 {
        8.0
    } else {
        6.0
    }
}
fn line_height(depth: usize) -> f32 {
    font_size(depth) * 1.32
}

/// Rough proportional-font advance. Biased slightly high so text never overflows
/// its box (the same estimate is used for wrapping, so it stays consistent).
fn text_width(s: &str, fs: f32) -> f32 {
    s.chars().count() as f32 * fs * 0.58
}

/// Split a title into display lines: honour explicit `\n`, then greedily
/// word-wrap each line to `max_w`, hard-breaking a single over-long word.
fn wrap_title(title: &str, depth: usize) -> Vec<String> {
    let fs = font_size(depth);
    let max_w = max_content_w(depth);
    let mut out = Vec::new();
    for raw in title.split('\n') {
        let raw = raw.trim_end();
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for word in raw.split(' ') {
            let candidate = if cur.is_empty() {
                word.to_string()
            } else {
                format!("{cur} {word}")
            };
            if text_width(&candidate, fs) <= max_w || cur.is_empty() {
                // A single word wider than the box: hard-break it by chars.
                if cur.is_empty() && text_width(word, fs) > max_w {
                    let mut chunk = String::new();
                    for ch in word.chars() {
                        let trial = format!("{chunk}{ch}");
                        if text_width(&trial, fs) > max_w && !chunk.is_empty() {
                            out.push(chunk.clone());
                            chunk.clear();
                        }
                        chunk.push(ch);
                    }
                    cur = chunk;
                } else {
                    cur = candidate;
                }
            } else {
                out.push(cur);
                cur = word.to_string();
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    // Cap very long titles so one topic can't produce an absurdly tall box.
    const MAX_LINES: usize = 12;
    if out.len() > MAX_LINES {
        out.truncate(MAX_LINES);
        if let Some(last) = out.last_mut() {
            last.push('…');
        }
    }
    out
}

fn prepare(node: &Node, depth: usize) -> Prepared {
    let lines = wrap_title(&node.title, depth);
    let fs = font_size(depth);
    let text_w = lines
        .iter()
        .map(|l| text_width(l, fs))
        .fold(0.0_f32, f32::max);
    let text_h = lines.len() as f32 * line_height(depth);

    // Marker row (markers + a note indicator), above the text.
    let marker_count = node.markers.len() + usize::from(node.has_note);
    let msize = marker_size(depth);
    let marker_h = if marker_count > 0 { msize + 4.0 } else { 0.0 };
    let marker_row_w = marker_count as f32 * (msize + 4.0);

    // Embedded image, above the text (its height carries a 4px gap below it).
    let (img_w, img_h) = node
        .image
        .as_ref()
        .map(|i| (i.w, i.h + 4.0))
        .unwrap_or((0.0, 0.0));

    // Labels, below the text.
    let label_text = node.labels.join("   ");
    let (label_w, label_h) = if node.labels.is_empty() {
        (0.0, 0.0)
    } else {
        (text_width(&label_text, LABEL_FS), LABEL_FS * 1.35 + 3.0)
    };

    let content_w = text_w.max(img_w).max(marker_row_w).max(label_w);
    let w = (content_w + 2.0 * pad_x(depth)).max(44.0);
    let h = marker_h + img_h + text_h + label_h + 2.0 * pad_y(depth);

    let children = node
        .children
        .iter()
        .map(|c| prepare(c, depth + 1))
        .collect();
    Prepared {
        id: node.id.clone(),
        lines,
        markers: node.markers.clone(),
        has_note: node.has_note,
        labels: node.labels.clone(),
        image: node.image.clone(),
        boundaries: node.boundaries.clone(),
        w,
        h,
        text_h,
        marker_h,
        img_h,
        depth,
        children,
    }
}

/// Total vertical extent a subtree needs.
fn measure(p: &Prepared) -> f32 {
    if p.children.is_empty() {
        return p.h;
    }
    let children: f32 =
        p.children.iter().map(measure).sum::<f32>() + V_GAP * (p.children.len() as f32 - 1.0);
    p.h.max(children)
}

/// Emit a positioned `BoxOut` for `p` at `(left, top)`.
fn push_box(
    p: &Prepared,
    left: f32,
    top: f32,
    color: usize,
    is_root: bool,
    boxes: &mut Vec<BoxOut>,
) {
    boxes.push(BoxOut {
        id: p.id.clone(),
        x: left,
        y: top,
        w: p.w,
        h: p.h,
        lines: p.lines.clone(),
        markers: p.markers.clone(),
        has_note: p.has_note,
        labels: p.labels.clone(),
        image: p.image.clone(),
        text_h: p.text_h,
        marker_h: p.marker_h,
        img_h: p.img_h,
        depth: p.depth,
        color,
        is_root,
    });
}

/// Turn a topic's child boundaries into padded outline rects, given the placed rect
/// of each direct child subtree.
fn collect_boundaries(p: &Prepared, child_rects: &[Rect], bounds: &mut Vec<BoundOut>) {
    for &(a, b) in &p.boundaries {
        if a >= child_rects.len() {
            continue;
        }
        let b = b.min(child_rects.len() - 1);
        let mut r = child_rects[a];
        for cr in &child_rects[a..=b] {
            r = r.union(*cr);
        }
        const PAD: f32 = 9.0;
        bounds.push(BoundOut {
            r: Rect {
                x0: r.x0 - PAD,
                y0: r.y0 - PAD,
                x1: r.x1 + PAD,
                y1: r.y1 + PAD,
            },
        });
    }
}

/// Place a subtree growing in direction `dir` (+1 right, −1 left). `tip_x` is the near
/// edge the box grows *from*; `center_y` is its vertical centre. Returns the subtree's
/// bounding rect (for boundary outlines).
#[allow(clippy::too_many_arguments)]
fn place(
    p: &Prepared,
    tip_x: f32,
    center_y: f32,
    dir: f32,
    color: usize,
    boxes: &mut Vec<BoxOut>,
    edges: &mut Vec<EdgeOut>,
    bounds: &mut Vec<BoundOut>,
) -> Rect {
    let left = if dir > 0.0 { tip_x } else { tip_x - p.w };
    let top = center_y - p.h / 2.0;
    push_box(p, left, top, color, false, boxes);
    let mut subtree = Rect::of(left, top, p.w, p.h);
    if p.children.is_empty() {
        return subtree;
    }
    let total: f32 =
        p.children.iter().map(measure).sum::<f32>() + V_GAP * (p.children.len() as f32 - 1.0);
    let parent_anchor_x = if dir > 0.0 { left + p.w } else { left };
    let parent_y = top + p.attach_offset();
    let mut y = center_y - total / 2.0;
    let mut child_rects: Vec<Rect> = Vec::with_capacity(p.children.len());
    for child in &p.children {
        let ext = measure(child);
        let cc = y + ext / 2.0;
        let child_tip = if dir > 0.0 {
            left + p.w + H_GAP
        } else {
            left - H_GAP
        };
        let child_attach_y = (cc - child.h / 2.0) + child.attach_offset();
        edges.push(EdgeOut {
            x1: parent_anchor_x,
            y1: parent_y,
            x2: child_tip,
            y2: child_attach_y,
            hw1: attach_half_w(p.depth),
            hw2: attach_half_w(child.depth),
            color,
        });
        let cr = place(child, child_tip, cc, dir, color, boxes, edges, bounds);
        subtree = subtree.union(cr);
        child_rects.push(cr);
        y += ext + V_GAP;
    }
    collect_boundaries(p, &child_rects, bounds);
    subtree
}

/// Lay out the whole map. Returns positioned boxes, edges + boundary outlines in
/// final coordinates, plus the canvas size.
#[allow(clippy::type_complexity)]
fn layout(root: &Node, structure: &str) -> (Vec<BoxOut>, Vec<EdgeOut>, Vec<BoundOut>, f32, f32) {
    let proot = prepare(root, 0);

    // Which sides do the top-level branches grow toward?
    //   logic.right → all right; logic.left → all left; otherwise a balanced map.
    let (all_right, all_left) = (
        structure.contains("logic.right")
            || structure.contains("org-chart")
            || structure.contains("tree.right"),
        structure.contains("logic.left") || structure.contains("tree.left"),
    );

    // Split top-level branches into (right, left) index lists.
    let (right_idx, left_idx) = split_branches(&proot.children, all_right, all_left);

    let right_ext = side_extent(&proot.children, &right_idx);
    let left_ext = side_extent(&proot.children, &left_idx);
    let root_center_y = right_ext.max(left_ext).max(proot.h) / 2.0;

    let mut boxes = Vec::new();
    let mut edges = Vec::new();
    let mut bounds = Vec::new();

    // Root box, centred horizontally on x = 0.
    let root_left = -proot.w / 2.0;
    let root_top = root_center_y - proot.h / 2.0;
    push_box(&proot, root_left, root_top, 0, true, &mut boxes);
    let root_right = proot.w / 2.0;

    // The placed subtree rect of each top-level branch, by its original index (for a
    // boundary the root itself declares over its branches).
    let mut branch_rects: Vec<Option<Rect>> = vec![None; proot.children.len()];
    place_side(
        &proot.children,
        &right_idx,
        root_right + H_GAP,
        (root_right, root_center_y),
        1.0,
        root_center_y,
        right_ext,
        &mut boxes,
        &mut edges,
        &mut bounds,
        &mut branch_rects,
    );
    place_side(
        &proot.children,
        &left_idx,
        root_left - H_GAP,
        (root_left, root_center_y),
        -1.0,
        root_center_y,
        left_ext,
        &mut boxes,
        &mut edges,
        &mut bounds,
        &mut branch_rects,
    );
    // Boundaries the root declares over its own branches.
    if !proot.boundaries.is_empty() && branch_rects.iter().all(Option::is_some) {
        let rects: Vec<Rect> = branch_rects.iter().map(|r| r.unwrap()).collect();
        collect_boundaries(&proot, &rects, &mut bounds);
    }

    // Normalize to positive coordinates with a margin.
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for b in &boxes {
        min_x = min_x.min(b.x);
        min_y = min_y.min(b.y);
        max_x = max_x.max(b.x + b.w);
        max_y = max_y.max(b.y + b.h);
    }
    // Boundaries can extend past the boxes (they're padded).
    for bd in &bounds {
        min_x = min_x.min(bd.r.x0);
        min_y = min_y.min(bd.r.y0);
        max_x = max_x.max(bd.r.x1);
        max_y = max_y.max(bd.r.y1);
    }
    let (dx, dy) = (CANVAS_MARGIN - min_x, CANVAS_MARGIN - min_y);
    for b in &mut boxes {
        b.x += dx;
        b.y += dy;
    }
    for e in &mut edges {
        e.x1 += dx;
        e.y1 += dy;
        e.x2 += dx;
        e.y2 += dy;
    }
    for bd in &mut bounds {
        bd.r.x0 += dx;
        bd.r.y0 += dy;
        bd.r.x1 += dx;
        bd.r.y1 += dy;
    }
    let w = (max_x - min_x) + 2.0 * CANVAS_MARGIN;
    let h = (max_y - min_y) + 2.0 * CANVAS_MARGIN;
    (boxes, edges, bounds, w, h)
}

/// Assign top-level branch indices to the right or left side. When neither forced
/// direction is set, greedily balance the two sides by measured height so the map
/// looks even — the classic XMind balanced-map behaviour.
fn split_branches(
    children: &[Prepared],
    all_right: bool,
    all_left: bool,
) -> (Vec<usize>, Vec<usize>) {
    if all_left {
        return (Vec::new(), (0..children.len()).collect());
    }
    if all_right {
        return ((0..children.len()).collect(), Vec::new());
    }
    let mut right = Vec::new();
    let mut left = Vec::new();
    let (mut rh, mut lh) = (0.0_f32, 0.0_f32);
    for (i, c) in children.iter().enumerate() {
        let ext = measure(c);
        if rh <= lh {
            right.push(i);
            rh += ext + V_GAP;
        } else {
            left.push(i);
            lh += ext + V_GAP;
        }
    }
    (right, left)
}

/// Total vertical extent of the branches on one side.
fn side_extent(children: &[Prepared], idx: &[usize]) -> f32 {
    if idx.is_empty() {
        return 0.0;
    }
    idx.iter().map(|&i| measure(&children[i])).sum::<f32>() + V_GAP * (idx.len() as f32 - 1.0)
}

#[allow(clippy::too_many_arguments)]
fn place_side(
    children: &[Prepared],
    idx: &[usize],
    tip_x: f32,
    root_anchor: (f32, f32),
    dir: f32,
    root_center_y: f32,
    side_ext: f32,
    boxes: &mut Vec<BoxOut>,
    edges: &mut Vec<EdgeOut>,
    bounds: &mut Vec<BoundOut>,
    branch_rects: &mut [Option<Rect>],
) {
    let mut y = root_center_y - side_ext / 2.0;
    for &i in idx {
        let child = &children[i];
        let ext = measure(child);
        let cc = y + ext / 2.0;
        // Colour = branch's original index (mapped into the palette at emit time via
        // `Theme::branch`), so left/right sides never collide and each branch keeps a
        // stable colour.
        let color = i;
        let child_attach_y = (cc - child.h / 2.0) + child.attach_offset();
        edges.push(EdgeOut {
            x1: root_anchor.0,
            y1: root_anchor.1,
            x2: tip_x,
            y2: child_attach_y,
            hw1: attach_half_w(0),
            hw2: attach_half_w(child.depth),
            color,
        });
        branch_rects[i] = Some(place(child, tip_x, cc, dir, color, boxes, edges, bounds));
        y += ext + V_GAP;
    }
}

// ---- SVG emission --------------------------------------------------------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn build_svg(root: &Node, structure: &str, theme: &Theme, rels: &[Relationship]) -> String {
    let (boxes, edges, boundaries, w, h) = layout(root, structure);
    let mut svg = String::with_capacity(8192);
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" \
         viewBox=\"0 0 {w:.0} {h:.0}\">"
    ));
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{w:.0}\" height=\"{h:.0}\" fill=\"{}\"/>",
        theme.canvas_bg
    ));

    // Boundaries behind everything: a translucent rounded outline grouping a subtree.
    for bd in &boundaries {
        let dash = if theme.boundary.dashed {
            " stroke-dasharray=\"7 5\""
        } else {
            ""
        };
        svg.push_str(&format!(
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"12\" ry=\"12\" \
             fill=\"{}\" fill-opacity=\"{:.2}\" stroke=\"{}\" stroke-width=\"2\"{dash}/>",
            bd.r.x0,
            bd.r.y0,
            bd.r.x1 - bd.r.x0,
            bd.r.y1 - bd.r.y0,
            theme.boundary.fill,
            theme.boundary.opacity,
            theme.boundary.line,
        ));
    }

    // Connectors next, so topic boxes paint over their endpoints. Each is a filled
    // tapered ribbon: an upper bezier out, down the child edge, a lower bezier back.
    for e in &edges {
        let color = theme.branch(e.color);
        let dx = e.x2 - e.x1;
        let (c1x, c2x) = (e.x1 + dx * 0.5, e.x2 - dx * 0.5);
        svg.push_str(&format!(
            "<path d=\"M{:.1} {:.1} C{:.1} {:.1} {:.1} {:.1} {:.1} {:.1} \
             L{:.1} {:.1} C{:.1} {:.1} {:.1} {:.1} {:.1} {:.1} Z\" fill=\"{color}\"/>",
            e.x1,
            e.y1 - e.hw1,
            c1x,
            e.y1 - e.hw1,
            c2x,
            e.y2 - e.hw2,
            e.x2,
            e.y2 - e.hw2,
            e.x2,
            e.y2 + e.hw2,
            c2x,
            e.y2 + e.hw2,
            c1x,
            e.y1 + e.hw1,
            e.x1,
            e.y1 + e.hw1,
        ));
    }

    // Topics. Root + main branches are boxes (filled in a colourful theme, else
    // outlined per the theme); sub-topics (depth ≥ 2) are plain text on a coloured
    // underline — the actual XMind look. Each carries its content stack: an optional
    // marker row + image above the text, and labels below.
    for b in &boxes {
        let branch = theme.branch(b.color).to_string();
        let text_top = b.y + pad_y(b.depth) + b.marker_h + b.img_h;
        let text_color;
        if b.is_root || b.depth == 1 {
            let s = topic_box_style(b, &branch, theme);
            let stroke = if s.stroke_w > 0.0 {
                format!(
                    " stroke=\"{}\" stroke-width=\"{:.1}\"",
                    s.stroke, s.stroke_w
                )
            } else {
                String::new()
            };
            svg.push_str(&format!(
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" \
                 rx=\"{:.1}\" ry=\"{:.1}\" fill=\"{}\"{stroke}/>",
                b.x, b.y, b.w, b.h, s.rx, s.rx, s.fill
            ));
            text_color = s.text;
        } else {
            let uline = theme
                .sub
                .line
                .clone()
                .or_else(|| theme.sub.border.clone())
                .unwrap_or(branch);
            text_color = theme.sub.text.clone().unwrap_or_else(|| "#35353a".into());
            let uy = text_top + b.text_h + 2.0;
            svg.push_str(&format!(
                "<line x1=\"{:.1}\" y1=\"{uy:.1}\" x2=\"{:.1}\" y2=\"{uy:.1}\" \
                 stroke=\"{uline}\" stroke-width=\"2\" stroke-linecap=\"round\"/>",
                b.x + 3.0,
                b.x + b.w - 3.0
            ));
        }
        draw_content(&mut svg, b, &text_color, b.depth <= 1, text_top, theme);
    }

    // Relationships on top: a dashed curved arrow between two topics (by id).
    if !rels.is_empty() {
        let centers: std::collections::HashMap<&str, (f32, f32)> = boxes
            .iter()
            .filter(|b| !b.id.is_empty())
            .map(|b| (b.id.as_str(), (b.x + b.w / 2.0, b.y + b.h / 2.0)))
            .collect();
        for r in rels {
            if let (Some(&p1), Some(&p2)) =
                (centers.get(r.end1.as_str()), centers.get(r.end2.as_str()))
            {
                draw_relationship(&mut svg, p1, p2, &r.title, theme);
            }
        }
    }

    svg.push_str("</svg>");
    svg
}

/// Draw a topic's content stack: marker row + image (above the text), the text, and
/// labels (below). `text_top` is where the text block begins.
fn draw_content(
    svg: &mut String,
    b: &BoxOut,
    text_color: &str,
    bold: bool,
    text_top: f32,
    theme: &Theme,
) {
    let cx = b.x + b.w / 2.0;
    // Marker row, centred above the text.
    let count = b.markers.len() + usize::from(b.has_note);
    if count > 0 {
        let ms = marker_size(b.depth);
        let step = ms + 4.0;
        let mut mx = cx - (count as f32 * step - 4.0) / 2.0;
        let my = b.y + pad_y(b.depth);
        for id in &b.markers {
            draw_marker(svg, id, mx, my, ms);
            mx += step;
        }
        if b.has_note {
            draw_note_marker(svg, mx, my, ms);
        }
    }
    // Embedded image, centred above the text.
    if let Some(img) = &b.image {
        let iy = b.y + pad_y(b.depth) + b.marker_h;
        svg.push_str(&format!(
            "<image x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" href=\"{}\"/>",
            cx - img.w / 2.0,
            iy,
            img.w,
            img.h,
            img.data_uri
        ));
    }
    // Title text.
    push_text(svg, b, text_color, bold, text_top);
    // Labels, below the underline / text.
    if !b.labels.is_empty() {
        let ly = text_top + b.text_h + 2.0 + LABEL_FS + 4.0;
        svg.push_str(&format!(
            "<text x=\"{cx:.1}\" y=\"{ly:.1}\" font-family=\"{FONT_FAMILY}\" \
             font-size=\"{LABEL_FS:.1}\" fill=\"{}\" text-anchor=\"middle\">{}</text>",
            theme.boundary.text,
            esc(&b.labels.join("   "))
        ));
    }
}

/// A dashed relationship arc between two topic centres, with an arrowhead at the end
/// and an optional label at the curve's apex.
fn draw_relationship(svg: &mut String, p1: (f32, f32), p2: (f32, f32), title: &str, theme: &Theme) {
    let (x1, y1) = p1;
    let (x2, y2) = p2;
    // Bow the curve outward from the midpoint (perpendicular offset ∝ distance).
    let (mx, my) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
    let (dx, dy) = (x2 - x1, y2 - y1);
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let (nx, ny) = (-dy / len, dx / len);
    let bow = (len * 0.18).min(80.0);
    let (cx, cy) = (mx + nx * bow, my + ny * bow);
    let dash = if theme.rel.dashed {
        " stroke-dasharray=\"6 5\""
    } else {
        ""
    };
    svg.push_str(&format!(
        "<path d=\"M{x1:.1} {y1:.1} Q{cx:.1} {cy:.1} {x2:.1} {y2:.1}\" fill=\"none\" \
         stroke=\"{}\" stroke-width=\"2\"{dash}/>",
        theme.rel.line
    ));
    if !title.trim().is_empty() {
        // Label at the curve apex (the quadratic's midpoint).
        let (lx, ly) = ((x1 + 2.0 * cx + x2) / 4.0, (y1 + 2.0 * cy + y2) / 4.0);
        svg.push_str(&format!(
            "<text x=\"{lx:.1}\" y=\"{ly:.1}\" font-family=\"{FONT_FAMILY}\" font-size=\"11\" \
             font-style=\"italic\" fill=\"{}\" text-anchor=\"middle\">{}</text>",
            theme.rel.text,
            esc(title.trim())
        ));
    }
    // Arrowhead at p2, aimed from the control point.
    let (ax, ay) = (x2 - cx, y2 - cy);
    let al = (ax * ax + ay * ay).sqrt().max(1.0);
    let (ux, uy) = (ax / al, ay / al);
    let (px, py) = (-uy, ux);
    let s = 9.0;
    svg.push_str(&format!(
        "<path d=\"M{:.1} {:.1} L{:.1} {:.1} L{:.1} {:.1} Z\" fill=\"{}\"/>",
        x2,
        y2,
        x2 - ux * s + px * s * 0.5,
        y2 - uy * s + py * s * 0.5,
        x2 - ux * s - px * s * 0.5,
        y2 - uy * s - py * s * 0.5,
        theme.rel.line
    ));
}

/// Resolved fill/stroke/text/radius for a root or main-topic box, honouring the theme.
struct BoxStyle {
    fill: String,
    stroke: String,
    stroke_w: f32,
    text: String,
    rx: f32,
}

fn topic_box_style(b: &BoxOut, branch: &str, theme: &Theme) -> BoxStyle {
    let ts = if b.is_root {
        &theme.central
    } else {
        &theme.main
    };
    let rx = if ts.rect {
        3.0
    } else if b.is_root {
        10.0
    } else {
        6.0
    };
    // A colourful theme fills the central topic dark and each main topic in its branch
    // colour (white text). Otherwise honour the theme's own fill: a concrete colour →
    // filled; "none" → an outlined card in the branch/border colour.
    if theme.colorful {
        let fill = if b.is_root {
            ts.fill.clone().unwrap_or_else(|| ROOT_FILL.to_string())
        } else {
            branch.to_string()
        };
        let text = ts.text.clone().unwrap_or_else(|| "#ffffff".into());
        BoxStyle {
            fill,
            stroke: String::new(),
            stroke_w: 0.0,
            text,
            rx,
        }
    } else if let Some(fill) = ts.fill.clone() {
        BoxStyle {
            stroke: ts.border.clone().unwrap_or_else(|| fill.clone()),
            stroke_w: ts.border_w.max(1.0),
            text: ts.text.clone().unwrap_or_else(|| contrast_text(&fill)),
            fill,
            rx,
        }
    } else {
        // Outlined card (fill "none").
        BoxStyle {
            fill: theme.canvas_bg.clone(),
            stroke: ts.border.clone().unwrap_or_else(|| branch.to_string()),
            stroke_w: ts.border_w.max(1.6),
            text: ts.text.clone().unwrap_or_else(|| {
                if b.is_root {
                    "#2b2b2b".into()
                } else {
                    "#333333".into()
                }
            }),
            rx,
        }
    }
}

/// Black or white text for readable contrast on `hex` (`#RRGGBB`).
fn contrast_text(hex: &str) -> String {
    if hex.len() == 7 {
        let v = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f32;
        let lum = 0.299 * v(1) + 0.587 * v(3) + 0.114 * v(5);
        if lum > 150.0 {
            "#222222".into()
        } else {
            "#ffffff".into()
        }
    } else {
        "#ffffff".into()
    }
}

fn push_text(svg: &mut String, b: &BoxOut, color: &str, bold: bool, text_top: f32) {
    let fs = font_size(b.depth);
    let lh = line_height(b.depth);
    let weight = if bold { "bold" } else { "normal" };
    let cx = b.x + b.w / 2.0;
    // Baseline of the first line, measured down from the text block's top.
    let first_baseline = text_top + fs * 0.78;
    for (i, line) in b.lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let y = first_baseline + i as f32 * lh;
        svg.push_str(&format!(
            "<text x=\"{cx:.1}\" y=\"{y:.1}\" font-family=\"{FONT_FAMILY}\" font-size=\"{fs:.1}\" \
             font-weight=\"{weight}\" fill=\"{color}\" text-anchor=\"middle\">{}</text>",
            esc(line)
        ));
    }
}

/// Draw a single XMind marker centred in an `ms`×`ms` box at `(x, y)`.
fn draw_marker(svg: &mut String, id: &str, x: f32, y: f32, ms: f32) {
    let (cx, cy, r) = (x + ms / 2.0, y + ms / 2.0, ms / 2.0);
    // priority-N → numbered disc.
    if let Some(n) = id.strip_prefix("priority-") {
        svg.push_str(&format!(
            "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{r:.1}\" fill=\"#e2483a\"/>\
             <text x=\"{cx:.1}\" y=\"{:.1}\" font-family=\"{FONT_FAMILY}\" font-size=\"{:.1}\" \
             font-weight=\"bold\" fill=\"#ffffff\" text-anchor=\"middle\">{}</text>",
            cy + ms * 0.34,
            ms * 0.92,
            esc(n)
        ));
        return;
    }
    // task-* → a progress pie (filled fraction).
    if let Some(kind) = id.strip_prefix("task-") {
        let frac = match kind {
            "done" => 1.0,
            "3oct" | "3quart" | "3quarter" => 0.75,
            "half" => 0.5,
            "oct" | "1oct" => 0.125,
            "quarter" | "quart" | "1quarter" => 0.25,
            _ => 0.0,
        };
        svg.push_str(&format!(
            "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{r:.1}\" fill=\"#ffffff\" \
             stroke=\"#3d8b3d\" stroke-width=\"1.5\"/>"
        ));
        if frac >= 1.0 {
            svg.push_str(&format!(
                "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{:.1}\" fill=\"#3d8b3d\"/>",
                r - 1.5
            ));
        } else if frac > 0.0 {
            let a = std::f32::consts::TAU * frac - std::f32::consts::FRAC_PI_2;
            let rr = r - 1.5;
            let (ex, ey) = (cx + rr * a.cos(), cy + rr * a.sin());
            let large = if frac > 0.5 { 1 } else { 0 };
            svg.push_str(&format!(
                "<path d=\"M{cx:.1} {cy:.1} L{cx:.1} {:.1} A{rr:.1} {rr:.1} 0 {large} 1 {ex:.1} {ey:.1} Z\" \
                 fill=\"#3d8b3d\"/>",
                cy - rr
            ));
        }
        return;
    }
    // flag-* / star-* / symbol-* → a coloured glyph.
    let named = |c: &str| match c {
        "red" => "#e2483a",
        "orange" => "#f0932b",
        "blue" => "#3498db",
        "green" => "#3d8b3d",
        "purple" => "#8e44ad",
        "darkblue" => "#2c3e9e",
        "gray" | "grey" => "#7f8c8d",
        _ => "#f1c40f",
    };
    if let Some(c) = id.strip_prefix("flag-") {
        // A little pennant.
        let col = named(c);
        svg.push_str(&format!(
            "<path d=\"M{:.1} {:.1} L{:.1} {:.1} L{:.1} {:.1} L{:.1} {:.1} Z\" fill=\"{col}\"/>\
             <line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{col}\" stroke-width=\"1.4\"/>",
            x + ms * 0.28, y + ms * 0.16,
            x + ms * 0.82, y + ms * 0.30,
            x + ms * 0.28, y + ms * 0.50,
            x + ms * 0.28, y + ms * 0.16,
            x + ms * 0.28, y + ms * 0.16, x + ms * 0.28, y + ms * 0.86,
        ));
        return;
    }
    if let Some(c) = id.strip_prefix("star-") {
        draw_glyph(svg, "★", cx, cy, ms, named(c));
        return;
    }
    if let Some(kind) = id.strip_prefix("symbol-") {
        let (glyph, col) = match kind {
            "attention" | "exclam" => ("!", "#e2483a"),
            "question" => ("?", "#3498db"),
            "wrong" | "err" => ("✕", "#e2483a"),
            "right" | "correct" => ("✓", "#3d8b3d"),
            "plus" => ("+", "#3d8b3d"),
            "minus" => ("−", "#e2483a"),
            _ => ("•", "#7f8c8d"),
        };
        svg.push_str(&format!(
            "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{r:.1}\" fill=\"{col}\"/>"
        ));
        draw_glyph(svg, glyph, cx, cy, ms * 0.92, "#ffffff");
        return;
    }
    // Unknown marker → a small neutral dot.
    svg.push_str(&format!(
        "<circle cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"{:.1}\" fill=\"#b0b0b0\"/>",
        r * 0.5
    ));
}

/// A note indicator: a small folded-corner page.
fn draw_note_marker(svg: &mut String, x: f32, y: f32, ms: f32) {
    let (l, t, w) = (x + ms * 0.18, y + ms * 0.12, ms * 0.64);
    svg.push_str(&format!(
        "<rect x=\"{l:.1}\" y=\"{t:.1}\" width=\"{w:.1}\" height=\"{:.1}\" rx=\"1.5\" \
         fill=\"#fff4c2\" stroke=\"#c9a227\" stroke-width=\"1\"/>\
         <line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"#c9a227\" stroke-width=\"1\"/>\
         <line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"#c9a227\" stroke-width=\"1\"/>",
        ms * 0.8,
        l + w * 0.2, t + ms * 0.28, l + w * 0.8, t + ms * 0.28,
        l + w * 0.2, t + ms * 0.5, l + w * 0.8, t + ms * 0.5,
    ));
}

/// Draw a single glyph centred at `(cx, cy)`, sized to `ms`.
fn draw_glyph(svg: &mut String, glyph: &str, cx: f32, cy: f32, ms: f32, color: &str) {
    svg.push_str(&format!(
        "<text x=\"{cx:.1}\" y=\"{:.1}\" font-family=\"{FONT_FAMILY}\" font-size=\"{:.1}\" \
         font-weight=\"bold\" fill=\"{color}\" text-anchor=\"middle\">{}</text>",
        cy + ms * 0.34,
        ms,
        esc(glyph)
    ));
}

// ---- rasterization -------------------------------------------------------

/// Rasterize the generated SVG with the embedded font loaded, so text renders.
fn rasterize(svg: &str) -> Result<PixImage, DecodeError> {
    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_font_data(FONT.to_vec());
    opt.font_family = FONT_FAMILY.to_string();

    let tree = usvg::Tree::from_data(svg.as_bytes(), &opt)
        .map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let size = tree.size();
    let maxdim = size.width().max(size.height()).max(1.0);
    // Render at a scale that brings the longest side up to ~RASTER_TARGET. Because the
    // source is vector, this is *true* higher-res rendering (crisper glyphs), not upscaling,
    // so a small map no longer looks chunky once the viewer fits it to the window. The 8×
    // cap keeps a 1–2 node map from ballooning.
    let scale = (RASTER_TARGET / maxdim).clamp(0.02, 8.0);
    let w = (size.width() * scale).round().max(1.0) as u32;
    let h = (size.height() * scale).round().max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(w, h)
        .ok_or_else(|| DecodeError::Malformed("mind map too large to rasterize".into()))?;
    pixmap.fill(tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let pixels = pixmap
        .pixels()
        .iter()
        .map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Ok(PixImage::from_rgba(w, h, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal `.xmind` (a zip with one content.json) in memory.
    fn make_xmind(content_json: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file("content.json", opts).unwrap();
            zw.write_all(content_json.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    const SAMPLE: &str = r#"[{"id":"s1","rootTopic":{"id":"r","title":"Root",
        "structureClass":"org.xmind.ui.map.clockwise","children":{"attached":[
        {"id":"a","title":"Branch A","children":{"attached":[
            {"id":"a1","title":"Leaf A1"},{"id":"a2","title":"Leaf A2"}]}},
        {"id":"b","title":"Branch B"},
        {"id":"c","title":"Branch C with a longer title that should wrap"}
    ]}}}]"#;

    #[test]
    fn parses_tree_from_content_json() {
        let v: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
        let rt = &v.as_array().unwrap()[0]["rootTopic"];
        let mut budget = 100;
        let mut rels = Vec::new();
        let root = parse_topic(rt, &mut budget, b"", &mut rels);
        assert_eq!(root.title, "Root");
        assert_eq!(root.children.len(), 3);
        assert_eq!(root.children[0].children.len(), 2);
        assert_eq!(root.children[0].children[1].title, "Leaf A2");
    }

    #[test]
    fn wraps_and_honours_newlines() {
        let lines = wrap_title("one\ntwo three", 2);
        assert!(lines.len() >= 2, "explicit newline should split");
        assert_eq!(lines[0], "one");
    }

    #[test]
    fn decodes_generated_xmind_to_nonzero_image() {
        let bytes = make_xmind(SAMPLE);
        let img = XMindDecoder.decode(&bytes).expect("decode xmind");
        assert!(
            img.width > 100 && img.height > 50,
            "got {}x{}",
            img.width,
            img.height
        );
        // Rendered on a white canvas → the corners should be white.
        assert_eq!(img.pixels[0], [255, 255, 255, 255]);
    }

    #[test]
    fn multi_sheet_titles_and_per_sheet_render() {
        // Two sheets with different roots → titles listed, and each sheet renders.
        let json = r#"[
            {"rootTopic":{"title":"Overview","children":{"attached":[{"title":"A"},{"title":"B"}]}}},
            {"rootTopic":{"title":"Details","children":{"attached":[{"title":"C"}]}}}
        ]"#;
        let bytes = make_xmind(json);
        assert_eq!(xmind_sheet_titles(&bytes), vec!["Overview", "Details"]);
        for idx in 0..2 {
            let img = render_xmind_sheet(&bytes, idx).expect("render sheet");
            assert!(
                img.width > 50 && img.height > 20,
                "sheet {idx}: {}x{}",
                img.width,
                img.height
            );
        }
        // An out-of-range index falls back to the first sheet, never panics.
        assert!(render_xmind_sheet(&bytes, 99).is_ok());
    }

    #[test]
    fn budget_caps_node_count() {
        // A deeply-nested chain must stop at the budget instead of recursing
        // forever. (Kept under serde_json's default 128-deep parse limit.)
        let depth = 20;
        let mut json = String::from(r#"[{"rootTopic":{"title":"r","children":{"attached":["#);
        for _ in 0..depth {
            json.push_str(r#"{"title":"x","children":{"attached":["#);
        }
        for _ in 0..depth {
            json.push_str("]}}");
        }
        json.push_str("]}}}]");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let rt = &v.as_array().unwrap()[0]["rootTopic"];
        let mut budget = 10;
        let mut rels = Vec::new();
        let _ = parse_topic(rt, &mut budget, b"", &mut rels);
        assert_eq!(budget, 0, "budget should be exhausted, not underflow");
    }
}
