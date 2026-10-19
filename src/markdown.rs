//! Rendered **Markdown** — the GitHub / VS Code-preview reading of a `.md` file, drawn in native
//! egui widgets rather than rasterised.
//!
//! `.md` already had two readings before this: the syntax-highlighted raster tile (what the grid
//! shows) and the real text editor (`draw_text_ui`). Neither is the one you want when the file is
//! a README — a heading should *be* a heading, a link should be clickable, and a badge should be
//! the image rather than `![build](https://…)`. This module is the third reading.
//!
//! **Split exactly like `icy_parser_core`:** [`pulldown_cmark`] is a parser only — it knows
//! CommonMark + GFM and nothing about pixels. The document model below, the styling and every
//! glyph drawn are ours, so the preview inherits the app's theme, its code font, its syntax
//! highlighter and its decoders (an inline `.pcx`, `.ans` or `.svg` renders, which github.com
//! cannot do).
//!
//! **Why native widgets and not an HTML/CSS engine.** No pure-Rust engine composes inside an egui
//! panel — `blitz`/Stylo wants its own wgpu surface, `wry` is a separate OS webview with a
//! webkit2gtk dependency. Native widgets keep the text *selectable*, the links real, and the whole
//! document restyled the instant the theme changes. "CSS" therefore means the active theme: see
//! [`MdStyle::resolve`], which reads the same keys the code viewer does so a preview and the
//! source listing beside it always agree.
//!
//! The parsing half ([`parse`]) is pure and unit-tested; the rendering half needs a `Ui`.

use eframe::egui;
use pulldown_cmark::{BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag,
                     TagEnd};

// ---------------------------------------------------------------------------------------------
// Document model
// ---------------------------------------------------------------------------------------------

/// Which emphases are active on a run of text. Markdown nests these freely (`**bold *and
/// italic***`), so they are flags carried down the inline stack rather than nested nodes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Emph {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    /// `` `inline code` `` — rendered in the code font on a tinted pill.
    pub code: bool,
    pub sup: bool,
    pub sub: bool,
}

/// One piece of a paragraph, heading or table cell.
#[derive(Clone, Debug, PartialEq)]
pub enum Inline {
    Text {
        text: String,
        emph: Emph,
        /// `Some(href)` when this run is part of a link — the whole run is clickable.
        link: Option<String>,
    },
    Image {
        url: String,
        alt: String,
        title: String,
        /// A badge is `[![alt](img)](href)`: the image itself is the link.
        link: Option<String>,
    },
    /// A newline in the source inside one paragraph. Markdown treats it as a space.
    SoftBreak,
    /// Two trailing spaces, a `\`, or `<br>` — a real line break.
    HardBreak,
    /// `[^1]` — rendered as a superscript link down to the definition.
    FootRef(String),
}

/// GFM table column alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// One list item. `task` is `Some` for a GFM task list (`- [x] done`).
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

/// A GitHub alert (`> [!NOTE]`), which is a blockquote wearing a kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl Alert {
    pub fn label(self) -> &'static str {
        match self {
            Alert::Note => "Note",
            Alert::Tip => "Tip",
            Alert::Important => "Important",
            Alert::Warning => "Warning",
            Alert::Caution => "Caution",
        }
    }

    /// The glyph GitHub puts beside the label. Plain Unicode — all of these resolve through the
    /// DejaVu fallback (see the font gotcha in CLAUDE.md), so none of them can tofu.
    pub fn glyph(self) -> &'static str {
        match self {
            Alert::Note => "ℹ",
            Alert::Tip => "★",
            Alert::Important => "❗",
            Alert::Warning => "⚠",
            Alert::Caution => "⛔",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        inl: Vec<Inline>,
        /// GitHub-style slug, so `#installation` links resolve and the outline can scroll here.
        anchor: String,
    },
    Para(Vec<Inline>),
    /// A fenced or indented code block. `lang` is the fence's info string, verbatim.
    Code {
        lang: String,
        text: String,
    },
    Quote {
        alert: Option<Alert>,
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<Item>,
    },
    Table {
        align: Vec<Align>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    /// `---` — a horizontal rule.
    Rule,
    /// Content an HTML block asked to centre (`<p align="center">`, `<div align=center>`,
    /// `<center>`). Modelled because it is how essentially every README opens — a centred logo
    /// over a row of badges — and left-aligning that is the single most visible way to look wrong.
    Center(Vec<Block>),
    /// `<details><summary>…</summary>` … `</details>`, the one HTML construct common enough in
    /// READMEs to be worth modelling (it collapses, so dropping it would hide content).
    Details {
        summary: Vec<Inline>,
        blocks: Vec<Block>,
    },
    /// YAML/TOML front matter. Shown collapsed — it is metadata, not prose.
    FrontMatter(String),
    /// `[^1]: …`, collected at the end of the document under a rule, like GitHub.
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
}

/// A parsed document plus the heading outline the sidebar renders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Doc {
    pub blocks: Vec<Block>,
    /// `(level, text, anchor)` for every heading, in document order.
    pub outline: Vec<(u8, String, String)>,
}

/// Flatten inlines to their text — the heading outline, an image's alt text, a link's tooltip.
pub fn inline_text(inl: &[Inline]) -> String {
    let mut s = String::new();
    for i in inl {
        match i {
            Inline::Text { text, .. } => s.push_str(text),
            Inline::Image { alt, .. } => s.push_str(alt),
            Inline::SoftBreak | Inline::HardBreak => s.push(' '),
            Inline::FootRef(l) => {
                s.push('[');
                s.push_str(l);
                s.push(']');
            }
        }
    }
    s
}

/// GitHub's heading slug: lowercase, drop punctuation, spaces to hyphens. Close enough that
/// hand-written `#anchor` links in real READMEs resolve.
fn slug(text: &str) -> String {
    let mut s = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            s.extend(ch.to_lowercase());
        } else if ch == ' ' || ch == '-' || ch == '_' {
            s.push(if ch == '_' { '_' } else { '-' });
        }
    }
    s
}

// ---------------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------------

/// What the event walk is currently inside. Blocks and inlines each have their own stack so an
/// inline container (a cell, a summary) can sit inside a block container (a table, a `<details>`).
enum Frame {
    Quote(Option<Alert>),
    List {
        ordered: bool,
        start: u64,
        items: Vec<Item>,
    },
    Item {
        task: Option<bool>,
    },
    Details {
        summary: Vec<Inline>,
    },
    Footnote {
        label: String,
    },
}

/// Parse markdown into the render model. Pure: no `Ui`, no I/O, no globals.
pub fn parse(src: &str) -> Doc {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_SUPERSCRIPT);
    // `> [!NOTE]` alerts. GFM also brings its own tweaks to tables/autolinks, all of which are
    // what a file written for github.com expects.
    opts.insert(Options::ENABLE_GFM);
    // Front matter is metadata, not a `---` rule followed by a stray paragraph of YAML.
    opts.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    opts.insert(Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS);

    let mut p = Walker::default();
    for ev in Parser::new_ext(src, opts) {
        p.event(ev);
    }
    p.finish()
}

#[derive(Default)]
struct Walker {
    /// Block containers, innermost last. `blocks[0]` is the document.
    blocks: Vec<Vec<Block>>,
    frames: Vec<Frame>,
    /// Inline containers, innermost last: a paragraph, a heading, a cell, an image's alt text.
    inl: Vec<Vec<Inline>>,
    emph: Emph,
    /// Link hrefs, innermost last (a link cannot nest, but reference links can resolve oddly).
    links: Vec<String>,
    /// Open `Tag::Heading` level, so `End(Heading)` knows what it is closing.
    heading: Option<u8>,
    /// Open code fence: `(info string, accumulated text)`.
    code: Option<(String, String)>,
    /// Open image: `(url, title)` — its alt text accumulates in the top inline container.
    image: Option<(String, String)>,
    /// `Tag::HtmlBlock` body, accumulated so tags split across events still parse.
    html: Option<String>,
    /// Open `Tag::Table` state.
    table: Option<TableAcc>,
    in_head: bool,
    /// Heading anchors already used, so duplicates get GitHub's `-1`, `-2` suffixes.
    anchors: std::collections::HashMap<String, usize>,
    outline: Vec<(u8, String, String)>,
    /// Footnote definitions, pulled out of the flow and appended at the end like GitHub.
    footnotes: Vec<(String, Vec<Block>)>,
    front: Option<String>,
    /// Inside `MetadataBlock`, so its `Text` goes to `front` rather than the document.
    in_meta: bool,
    /// An inline container this walk opened by itself, because inline content arrived with no
    /// `Paragraph` around it. See [`Walker::ensure_inline`].
    implicit: bool,
}

struct TableAcc {
    align: Vec<Align>,
    head: Vec<Vec<Inline>>,
    rows: Vec<Vec<Vec<Inline>>>,
    row: Vec<Vec<Inline>>,
}

impl Walker {
    /// The block list being appended to.
    fn top(&mut self) -> &mut Vec<Block> {
        if self.blocks.is_empty() {
            self.blocks.push(Vec::new());
        }
        self.blocks.last_mut().expect("just pushed")
    }

    fn push_block(&mut self, b: Block) {
        self.top().push(b);
    }

    /// Append text, merging into the previous run when it carries the same emphasis and link —
    /// fewer, longer runs mean fewer `LayoutJob` sections and simpler link ranges.
    fn push_text(&mut self, text: &str) {
        self.ensure_inline();
        let (emph, link) = (self.emph, self.links.last().cloned());
        let Some(c) = self.inl.last_mut() else { return };
        if let Some(Inline::Text { text: prev, emph: pe, link: pl }) = c.last_mut() {
            if *pe == emph && *pl == link {
                prev.push_str(text);
                return;
            }
        }
        c.push(Inline::Text { text: text.to_string(), emph, link });
    }

    fn push_inline(&mut self, i: Inline) {
        self.ensure_inline();
        if let Some(c) = self.inl.last_mut() {
            c.push(i);
        }
    }

    /// Open a paragraph for inline content that arrived without one.
    ///
    /// **A tight list emits no `Paragraph` events at all.** `- **a** then *b*` arrives as bare
    /// `Strong`/`Text`/`Emphasis` events straight inside the `Item`, and with nowhere to put them
    /// each run was flushed as its own block — so every README bullet broke into one stacked line
    /// per emphasis change. Opening an implicit paragraph on the first inline event, and closing
    /// it when a block boundary arrives, makes a tight item read exactly like a loose one.
    fn ensure_inline(&mut self) {
        if self.inl.is_empty() {
            self.inl.push(Vec::new());
            self.implicit = true;
        }
    }

    /// Close an implicitly-opened paragraph at a block boundary.
    fn flush_inline(&mut self) {
        if !self.implicit {
            return;
        }
        self.implicit = false;
        if self.inl.len() == 1 {
            let mut inl = self.inl.pop().unwrap_or_default();
            autolink(&mut inl);
            if !inl.is_empty() {
                self.push_block(Block::Para(inl));
            }
        }
    }

    fn event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.in_meta {
                    self.front.get_or_insert_with(String::new).push_str(&t);
                } else if let Some((_, body)) = self.code.as_mut() {
                    body.push_str(&t);
                } else {
                    self.push_text(&t);
                }
            }
            Event::Code(t) => {
                let was = self.emph;
                self.emph.code = true;
                self.push_text(&t);
                self.emph = was;
            }
            // Math is parsed only with ENABLE_MATH (off): keep the source visible rather than
            // silently dropping a formula.
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                let was = self.emph;
                self.emph.code = true;
                self.push_text(&t);
                self.emph = was;
            }
            Event::Html(h) => self.html.get_or_insert_with(String::new).push_str(&h),
            Event::InlineHtml(h) => self.inline_html(&h),
            Event::FootnoteReference(l) => self.push_inline(Inline::FootRef(l.to_string())),
            Event::SoftBreak => self.push_inline(Inline::SoftBreak),
            Event::HardBreak => self.push_inline(Inline::HardBreak),
            Event::Rule => {
                self.flush_inline();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(done) => {
                // The marker arrives inside the item's first paragraph; hoist it onto the item.
                for f in self.frames.iter_mut().rev() {
                    if let Frame::Item { task } = f {
                        *task = Some(done);
                        break;
                    }
                }
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        // A block tag ends any paragraph this walk opened implicitly.
        if matches!(
            tag,
            Tag::Paragraph
                | Tag::Heading { .. }
                | Tag::BlockQuote(_)
                | Tag::CodeBlock(_)
                | Tag::HtmlBlock
                | Tag::List(_)
                | Tag::Item
                | Tag::FootnoteDefinition(_)
                | Tag::Table(_)
                | Tag::MetadataBlock(_)
        ) {
            self.flush_inline();
        }
        match tag {
            Tag::Paragraph => self.inl.push(Vec::new()),
            Tag::Heading { level, .. } => {
                self.heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                });
                self.inl.push(Vec::new());
            }
            Tag::BlockQuote(kind) => {
                let alert = kind.map(|k| match k {
                    BlockQuoteKind::Note => Alert::Note,
                    BlockQuoteKind::Tip => Alert::Tip,
                    BlockQuoteKind::Important => Alert::Important,
                    BlockQuoteKind::Warning => Alert::Warning,
                    BlockQuoteKind::Caution => Alert::Caution,
                });
                self.frames.push(Frame::Quote(alert));
                self.blocks.push(Vec::new());
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::HtmlBlock => {
                self.html = Some(String::new());
            }
            Tag::List(start) => {
                self.frames.push(Frame::List {
                    ordered: start.is_some(),
                    start: start.unwrap_or(1),
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                self.frames.push(Frame::Item { task: None });
                self.blocks.push(Vec::new());
            }
            Tag::FootnoteDefinition(label) => {
                self.frames.push(Frame::Footnote { label: label.to_string() });
                self.blocks.push(Vec::new());
            }
            Tag::Table(align) => {
                self.table = Some(TableAcc {
                    align: align
                        .into_iter()
                        .map(|a| match a {
                            pulldown_cmark::Alignment::None => Align::None,
                            pulldown_cmark::Alignment::Left => Align::Left,
                            pulldown_cmark::Alignment::Center => Align::Center,
                            pulldown_cmark::Alignment::Right => Align::Right,
                        })
                        .collect(),
                    head: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Tag::TableHead => self.in_head = true,
            Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.row = Vec::new();
                }
            }
            Tag::TableCell => self.inl.push(Vec::new()),
            Tag::Emphasis => self.emph.italic = true,
            Tag::Strong => self.emph.bold = true,
            Tag::Strikethrough => self.emph.strike = true,
            Tag::Superscript => self.emph.sup = true,
            Tag::Subscript => self.emph.sub = true,
            Tag::Link { dest_url, .. } => self.links.push(dest_url.to_string()),
            Tag::Image { dest_url, title, .. } => {
                self.image = Some((dest_url.to_string(), title.to_string()));
                self.inl.push(Vec::new()); // the alt text accumulates here
            }
            Tag::MetadataBlock(_) => self.in_meta = true,
            // Definition lists are off; anything else renders as its inner content.
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        // Likewise on the way out: an item's implicit paragraph belongs to that item.
        if matches!(
            tag,
            TagEnd::Item
                | TagEnd::List(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::FootnoteDefinition
                | TagEnd::Table
        ) {
            self.flush_inline();
        }
        match tag {
            TagEnd::Paragraph => {
                let mut inl = self.inl.pop().unwrap_or_default();
                autolink(&mut inl);
                if !inl.is_empty() {
                    self.push_block(Block::Para(inl));
                }
            }
            TagEnd::Heading(_) => {
                let inl = self.inl.pop().unwrap_or_default();
                let level = self.heading.take().unwrap_or(1);
                let text = inline_text(&inl);
                let base = slug(&text);
                // GitHub dedupes repeated headings with a numeric suffix.
                let n = self.anchors.entry(base.clone()).or_insert(0);
                let anchor = if *n == 0 { base.clone() } else { format!("{base}-{n}") };
                *n += 1;
                self.outline.push((level, text, anchor.clone()));
                self.push_block(Block::Heading { level, inl, anchor });
            }
            TagEnd::BlockQuote(_) => {
                let blocks = self.blocks.pop().unwrap_or_default();
                let alert = match self.frames.pop() {
                    Some(Frame::Quote(a)) => a,
                    _ => None,
                };
                self.push_block(Block::Quote { alert, blocks });
            }
            TagEnd::CodeBlock => {
                if let Some((lang, text)) = self.code.take() {
                    self.push_block(Block::Code { lang, text });
                }
            }
            TagEnd::HtmlBlock => {
                if let Some(h) = self.html.take() {
                    self.html_block(&h);
                }
            }
            TagEnd::List(_) => {
                if let Some(Frame::List { ordered, start, items }) = self.frames.pop() {
                    self.push_block(Block::List { ordered, start, items });
                }
            }
            TagEnd::Item => {
                let blocks = self.blocks.pop().unwrap_or_default();
                let task = match self.frames.pop() {
                    Some(Frame::Item { task }) => task,
                    _ => None,
                };
                // The item belongs to the enclosing list, not the block flow.
                if let Some(Frame::List { items, .. }) = self.frames.last_mut() {
                    items.push(Item { task, blocks });
                } else {
                    for b in blocks {
                        self.push_block(b);
                    }
                }
            }
            TagEnd::FootnoteDefinition => {
                let blocks = self.blocks.pop().unwrap_or_default();
                if let Some(Frame::Footnote { label }) = self.frames.pop() {
                    self.footnotes.push((label, blocks));
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.push_block(Block::Table { align: t.align, head: t.head, rows: t.rows });
                }
            }
            TagEnd::TableHead => self.in_head = false,
            TagEnd::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    let row = std::mem::take(&mut t.row);
                    t.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                let mut cell = self.inl.pop().unwrap_or_default();
                autolink(&mut cell);
                if let Some(t) = self.table.as_mut() {
                    if self.in_head {
                        t.head.push(cell);
                    } else {
                        t.row.push(cell);
                    }
                }
            }
            TagEnd::Emphasis => self.emph.italic = false,
            TagEnd::Strong => self.emph.bold = false,
            TagEnd::Strikethrough => self.emph.strike = false,
            TagEnd::Superscript => self.emph.sup = false,
            TagEnd::Subscript => self.emph.sub = false,
            TagEnd::Link => {
                self.links.pop();
            }
            TagEnd::Image => {
                let alt_inl = self.inl.pop().unwrap_or_default();
                if let Some((url, title)) = self.image.take() {
                    let link = self.links.last().cloned();
                    self.push_inline(Inline::Image {
                        url,
                        alt: inline_text(&alt_inl),
                        title,
                        link,
                    });
                }
            }
            TagEnd::MetadataBlock(_) => self.in_meta = false,
            _ => {}
        }
    }

    /// A raw HTML **block**. Markdown passes these through verbatim, and READMEs lean on a handful
    /// of them: a centered logo, a badge table, a `<details>` section. Rather than printing the
    /// tags (which reads as a rendering bug) or dropping the block (which hides content), the
    /// useful parts are extracted — `<details>`/`<summary>` become a real collapsing section,
    /// `<img>` becomes an image, `<a>` wraps it in a link, and everything else contributes its
    /// text. A full HTML engine is out of scope (see the module docs).
    fn html_block(&mut self, html: &str) {
        let low = html.to_ascii_lowercase();
        if low.contains("<details") {
            let summary = tag_inner(html, "summary")
                .map(|s| {
                    let mut v = Vec::new();
                    html_inlines(&s, &mut v);
                    v
                })
                .unwrap_or_default();
            self.frames.push(Frame::Details { summary });
            self.blocks.push(Vec::new());
            return;
        }
        if low.contains("</details>") {
            let blocks = self.blocks.pop().unwrap_or_default();
            let summary = match self.frames.pop() {
                Some(Frame::Details { summary }) => summary,
                _ => Vec::new(),
            };
            self.push_block(Block::Details { summary, blocks });
            return;
        }
        if low.contains("<summary") {
            // A `<summary>` on its own event: attach it to the open `<details>`.
            if let Some(s) = tag_inner(html, "summary") {
                let mut v = Vec::new();
                html_inlines(&s, &mut v);
                if let Some(Frame::Details { summary }) = self.frames.last_mut() {
                    *summary = v;
                }
            }
            return;
        }
        let mut inl = Vec::new();
        html_inlines(html, &mut inl);
        if !inl.is_empty() {
            let centred = low.contains("<center")
                || attr(html, "align").is_some_and(|a| a.eq_ignore_ascii_case("center"));
            let para = Block::Para(inl);
            self.push_block(if centred {
                Block::Center(vec![para])
            } else {
                para
            });
        }
    }

    /// Raw HTML *inside* a paragraph. The text between the tags arrives as its own `Text` events,
    /// so only the tags themselves are interpreted here.
    fn inline_html(&mut self, html: &str) {
        let t = html.trim();
        let low = t.to_ascii_lowercase();
        if low.starts_with("<br") {
            self.push_inline(Inline::HardBreak);
        } else if low.starts_with("<img") {
            if let Some(img) = img_tag(t, self.links.last().cloned()) {
                self.push_inline(img);
            }
        } else if low.starts_with("<code") || low.starts_with("<kbd") || low.starts_with("<tt") {
            self.emph.code = true;
        } else if low.starts_with("</code") || low.starts_with("</kbd") || low.starts_with("</tt") {
            self.emph.code = false;
        } else if low.starts_with("<b>") || low.starts_with("<strong") {
            self.emph.bold = true;
        } else if low.starts_with("</b>") || low.starts_with("</strong") {
            self.emph.bold = false;
        } else if low.starts_with("<i>") || low.starts_with("<em") {
            self.emph.italic = true;
        } else if low.starts_with("</i>") || low.starts_with("</em") {
            self.emph.italic = false;
        } else if low.starts_with("<a ") {
            if let Some(href) = attr(t, "href") {
                self.links.push(href);
            }
        } else if low.starts_with("</a") {
            self.links.pop();
        }
        // Everything else (a `<div>`, a `<p align>`) is chrome we have no layout for: dropping the
        // tag keeps its text, which is the better half of it.
    }

    fn finish(mut self) -> Doc {
        self.flush_inline();
        // Close anything the document left open (a truncated file, an unbalanced `<details>`).
        while self.blocks.len() > 1 {
            let blocks = self.blocks.pop().unwrap_or_default();
            for b in blocks {
                self.push_block(b);
            }
        }
        let mut blocks = self.blocks.pop().unwrap_or_default();
        if let Some(f) = self.front.take() {
            blocks.insert(0, Block::FrontMatter(f));
        }
        for (label, fb) in std::mem::take(&mut self.footnotes) {
            blocks.push(Block::Footnote { label, blocks: fb });
        }
        Doc { blocks, outline: self.outline }
    }
}

/// The text between `<tag …>` and `</tag>`, if both are present.
fn tag_inner(html: &str, tag: &str) -> Option<String> {
    let low = html.to_ascii_lowercase();
    let open = low.find(&format!("<{tag}"))?;
    let gt = low[open..].find('>')? + open + 1;
    let close = low[gt..].find(&format!("</{tag}"))? + gt;
    Some(html[gt..close].to_string())
}

/// An HTML attribute's value, quoted or bare.
fn attr(tag: &str, name: &str) -> Option<String> {
    let low = tag.to_ascii_lowercase();
    let mut from = 0;
    // Match the attribute name on a word boundary so `src` doesn't hit `data-src`.
    while let Some(i) = low[from..].find(name) {
        let at = from + i;
        let before_ok = at > 0
            && low[..at]
                .chars()
                .last()
                .is_some_and(|c| c.is_whitespace() || c == '<');
        let rest = &low[at + name.len()..];
        let after = rest.trim_start();
        if before_ok && after.starts_with('=') {
            let v = &tag[at + name.len()..];
            let v = v.trim_start().trim_start_matches('=').trim_start();
            let val = if let Some(q) = v.strip_prefix('"') {
                q.split('"').next().unwrap_or("")
            } else if let Some(q) = v.strip_prefix('\'') {
                q.split('\'').next().unwrap_or("")
            } else {
                v.split([' ', '>', '\n', '\t']).next().unwrap_or("")
            };
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
        from = at + name.len();
    }
    None
}

/// `<img src alt title>` → an [`Inline::Image`].
fn img_tag(tag: &str, link: Option<String>) -> Option<Inline> {
    let url = attr(tag, "src")?;
    Some(Inline::Image {
        url,
        alt: attr(tag, "alt").unwrap_or_default(),
        title: attr(tag, "title").unwrap_or_default(),
        link,
    })
}

/// Walk a raw HTML chunk into inlines: `<img>` (badges), `<a href>` around them, `<br>`, and the
/// plain text between tags. Unknown tags are dropped, their contents kept.
fn html_inlines(html: &str, out: &mut Vec<Inline>) {
    let mut emph = Emph::default();
    let mut links: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut rest = html;
    let flush = |text: &mut String, out: &mut Vec<Inline>, emph: Emph, links: &[String]| {
        if text.trim().is_empty() {
            text.clear();
            return;
        }
        out.push(Inline::Text {
            text: std::mem::take(text),
            emph,
            link: links.last().cloned(),
        });
    };
    while let Some(lt) = rest.find('<') {
        text.push_str(&rest[..lt]);
        let after = &rest[lt..];
        let Some(gt) = after.find('>') else {
            // An unterminated `<` is just text.
            text.push_str(after);
            rest = "";
            break;
        };
        let tag = &after[..=gt];
        let low = tag.to_ascii_lowercase();
        if low.starts_with("<br") {
            flush(&mut text, out, emph, &links);
            out.push(Inline::HardBreak);
        } else if low.starts_with("<img") {
            flush(&mut text, out, emph, &links);
            if let Some(img) = img_tag(tag, links.last().cloned()) {
                out.push(img);
            }
        } else if low.starts_with("<a ") {
            flush(&mut text, out, emph, &links);
            if let Some(h) = attr(tag, "href") {
                links.push(h);
            }
        } else if low.starts_with("</a") {
            flush(&mut text, out, emph, &links);
            links.pop();
        } else if low.starts_with("<b>") || low.starts_with("<strong") {
            flush(&mut text, out, emph, &links);
            emph.bold = true;
        } else if low.starts_with("</b>") || low.starts_with("</strong") {
            flush(&mut text, out, emph, &links);
            emph.bold = false;
        } else if low.starts_with("<i>") || low.starts_with("<em") {
            flush(&mut text, out, emph, &links);
            emph.italic = true;
        } else if low.starts_with("</i>") || low.starts_with("</em") {
            flush(&mut text, out, emph, &links);
            emph.italic = false;
        } else if low.starts_with("<code") || low.starts_with("<kbd") || low.starts_with("<tt") {
            flush(&mut text, out, emph, &links);
            emph.code = true;
        } else if low.starts_with("</code") || low.starts_with("</kbd") || low.starts_with("</tt") {
            flush(&mut text, out, emph, &links);
            emph.code = false;
        } else if low.starts_with("<!--") {
            // A comment may legitimately contain `>`; skip to its real end.
            if let Some(end) = after.find("-->") {
                rest = &after[end + 3..];
                continue;
            }
        }
        rest = &after[gt + 1..];
    }
    text.push_str(rest);
    flush(&mut text, out, emph, &links);
}

/// Split plain text runs on bare URLs and turn each into a link — GFM's "autolink literals".
///
/// GitHub linkifies a URL typed straight into prose; `pulldown-cmark` does not implement that
/// extension, so a README that writes `see https://example.com` rendered it as dead text while the
/// same URL in `<angle brackets>` was clickable. Runs that already carry a link, and `code` runs,
/// are left alone — a URL inside `` `…` `` is being shown, not offered.
fn autolink(inl: &mut Vec<Inline>) {
    let mut out: Vec<Inline> = Vec::with_capacity(inl.len());
    for i in inl.drain(..) {
        let Inline::Text { text, emph, link } = &i else {
            out.push(i);
            continue;
        };
        if link.is_some() || emph.code || !(text.contains("http://") || text.contains("https://")) {
            out.push(i);
            continue;
        }
        let (emph, text) = (*emph, text.clone());
        let mut rest = text.as_str();
        let mut plain = String::new();
        while let Some(at) = rest.find("http") {
            let tail = &rest[at..];
            if !(tail.starts_with("http://") || tail.starts_with("https://")) {
                // A "http" that does not start a URL (e.g. "https" inside a word): keep scanning.
                let step = at + 4;
                plain.push_str(&rest[..step]);
                rest = &rest[step..];
                continue;
            }
            let end = tail
                .find(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '"')
                .unwrap_or(tail.len());
            // Trailing punctuation belongs to the sentence, not the URL: "see https://x.test."
            let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '\'']);
            if url.len() <= "https://".len() {
                plain.push_str(&tail[..end]);
                rest = &tail[end..];
                continue;
            }
            plain.push_str(&rest[..at]);
            if !plain.is_empty() {
                out.push(Inline::Text {
                    text: std::mem::take(&mut plain),
                    emph,
                    link: None,
                });
            }
            out.push(Inline::Text {
                text: url.to_string(),
                emph,
                link: Some(url.to_string()),
            });
            rest = &tail[url.len()..];
        }
        plain.push_str(rest);
        if !plain.is_empty() {
            out.push(Inline::Text { text: plain, emph, link: None });
        }
    }
    *inl = out;
}

/// A fence info string → the extension the syntax highlighter knows that language by.
///
/// The highlighter is keyed by file extension (`decode/code.rs` picks a `LangSpec` from one), but a
/// fence says `rust` or `shell`. Without this map every fenced block would render unhighlighted.
pub fn fence_ext(info: &str) -> &str {
    // The info string may carry more than the language (`rust,ignore`, `js {highlight=1}`).
    let lang = info
        .split([',', ' ', '\t', '{', ':'])
        .next()
        .unwrap_or("")
        .trim();
    let lower: &str = &lang.to_ascii_lowercase();
    match lower {
        "rust" | "rs" => "rs",
        "python" | "py" | "python3" => "py",
        "javascript" | "js" | "node" | "mjs" | "cjs" => "js",
        "typescript" | "ts" => "ts",
        "jsx" => "jsx",
        "tsx" => "tsx",
        "c" => "c",
        "c++" | "cpp" | "cxx" | "cc" => "cpp",
        "objective-c" | "objc" => "m",
        "csharp" | "cs" | "c#" => "cs",
        "java" => "java",
        "kotlin" | "kt" => "kt",
        "swift" => "swift",
        "go" | "golang" => "go",
        "ruby" | "rb" => "rb",
        "php" => "php",
        "perl" | "pl" => "pl",
        "lua" => "lua",
        "shell" | "sh" | "bash" | "zsh" | "console" | "shell-session" => "sh",
        "fish" => "fish",
        "powershell" | "ps1" | "pwsh" => "ps1",
        "bat" | "batch" | "cmd" => "bat",
        "sql" => "sql",
        "html" => "html",
        "xml" | "svg" => "xml",
        "css" => "css",
        "scss" => "scss",
        "sass" => "sass",
        "less" => "less",
        "json" | "jsonc" | "json5" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "ini" | "cfg" | "conf" => "ini",
        "diff" | "patch" => "diff",
        "makefile" | "make" => "mk",
        "cmake" => "cmake",
        "dockerfile" | "docker" => "dockerfile",
        "haskell" | "hs" => "hs",
        "scala" => "scala",
        "elixir" | "ex" | "exs" => "ex",
        "erlang" | "erl" => "erl",
        "clojure" | "clj" => "clj",
        "lisp" | "elisp" | "emacs-lisp" => "lisp",
        "scheme" | "scm" => "scm",
        "r" => "r",
        "julia" | "jl" => "jl",
        "dart" => "dart",
        "zig" => "zig",
        "nim" => "nim",
        "crystal" | "cr" => "cr",
        "d" => "d",
        "pascal" | "pas" | "delphi" => "pas",
        "fortran" | "f90" => "f90",
        "cobol" | "cob" => "cob",
        "ada" => "adb",
        "basic" | "qbasic" | "qb64" | "qb64pe" | "vb" | "vba" | "bas" => "bas",
        "asm" | "assembly" | "nasm" | "x86asm" => "asm",
        "vim" | "viml" | "vimscript" => "vim",
        "tex" | "latex" => "tex",
        "md" | "markdown" => "md",
        "rst" => "rst",
        "csv" => "csv",
        "tsv" => "tsv",
        "log" => "log",
        "gdscript" | "gd" => "gd",
        "glsl" | "hlsl" | "shader" => "glsl",
        "graphql" | "gql" => "graphql",
        "proto" | "protobuf" => "proto",
        "nix" => "nix",
        "tf" | "terraform" | "hcl" => "tf",
        // Unknown or absent: no language, which renders as plain (correct, just uncoloured).
        _ => "",
    }
}

// ---------------------------------------------------------------------------------------------
// Styling — "CSS", resolved from the active theme
// ---------------------------------------------------------------------------------------------

/// Every colour, font and metric the renderer draws with, resolved once per frame.
///
/// This is the stylesheet. It is deliberately *derived* rather than authored: the preview reads the
/// same theme keys the code viewer and the raster tile read (`editor.background` for a code block,
/// `textLink.foreground` for a link, the `tokenColors` for syntax), so the three surfaces can never
/// drift apart, and importing a VS Code theme restyles the markdown too — which is exactly what VS
/// Code's own preview does.
pub struct MdStyle {
    /// Body text font. Proportional: markdown is prose, not a listing.
    pub body: egui::FontId,
    /// Code font — the user's `code_font_path` choice, shared with the text viewer.
    pub mono: egui::FontId,
    /// Multipliers on `body.size` for `#`..`######`.
    pub h_scale: [f32; 6],
    pub text: egui::Color32,
    pub weak: egui::Color32,
    pub strong: egui::Color32,
    pub link: egui::Color32,
    /// Fenced-block background + border.
    pub code_bg: egui::Color32,
    pub code_border: egui::Color32,
    /// `` `inline code` `` pill.
    pub inline_bg: egui::Color32,
    pub quote_bar: egui::Color32,
    pub quote_text: egui::Color32,
    pub rule: egui::Color32,
    pub table_border: egui::Color32,
    pub table_head_bg: egui::Color32,
    pub table_zebra: egui::Color32,
    /// GitHub reads at a measured column, not the full width of a 4K monitor. 0 = unconstrained.
    pub max_width: f32,
}

impl MdStyle {
    /// Resolve against the live `Ui` (so unthemed installs still look deliberate) and the active
    /// theme where it has an opinion.
    pub fn resolve(
        ui: &egui::Ui,
        theme: Option<&crate::theme::Theme>,
        body_size: f32,
        mono: egui::FontId,
        max_width: f32,
    ) -> MdStyle {
        let v = ui.visuals();
        let rgb = |c: Option<[u8; 4]>| {
            c.map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
        };
        let th = |f: fn(&crate::theme::Theme) -> Option<[u8; 4]>| theme.and_then(|t| rgb(f(t)));
        let text = th(|t| t.text).unwrap_or_else(|| v.text_color());
        let weak = th(|t| t.weak_text).unwrap_or_else(|| v.weak_text_color());
        MdStyle {
            body: egui::FontId::proportional(body_size),
            mono,
            // GitHub's own type scale, as ratios of the body size.
            h_scale: [2.0, 1.5, 1.25, 1.0, 0.875, 0.85],
            text,
            weak,
            strong: th(|t| t.strong_text).unwrap_or_else(|| v.strong_text_color()),
            link: th(|t| t.hyperlink).unwrap_or(v.hyperlink_color),
            code_bg: th(|t| t.extreme_bg.or(t.window_bg)).unwrap_or(v.extreme_bg_color),
            code_border: th(|t| t.border).unwrap_or(v.widgets.noninteractive.bg_stroke.color),
            inline_bg: th(|t| t.faint_bg).unwrap_or(v.faint_bg_color),
            quote_bar: weak.gamma_multiply(0.6),
            quote_text: weak,
            rule: th(|t| t.border).unwrap_or(v.widgets.noninteractive.bg_stroke.color),
            table_border: th(|t| t.border).unwrap_or(v.widgets.noninteractive.bg_stroke.color),
            table_head_bg: th(|t| t.faint_bg).unwrap_or(v.faint_bg_color),
            table_zebra: th(|t| t.faint_bg)
                .unwrap_or(v.faint_bg_color)
                .gamma_multiply(0.5),
            max_width,
        }
    }

    fn heading_size(&self, level: u8) -> f32 {
        self.body.size * self.h_scale[(level.clamp(1, 6) - 1) as usize]
    }
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// An image the host has resolved for us. Decoding and fetching belong to the app (worker threads,
/// the HTTP cache, the decoder registry); the renderer only ever paints a finished texture — the
/// same "only CPU pixels cross back, the UI thread uploads" rule the thumbnailer follows.
pub enum Img {
    /// Being fetched/decoded — draw a placeholder so the layout doesn't jump when it lands.
    Pending,
    /// Not an image, missing, or a dead URL. Draws as its alt text.
    Failed,
    Ready {
        tex: egui::TextureHandle,
        /// Source pixel size, so the image can be clamped to the text column by aspect.
        size: egui::Vec2,
    },
}

/// What the reader did that the host has to act on. The renderer never navigates by itself: a
/// relative link may mean "open this file in the viewer", which only `Kaleidotron` can do.
pub enum Action {
    /// An `http(s)`/`mailto` link — hand to the OS.
    Url(String),
    /// A `#heading` link — scroll this document.
    Anchor(String),
    /// A link to a path relative to this document (a sibling `.md`, an image, a folder).
    Local(std::path::PathBuf),
    /// "Copy" on a fenced block.
    Copy(String),
}

/// A laid-out run of inlines: the job itself, its links as `(href, char start, char end)`, and its
/// bold runs as char ranges. Char counts, not bytes, because that is what `CCursor` indexes.
type InlineJob = (
    egui::text::LayoutJob,
    Vec<(String, usize, usize)>,
    Vec<(usize, usize)>,
);

/// One pass of rendering. Built fresh each frame: `images` is read-only, and everything the host
/// must act on comes back in the `out_*` fields rather than through a callback that would have to
/// borrow `Kaleidotron` while the `Ui` already does.
pub struct Render<'a> {
    pub style: &'a MdStyle,
    /// Resolved images, keyed by the URL as written in the document.
    pub images: &'a std::collections::HashMap<String, Img>,
    /// The document's own directory — what a relative link or image path is relative to.
    pub base: std::path::PathBuf,
    /// Anchor to scroll to this frame, consumed when it is found.
    pub scroll_to: Option<String>,
    /// Out: image URLs the renderer needs and `images` does not have yet.
    pub want_images: Vec<String>,
    /// Out: what the reader clicked.
    pub action: Option<Action>,
    /// Depth of nested lists, for the bullet glyph and indentation.
    depth: usize,
}

impl<'a> Render<'a> {
    pub fn new(
        style: &'a MdStyle,
        images: &'a std::collections::HashMap<String, Img>,
        base: std::path::PathBuf,
    ) -> Self {
        Render {
            style,
            images,
            base,
            scroll_to: None,
            want_images: Vec::new(),
            action: None,
            depth: 0,
        }
    }

    /// Render a whole document.
    pub fn doc(&mut self, ui: &mut egui::Ui, doc: &Doc) {
        // A measured column, centred, like every documentation site — text that runs the full width
        // of a wide window is unreadable. 0 means "fill", for a narrow dock.
        let avail = ui.available_width();
        let width = if self.style.max_width > 0.0 {
            self.style.max_width.min(avail)
        } else {
            avail
        };
        let pad = ((avail - width) * 0.5).max(0.0);
        ui.horizontal(|ui| {
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.vertical(|ui| {
                ui.set_width(width);
                self.blocks(ui, &doc.blocks, width);
            });
        });
    }

    fn blocks(&mut self, ui: &mut egui::Ui, blocks: &[Block], width: f32) {
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 {
                ui.add_space(self.style.body.size * 0.5);
            }
            self.block(ui, b, width);
        }
    }

    fn block(&mut self, ui: &mut egui::Ui, b: &Block, width: f32) {
        match b {
            Block::Heading { level, inl, anchor } => self.heading(ui, *level, inl, anchor, width),
            Block::Para(inl) => {
                self.inlines(ui, inl, self.style.body.clone(), self.style.text, width);
            }
            Block::Code { lang, text } => self.code_block(ui, lang, text, width),
            Block::Quote { alert, blocks } => self.quote(ui, *alert, blocks, width),
            Block::List { ordered, start, items } => self.list(ui, *ordered, *start, items, width),
            Block::Table { align, head, rows } => self.table(ui, align, head, rows, width),
            Block::Rule => {
                ui.add_space(4.0);
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(width, 1.0),
                    egui::Sense::hover(),
                );
                ui.painter().hline(rect.x_range(), rect.center().y, (1.0, self.style.rule));
                ui.add_space(4.0);
            }
            Block::Center(inner) => {
                // Centred by measured leading space, not by a centring layout: the content of a
                // centred block is a `horizontal_wrapped` row, which claims the full width — so a
                // layout alignment has no slack to work with and the row stays hard left. Measure
                // how wide the row actually comes out, then indent by half the remainder.
                for b in inner {
                    let natural = match b {
                        Block::Para(inl) => self.row_width(ui, inl),
                        // Only a paragraph is measured; anything else keeps the full column.
                        _ => width,
                    };
                    let lead = ((width - natural) * 0.5).max(0.0);
                    ui.horizontal(|ui| {
                        if lead > 0.0 {
                            ui.add_space(lead);
                        }
                        ui.vertical(|ui| {
                            ui.set_width((width - lead).max(32.0));
                            self.block(ui, b, (width - lead).max(32.0));
                        });
                    });
                }
            }
            Block::Details { summary, blocks } => {
                let title = inline_text(summary);
                let title = if title.trim().is_empty() {
                    "Details".to_string()
                } else {
                    title
                };
                // A stable id from the summary text, so opening one section does not reset
                // another's state when the document re-renders.
                egui::CollapsingHeader::new(egui::RichText::new(title).strong())
                    .id_salt(("md_details", inline_text(summary)))
                    .show(ui, |ui| {
                        let w = ui.available_width();
                        self.blocks(ui, blocks, w);
                    });
            }
            Block::FrontMatter(text) => {
                // Metadata, not prose: collapsed, because it is almost never what you opened the
                // file to read — but shown, because it is part of the file.
                egui::CollapsingHeader::new(
                    egui::RichText::new("front matter").color(self.style.weak).italics(),
                )
                .id_salt("md_front_matter")
                .show(ui, |ui| {
                    let w = ui.available_width();
                    self.code_block(ui, "yaml", text, w);
                });
            }
            Block::Footnote { label, blocks } => {
                ui.horizontal_top(|ui| {
                    ui.label(
                        egui::RichText::new(format!("[{label}]"))
                            .color(self.style.weak)
                            .font(self.style.body.clone()),
                    );
                    ui.vertical(|ui| {
                        let w = ui.available_width();
                        self.blocks(ui, blocks, w);
                    });
                });
            }
        }
    }

    fn heading(
        &mut self,
        ui: &mut egui::Ui,
        level: u8,
        inl: &[Inline],
        anchor: &str,
        width: f32,
    ) {
        ui.add_space(self.style.body.size * (if level <= 2 { 0.8 } else { 0.5 }));
        let font = egui::FontId::proportional(self.style.heading_size(level));
        let top = ui.cursor().top();
        // Headings are bold regardless of the source's emphasis.
        let bold: Vec<Inline> = inl
            .iter()
            .map(|i| match i {
                Inline::Text { text, emph, link } => Inline::Text {
                    text: text.clone(),
                    emph: Emph { bold: true, ..*emph },
                    link: link.clone(),
                },
                other => other.clone(),
            })
            .collect();
        self.inlines(ui, &bold, font, self.style.strong, width);
        // GitHub rules off its top two levels.
        if level <= 2 {
            ui.add_space(3.0);
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
            ui.painter()
                .hline(rect.x_range(), rect.center().y, (1.0, self.style.rule));
        }
        // An outline click asked for this heading: bring it to the top of the viewport.
        if self.scroll_to.as_deref() == Some(anchor) {
            let bottom = ui.cursor().top();
            let r = egui::Rect::from_min_max(
                egui::pos2(ui.max_rect().left(), top),
                egui::pos2(ui.max_rect().right(), bottom.max(top + 1.0)),
            );
            ui.scroll_to_rect(r, Some(egui::Align::TOP));
            self.scroll_to = None;
        }
    }

    /// A blockquote, or a GitHub alert (`> [!NOTE]`) — the same shape with a coloured bar + label.
    fn quote(&mut self, ui: &mut egui::Ui, alert: Option<Alert>, blocks: &[Block], width: f32) {
        let accent = match alert {
            None => self.style.quote_bar,
            Some(Alert::Note) => egui::Color32::from_rgb(47, 129, 247),
            Some(Alert::Tip) => egui::Color32::from_rgb(63, 185, 80),
            Some(Alert::Important) => egui::Color32::from_rgb(163, 113, 247),
            Some(Alert::Warning) => egui::Color32::from_rgb(210, 153, 34),
            Some(Alert::Caution) => egui::Color32::from_rgb(248, 81, 73),
        };
        let bar_w = 4.0;
        let gap = 10.0;
        let inner = (width - bar_w - gap).max(40.0);
        let top = ui.cursor().top();
        ui.horizontal_top(|ui| {
            ui.add_space(bar_w + gap);
            ui.vertical(|ui| {
                ui.set_width(inner);
                if let Some(a) = alert {
                    ui.label(
                        egui::RichText::new(format!("{} {}", a.glyph(), a.label()))
                            .color(accent)
                            .strong()
                            .font(self.style.body.clone()),
                    );
                    ui.add_space(2.0);
                }
                // A quote's own text is dimmed; an alert's is normal (it is the point).
                let was = self.style.quote_text;
                let _ = was;
                self.blocks(ui, blocks, inner);
            });
        });
        // Painted after the content, since the bar sits in the left margin rather than under it.
        let bottom = ui.cursor().top();
        let x = ui.max_rect().left() + bar_w * 0.5;
        ui.painter().vline(
            x,
            egui::Rangef::new(top, bottom - self.style.body.size * 0.2),
            (bar_w, accent),
        );
    }

    fn list(
        &mut self,
        ui: &mut egui::Ui,
        ordered: bool,
        start: u64,
        items: &[Item],
        width: f32,
    ) {
        // Nested lists cycle the bullet, like every markdown renderer.
        const BULLETS: [&str; 3] = ["•", "◦", "▪"];
        let indent = self.style.body.size * 1.2;
        for (i, item) in items.iter().enumerate() {
            let marker = if let Some(done) = item.task {
                // Rendered, not interactive: this is a preview, and silently rewriting the file
                // from a checkbox click is not something a viewer should do.
                if done {
                    "☑".to_string()
                } else {
                    "☐".to_string()
                }
            } else if ordered {
                format!("{}.", start + i as u64)
            } else {
                BULLETS[self.depth % BULLETS.len()].to_string()
            };
            let mw = self.style.body.size * if ordered { 1.8 } else { 1.1 };
            let inner = (width - indent - mw - 4.0).max(40.0);
            ui.horizontal_top(|ui| {
                ui.add_space(indent);
                // A fixed marker column so item text lines up whatever the number's width.
                ui.add_sized(
                    [mw, self.style.body.size * 1.4],
                    egui::Label::new(
                        egui::RichText::new(marker)
                            .color(if item.task.is_some() {
                                self.style.text
                            } else {
                                self.style.weak
                            })
                            .font(self.style.body.clone()),
                    )
                    .selectable(false),
                );
                ui.vertical(|ui| {
                    ui.set_width(inner);
                    self.depth += 1;
                    self.blocks(ui, &item.blocks, inner);
                    self.depth -= 1;
                });
            });
        }
    }

    /// A fenced block: tinted card, language label, copy button, syntax-highlighted body.
    fn code_block(&mut self, ui: &mut egui::Ui, lang: &str, text: &str, width: f32) {
        let ext = fence_ext(lang);
        // The exact colours the raster tile and the text viewer resolve, from the same place.
        let (_bg, toks) = crate::decode::code_palette(ext);
        let spans = crate::decode::highlight_lines(text, ext);
        let mono = self.style.mono.clone();
        let mut job = egui::text::LayoutJob::default();
        // Code must not reflow: a wrapped shell command is a wrong shell command. It scrolls.
        job.wrap.max_width = f32::INFINITY;
        for (i, runs) in spans.iter().enumerate() {
            if i > 0 {
                job.append(
                    "\n",
                    0.0,
                    egui::TextFormat {
                        font_id: mono.clone(),
                        color: self.style.text,
                        ..Default::default()
                    },
                );
            }
            for (run, tok) in runs {
                let idx = crate::decode::ALL_TOKS
                    .iter()
                    .position(|t| t == tok)
                    .unwrap_or(0);
                let c = toks[idx];
                job.append(
                    run,
                    0.0,
                    egui::TextFormat {
                        font_id: mono.clone(),
                        color: egui::Color32::from_rgb(c[0], c[1], c[2]),
                        ..Default::default()
                    },
                );
            }
        }
        let mut want_copy = false;
        egui::Frame::new()
            .fill(self.style.code_bg)
            .stroke((1.0, self.style.code_border))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width((width - 22.0).max(40.0));
                // The header only earns its row when it says something: a language, or the copy
                // button, which is worth a row on its own for a shell snippet.
                ui.horizontal(|ui| {
                    if !lang.trim().is_empty() {
                        ui.label(
                            egui::RichText::new(lang.trim())
                                .color(self.style.weak)
                                .size(self.style.body.size * 0.8),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(crate::app::icons::COPY)
                                        .size(self.style.body.size * 0.95),
                                )
                                .frame(false),
                            )
                            .on_hover_text("Copy this block")
                            .clicked()
                        {
                            want_copy = true;
                        }
                    });
                });
                egui::ScrollArea::horizontal()
                    .id_salt(("md_code", text.len(), lang.to_string()))
                    .show(ui, |ui| {
                        let galley = ui.painter().layout_job(job);
                        ui.add(egui::Label::new(galley).selectable(true));
                    });
            });
        if want_copy {
            self.action = Some(Action::Copy(text.to_string()));
        }
    }

    fn table(
        &mut self,
        ui: &mut egui::Ui,
        align: &[Align],
        head: &[Vec<Inline>],
        rows: &[Vec<Vec<Inline>>],
        width: f32,
    ) {
        let cols = head.len().max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
        if cols == 0 {
            return;
        }
        let pad = 8.0;
        // Natural width of each column = its widest cell laid out unwrapped, then scaled down to
        // fit. Measuring beats guessing: a table of one-word cells should not get equal thirds.
        //
        // Measured through `inline_job` — the very layout the cell will be painted with — because
        // a cell is not plain body text: a header is bold, a `` `.ans` `` span is the monospace
        // font at a different size. Measuring the flattened string in the body font under-measured
        // both, and the text then spilled across the column rule into its neighbour.
        let mut nat = vec![0.0f32; cols];
        let measure = |me: &Self, ui: &egui::Ui, inl: &[Inline]| -> f32 {
            let (job, _, _) = me.inline_job(inl, &me.style.body, me.style.text, f32::INFINITY);
            if job.text.is_empty() {
                return 0.0;
            }
            ui.painter().layout_job(job).rect.width()
        };
        for (c, cell) in head.iter().enumerate() {
            // Headers are drawn bold; faux bold is the same glyphs drawn twice a fraction apart,
            // so it is a hair wider than the metrics say. One pixel of allowance covers it.
            nat[c] = nat[c].max(measure(self, ui, cell) + 1.0);
        }
        for row in rows {
            for (c, cell) in row.iter().enumerate() {
                if c < cols {
                    nat[c] = nat[c].max(measure(self, ui, cell));
                }
            }
        }
        // A sub-pixel of rounding is enough to wrap a cell that measured as exactly fitting.
        for w in nat.iter_mut() {
            *w += 2.0;
        }
        let avail = width - pad * 2.0 * cols as f32;
        let total: f32 = nat.iter().sum();
        let widths: Vec<f32> = if total <= avail || total <= 0.0 {
            nat
        } else {
            // Over budget: scale proportionally, with a floor so a narrow column stays readable.
            let k = avail / total;
            nat.iter().map(|w| (w * k).max(36.0)).collect()
        };
        let border = self.style.table_border;
        let mut row_rects: Vec<egui::Rect> = Vec::new();
        let draw_row =
            |me: &mut Self, ui: &mut egui::Ui, cells: &[Vec<Inline>], header: bool| -> egui::Rect {
                let top = ui.cursor().top();
                let resp = ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (c, &w) in widths.iter().enumerate().take(cols) {
                        let a = align.get(c).copied().unwrap_or(Align::None);
                        ui.allocate_ui_with_layout(
                            egui::vec2(w + pad * 2.0, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                // Pin the cell to the full reserved width. `allocate_ui_with_layout`
                                // advances the parent cursor by the child's *min_rect*, so a cell
                                // narrower than its column packs the next one in early — while the
                                // column rules, drawn at the reserved pitch, stay where they were.
                                // The dividers then walk further right with every column and slice
                                // through the text. (Same trap as the pad grid's `scope_builder`
                                // cursor-advance: a child's measured size corrupting the parent.)
                                ui.set_min_width(w + pad * 2.0);
                                ui.add_space(4.0);
                                let empty = Vec::new();
                                let cell = cells.get(c).unwrap_or(&empty);
                                // `:-:` / `--:` alignment, as leading space measured from the
                                // cell's own natural width. A `Layout` alignment cannot do it:
                                // the cell's content is a full-width child, so there is no slack
                                // left for the layout to distribute — the text has to be moved.
                                let natural = {
                                    let (job, _, _) = me.inline_job(
                                        cell,
                                        &me.style.body,
                                        me.style.text,
                                        f32::INFINITY,
                                    );
                                    if job.text.is_empty() {
                                        0.0
                                    } else {
                                        ui.painter().layout_job(job).rect.width()
                                    }
                                };
                                let slack = (w - natural).max(0.0);
                                let lead = pad
                                    + match a {
                                        Align::Center => slack * 0.5,
                                        Align::Right => slack,
                                        _ => 0.0,
                                    };
                                ui.horizontal(|ui| {
                                    ui.add_space(lead);
                                    ui.vertical(|ui| {
                                        // Only the *remaining* width, or a right-aligned cell
                                        // would wrap where it should not.
                                        ui.set_width(w - (lead - pad));
                                        let font = me.style.body.clone();
                                        let colour = if header {
                                            me.style.strong
                                        } else {
                                            me.style.text
                                        };
                                        let cell: Vec<Inline> = if header {
                                            cell.iter()
                                                .map(|i| match i {
                                                    Inline::Text { text, emph, link } => {
                                                        Inline::Text {
                                                            text: text.clone(),
                                                            emph: Emph {
                                                                bold: true,
                                                                ..*emph
                                                            },
                                                            link: link.clone(),
                                                        }
                                                    }
                                                    o => o.clone(),
                                                })
                                                .collect()
                                        } else {
                                            cell.clone()
                                        };
                                        me.inlines(ui, &cell, font, colour, w);
                                    });
                                });
                                ui.add_space(4.0);
                            },
                        );
                    }
                });
                egui::Rect::from_min_max(
                    egui::pos2(ui.max_rect().left(), top),
                    egui::pos2(
                        ui.max_rect().left() + widths.iter().sum::<f32>() + pad * 2.0 * cols as f32,
                        resp.response.rect.bottom(),
                    ),
                )
            };
        // Reserve the fills so they paint UNDER the text (the zebra-stripe idiom from the recolor
        // rows and the code viewer's current-line tint).
        let bg_slot = ui.painter().add(egui::Shape::Noop);
        let head_rect = draw_row(self, ui, head, true);
        let mut fills: Vec<egui::Shape> = vec![egui::Shape::rect_filled(
            head_rect,
            0.0,
            self.style.table_head_bg,
        )];
        row_rects.push(head_rect);
        for (i, row) in rows.iter().enumerate() {
            let r = draw_row(self, ui, row, false);
            if i % 2 == 1 {
                fills.push(egui::Shape::rect_filled(r, 0.0, self.style.table_zebra));
            }
            row_rects.push(r);
        }
        ui.painter().set(bg_slot, egui::Shape::Vec(fills));
        // Grid lines last, over the cells.
        if let (Some(first), Some(last)) = (row_rects.first(), row_rects.last()) {
            let outer = first.union(*last);
            let p = ui.painter();
            p.rect_stroke(
                outer,
                0.0,
                (1.0, border),
                egui::StrokeKind::Inside,
            );
            for r in &row_rects {
                p.hline(outer.x_range(), r.bottom(), (1.0, border));
            }
            let mut x = outer.left();
            for w in widths.iter().take(cols.saturating_sub(1)) {
                x += w + pad * 2.0;
                p.vline(x, outer.y_range(), (1.0, border));
            }
        }
    }

    // -------------------------------------------------------------------------------------
    // Inlines
    // -------------------------------------------------------------------------------------

    /// Render a run of inlines, wrapping at `width`.
    ///
    /// Two paths, because they want different things. **Text only** goes through one `LayoutJob`
    /// laid out once: perfect wrapping, and — crucially — *selectable*, since it is a single
    /// `Label`. Link rectangles are then recovered from that same galley by char range, so
    /// hit-testing cannot disagree with what was drawn. **With images** (a badge row, an inline
    /// icon) it falls back to word-by-word widgets in a `horizontal_wrapped`, which is the only way
    /// to flow an image between words; selection is lost for those paragraphs alone.
    fn inlines(
        &mut self,
        ui: &mut egui::Ui,
        inl: &[Inline],
        font: egui::FontId,
        color: egui::Color32,
        width: f32,
    ) {
        if inl.is_empty() {
            return;
        }
        if inl.iter().any(|i| matches!(i, Inline::Image { .. })) {
            self.flow(ui, inl, font, color, width);
        } else {
            self.text_run(ui, inl, font, color, width);
        }
    }

    /// The `TextFormat` for one run, from its emphasis flags.
    fn fmt(&self, emph: Emph, font: &egui::FontId, color: egui::Color32, link: bool) -> egui::TextFormat {
        let mut size = font.size;
        if emph.sup || emph.sub {
            size *= 0.75;
        }
        let family = if emph.code {
            self.style.mono.family.clone()
        } else {
            font.family.clone()
        };
        let mut f = egui::TextFormat {
            font_id: egui::FontId::new(if emph.code { size * 0.92 } else { size }, family),
            color: if link { self.style.link } else { color },
            background: if emph.code {
                self.style.inline_bg
            } else {
                egui::Color32::TRANSPARENT
            },
            italics: emph.italic,
            strikethrough: if emph.strike {
                egui::Stroke::new(1.0, color)
            } else {
                egui::Stroke::NONE
            },
            underline: if link {
                egui::Stroke::new(1.0, self.style.link)
            } else {
                egui::Stroke::NONE
            },
            ..Default::default()
        };
        // egui has no synthetic bold, so emphasis is faked the way the OSD title is: see
        // `text_run`, which double-draws a bold galley with a small offset. The format itself only
        // records the intent.
        if emph.sup {
            f.valign = egui::Align::TOP;
        } else if emph.sub {
            f.valign = egui::Align::BOTTOM;
        }
        f
    }

    /// How wide a run of inlines would be laid out on one line, images included.
    ///
    /// Used to centre a block: an image contributes the width it will actually be *drawn* at
    /// (clamped to the column), so a centred logo is centred on the logo, not on its source size.
    /// An image that has not loaded yet measures as the spinner, and the row re-centres by itself
    /// on the frame it arrives.
    fn row_width(&self, ui: &egui::Ui, inl: &[Inline]) -> f32 {
        let mut w = 0.0;
        let mut text: Vec<Inline> = Vec::new();
        let flush = |text: &mut Vec<Inline>, w: &mut f32| {
            if text.is_empty() {
                return;
            }
            let (job, _, _) =
                self.inline_job(text, &self.style.body, self.style.text, f32::INFINITY);
            if !job.text.is_empty() {
                *w += ui.painter().layout_job(job).rect.width();
            }
            text.clear();
        };
        for i in inl {
            match i {
                Inline::Image { url, .. } => {
                    flush(&mut text, &mut w);
                    w += match self.images.get(url) {
                        Some(Img::Ready { size, .. }) => size.x.min(self.style.max_width.max(32.0)),
                        _ => self.style.body.size,
                    };
                }
                other => text.push(other.clone()),
            }
        }
        flush(&mut text, &mut w);
        w
    }

    /// Build the `LayoutJob` for a run of inlines, with the char ranges of its links and its bold
    /// runs.
    ///
    /// Shared by drawing and by the table's column measurement, deliberately: a column sized from
    /// a *different* layout than the one painted is a column whose text spills into its neighbour
    /// — which is exactly what happened when this measured plain body text while the cells drew
    /// bold headers and monospace `` `code` `` spans.
    fn inline_job(&self, inl: &[Inline], font: &egui::FontId, color: egui::Color32, wrap: f32) -> InlineJob {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap;
        // `(href, char start, char end)` — char counts, because that is what `CCursor` indexes.
        let mut links: Vec<(String, usize, usize)> = Vec::new();
        // Bold runs, same units: egui cannot synthesise bold, so these are re-drawn offset.
        let mut bolds: Vec<(usize, usize)> = Vec::new();
        let mut chars = 0usize;
        for i in inl {
            match i {
                Inline::Text { text, emph, link } => {
                    let fmt = self.fmt(*emph, font, color, link.is_some());
                    let n = text.chars().count();
                    job.append(text, 0.0, fmt);
                    if let Some(u) = link {
                        // Merge with the previous range when it is the same link, so a styled
                        // fragment inside one link does not become several hit rects.
                        match links.last_mut() {
                            Some((pu, _, pe)) if pu == u && *pe == chars => *pe = chars + n,
                            _ => links.push((u.clone(), chars, chars + n)),
                        }
                    }
                    if emph.bold {
                        match bolds.last_mut() {
                            Some((_, pe)) if *pe == chars => *pe = chars + n,
                            _ => bolds.push((chars, chars + n)),
                        }
                    }
                    chars += n;
                }
                Inline::SoftBreak => {
                    job.append(" ", 0.0, self.fmt(Emph::default(), font, color, false));
                    chars += 1;
                }
                Inline::HardBreak => {
                    job.append("\n", 0.0, self.fmt(Emph::default(), font, color, false));
                    chars += 1;
                }
                Inline::FootRef(l) => {
                    let txt = format!("[{l}]");
                    let n = txt.chars().count();
                    let mut f = self.fmt(Emph { sup: true, ..Default::default() }, font, color, true);
                    f.underline = egui::Stroke::NONE;
                    job.append(&txt, 0.0, f);
                    links.push((format!("#fn-{l}"), chars, chars + n));
                    chars += n;
                }
                // Handled by `flow`; unreachable here.
                Inline::Image { .. } => {}
            }
        }
        (job, links, bolds)
    }

    /// Selectable path: one galley, link rects recovered by char range.
    fn text_run(
        &mut self,
        ui: &mut egui::Ui,
        inl: &[Inline],
        font: egui::FontId,
        color: egui::Color32,
        width: f32,
    ) {
        let (job, links, bolds) = self.inline_job(inl, &font, color, width);
        if job.text.is_empty() {
            return;
        }
        // Lay out once and keep the galley: the Label paints exactly this, so every rect derived
        // from it is exactly where the glyphs are.
        let galley = ui.painter().layout_job(job.clone());
        let resp = ui.add(egui::Label::new(galley.clone()).selectable(true));
        let origin = resp.rect.min.to_vec2();
        // Faux bold: epaint has no synthetic weight and we do not ship a bold face, so a bold run
        // is the same glyphs drawn again a fraction of a pixel to the side — the trick the
        // metadata OSD already uses for its headline.
        if !bolds.is_empty() {
            let painter = ui.painter().with_clip_rect(ui.clip_rect());
            for (a, b) in &bolds {
                for r in rects_for_chars(&galley, *a, *b) {
                    // Re-drawing the whole galley clipped to the bold run's rect keeps the glyph
                    // positions identical to the first pass — no re-layout, no drift.
                    let p = painter.with_clip_rect(r.translate(origin));
                    p.galley(resp.rect.min + egui::vec2(0.6, 0.0), galley.clone(), color);
                }
            }
        }
        // Links last, so they are on top and take the click before the Label's text selection.
        for (url, a, b) in &links {
            for (n, r) in rects_for_chars(&galley, *a, *b).into_iter().enumerate() {
                let rect = r.translate(origin);
                let id = ui.id().with(("md_link", url.as_str(), *a, n));
                let r2 = ui.interact(rect, id, egui::Sense::click());
                if r2.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    // A slightly brighter underline on hover, drawn where the text actually is.
                    ui.painter().hline(
                        rect.x_range(),
                        rect.bottom() - 1.0,
                        (1.0, self.style.link),
                    );
                }
                if r2.clicked() {
                    self.action = Some(self.link_action(url));
                }
                r2.on_hover_text(url.clone());
            }
        }
    }

    /// Flowing path: words and images in a `horizontal_wrapped`. Used only for a paragraph that
    /// contains an image, where a single galley cannot express the layout.
    fn flow(
        &mut self,
        ui: &mut egui::Ui,
        inl: &[Inline],
        font: egui::FontId,
        color: egui::Color32,
        width: f32,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.set_max_width(width);
            // Words carry their own trailing space, so egui's inter-widget spacing must go.
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.spacing_mut().item_spacing.y = 2.0;
            for i in inl {
                match i {
                    Inline::Text { text, emph, link } => {
                        // One widget per word: that is what lets the row wrap between words.
                        for word in split_words(text) {
                            if word == "\u{00a0}" {
                                continue;
                            }
                            let mut job = egui::text::LayoutJob::default();
                            job.wrap.max_width = f32::INFINITY;
                            job.append(&word, 0.0, self.fmt(*emph, &font, color, link.is_some()));
                            let galley = ui.painter().layout_job(job);
                            let sense = if link.is_some() {
                                egui::Sense::click()
                            } else {
                                egui::Sense::hover()
                            };
                            let (rect, resp) = ui.allocate_exact_size(galley.size(), sense);
                            ui.painter().galley(rect.min, galley, color);
                            if let Some(u) = link {
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if resp.clicked() {
                                    self.action = Some(self.link_action(u));
                                }
                            }
                        }
                    }
                    Inline::SoftBreak => {
                        let mut job = egui::text::LayoutJob::default();
                        job.append(" ", 0.0, self.fmt(Emph::default(), &font, color, false));
                        let galley = ui.painter().layout_job(job);
                        let (rect, _) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
                        ui.painter().galley(rect.min, galley, color);
                    }
                    Inline::HardBreak => ui.end_row(),
                    Inline::FootRef(l) => {
                        ui.label(
                            egui::RichText::new(format!("[{l}]"))
                                .color(self.style.link)
                                .size(font.size * 0.75),
                        );
                    }
                    Inline::Image { url, alt, title, link } => {
                        self.image(ui, url, alt, title, link.as_deref(), width);
                    }
                }
            }
        });
    }

    /// One image. Requests it from the host on first sight, draws a placeholder until it lands.
    fn image(
        &mut self,
        ui: &mut egui::Ui,
        url: &str,
        alt: &str,
        title: &str,
        link: Option<&str>,
        width: f32,
    ) {
        match self.images.get(url) {
            Some(Img::Ready { tex, size }) => {
                // Clamp to the text column by aspect, and never upscale: a 16px icon should stay
                // a 16px icon rather than being blown up to the column width.
                let max_w = width.max(32.0);
                let scale = (max_w / size.x).min(1.0);
                let draw = *size * scale;
                let sense = if link.is_some() {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                };
                let (rect, resp) = ui.allocate_exact_size(draw, sense);
                // NEAREST would be wrong here: a screenshot shrunk to the column must area-average
                // (the project's downscale rule), and these are documents, not pixel art.
                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                let tip = if !title.is_empty() {
                    title
                } else {
                    alt
                };
                if !tip.is_empty() {
                    resp.clone().on_hover_text(tip.to_string());
                }
                if let Some(u) = link {
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        self.action = Some(self.link_action(u));
                    }
                }
            }
            Some(Img::Failed) => {
                let label = if alt.is_empty() {
                    format!("🖼 {}", short_url(url))
                } else {
                    format!("🖼 {alt}")
                };
                ui.label(
                    egui::RichText::new(label)
                        .color(self.style.weak)
                        .italics()
                        .size(self.style.body.size * 0.9),
                )
                .on_hover_text(url.to_string());
            }
            Some(Img::Pending) => {
                ui.add(egui::Spinner::new().size(self.style.body.size));
            }
            None => {
                self.want_images.push(url.to_string());
                ui.add(egui::Spinner::new().size(self.style.body.size));
            }
        }
    }

    /// Classify a link target. `#anchor` scrolls, a URL goes to the OS, anything else is a path
    /// relative to the document — which is how a sibling `.md` or a screenshot is linked.
    fn link_action(&self, url: &str) -> Action {
        if let Some(a) = url.strip_prefix('#') {
            return Action::Anchor(a.to_string());
        }
        if is_url(url) {
            return Action::Url(url.to_string());
        }
        // Strip a fragment/query before touching the filesystem: `doc.md#usage` is a file.
        let path = url.split(['#', '?']).next().unwrap_or(url);
        let decoded = percent_decode(path);
        let p = std::path::Path::new(&decoded);
        Action::Local(if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base.join(p)
        })
    }
}

/// The rectangles a char range occupies in a laid-out galley — one per wrapped row, in galley-local
/// coordinates.
///
/// This is what keeps a link's clickable area honest: it is measured from the very galley that was
/// painted, so wrapping, a custom font and a mid-word break all land correctly.
fn rects_for_chars(galley: &egui::Galley, a: usize, b: usize) -> Vec<egui::Rect> {
    use egui::text::CCursor;
    if b <= a || galley.rows.is_empty() {
        return Vec::new();
    }
    let la = galley.layout_from_cursor(CCursor::new(a));
    let lb = galley.layout_from_cursor(CCursor::new(b));
    let mut out = Vec::new();
    for row in la.row..=lb.row.min(galley.rows.len() - 1) {
        let start = if row == la.row { la.column } else { 0 };
        let end = if row == lb.row {
            lb.column
        } else {
            galley.rows[row].char_count_excluding_newline()
        };
        if end <= start {
            continue;
        }
        let r1 = galley.pos_from_layout_cursor(&egui::epaint::text::cursor::LayoutCursor {
            row,
            column: start,
        });
        let r2 = galley.pos_from_layout_cursor(&egui::epaint::text::cursor::LayoutCursor { row, column: end });
        out.push(r1.union(r2));
    }
    out
}

/// Split text into wrappable chunks, each keeping its own trailing whitespace so the flow layout
/// spaces words without relying on `item_spacing`.
fn split_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            cur.push(ch);
            in_space = true;
        } else {
            if in_space {
                out.push(std::mem::take(&mut cur));
                in_space = false;
            }
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Is this a URL we should hand to the OS rather than resolve on disk?
pub fn is_url(s: &str) -> bool {
    let low = s.to_ascii_lowercase();
    low.starts_with("http://")
        || low.starts_with("https://")
        || low.starts_with("mailto:")
        || low.starts_with("ftp://")
        || low.starts_with("data:")
}

/// `%20` and friends — a link to "my file.png" is written encoded.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A long URL shortened for a placeholder label.
fn short_url(url: &str) -> String {
    let tail = url.rsplit('/').next().unwrap_or(url);
    if tail.is_empty() {
        url.chars().take(40).collect()
    } else {
        tail.chars().take(40).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one-line smoke test that the event walk builds the tree the renderer expects.
    #[test]
    fn headings_paragraphs_and_emphasis() {
        let d = parse("# Title\n\nHello **bold** and *italic* and `code`.\n");
        assert_eq!(d.blocks.len(), 2);
        let Block::Heading { level, anchor, inl } = &d.blocks[0] else {
            panic!("expected heading, got {:?}", d.blocks[0]);
        };
        assert_eq!(*level, 1);
        assert_eq!(anchor, "title");
        assert_eq!(inline_text(inl), "Title");
        assert_eq!(d.outline, vec![(1, "Title".to_string(), "title".to_string())]);
        let Block::Para(p) = &d.blocks[1] else {
            panic!("expected paragraph");
        };
        // Emphasis is carried as flags on runs, so the text survives intact.
        assert_eq!(inline_text(p), "Hello bold and italic and code.");
        assert!(p.iter().any(|i| matches!(i, Inline::Text { emph, .. } if emph.bold)));
        assert!(p.iter().any(|i| matches!(i, Inline::Text { emph, .. } if emph.italic)));
        assert!(p.iter().any(|i| matches!(i, Inline::Text { emph, .. } if emph.code)));
    }

    #[test]
    fn duplicate_headings_get_distinct_anchors() {
        // Two "Usage" sections must not both answer to `#usage`, or the outline scrolls to the
        // wrong one — GitHub's own rule.
        let d = parse("## Usage\n\n## Usage\n\n## Usage\n");
        let anchors: Vec<_> = d.outline.iter().map(|(_, _, a)| a.as_str()).collect();
        assert_eq!(anchors, vec!["usage", "usage-1", "usage-2"]);
    }

    #[test]
    fn code_fence_keeps_language_and_body() {
        let d = parse("```rust\nfn main() {}\n```\n");
        let Block::Code { lang, text } = &d.blocks[0] else {
            panic!("expected code block");
        };
        assert_eq!(lang, "rust");
        assert_eq!(text, "fn main() {}\n");
    }

    #[test]
    fn fence_info_maps_to_a_highlighter_extension() {
        // The highlighter is keyed by extension; a fence names a language.
        assert_eq!(fence_ext("rust"), "rs");
        assert_eq!(fence_ext("bash"), "sh");
        assert_eq!(fence_ext("QB64"), "bas");
        // Info strings carry more than the language.
        assert_eq!(fence_ext("rust,ignore"), "rs");
        assert_eq!(fence_ext("js {highlight=2}"), "js");
        // An unknown language renders plain rather than guessing.
        assert_eq!(fence_ext("brainfuck"), "");
        assert_eq!(fence_ext(""), "");
    }

    #[test]
    fn links_and_images_carry_their_targets() {
        let d = parse("[text](https://example.com) ![alt](pic.png \"t\")\n");
        let Block::Para(p) = &d.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(p.iter().any(
            |i| matches!(i, Inline::Text { link: Some(u), .. } if u == "https://example.com")
        ));
        let img = p
            .iter()
            .find_map(|i| match i {
                Inline::Image { url, alt, title, .. } => Some((url, alt, title)),
                _ => None,
            })
            .expect("image");
        assert_eq!((img.0.as_str(), img.1.as_str(), img.2.as_str()), ("pic.png", "alt", "t"));
    }

    #[test]
    fn badge_is_an_image_that_is_also_a_link() {
        // `[![alt](img)](href)` — every README's top line. The image must carry the *outer* href,
        // or clicking a badge does nothing.
        let d = parse("[![build](https://img.shields.io/b.svg)](https://ci.example/job)\n");
        let Block::Para(p) = &d.blocks[0] else {
            panic!("expected paragraph");
        };
        let Some(Inline::Image { url, link, .. }) = p.first() else {
            panic!("expected image, got {p:?}");
        };
        assert_eq!(url, "https://img.shields.io/b.svg");
        assert_eq!(link.as_deref(), Some("https://ci.example/job"));
    }

    #[test]
    fn nested_lists_and_task_items() {
        let d = parse("- [x] done\n- [ ] todo\n  - nested\n");
        let Block::List { ordered, items, .. } = &d.blocks[0] else {
            panic!("expected list, got {:?}", d.blocks[0]);
        };
        assert!(!ordered);
        assert_eq!(items.len(), 2);
        // The task marker arrives inside the item's paragraph and must be hoisted onto the item.
        assert_eq!(items[0].task, Some(true));
        assert_eq!(items[1].task, Some(false));
        // A nested list is a block of its parent item, not a sibling of the outer list.
        assert!(items[1].blocks.iter().any(|b| matches!(b, Block::List { .. })));
    }

    #[test]
    fn a_tight_list_item_is_one_paragraph_not_one_per_run() {
        // A tight list emits NO `Paragraph` events: the item's runs arrive bare. Without an
        // implicit paragraph each run became its own block, so a README bullet with a bold lead-in
        // rendered as a stack of one-line fragments instead of a wrapped sentence.
        let d = parse("- **Bold lead** — then plain, then **bold** again, over\n  two source lines.\n");
        let Block::List { items, .. } = &d.blocks[0] else {
            panic!("expected list, got {:?}", d.blocks[0]);
        };
        assert_eq!(items.len(), 1);
        let paras = items[0]
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Para(_)))
            .count();
        assert_eq!(paras, 1, "item split into {paras} paragraphs: {:?}", items[0].blocks);
        let Block::Para(p) = &items[0].blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(
            inline_text(p),
            "Bold lead — then plain, then bold again, over two source lines."
        );
        // …and the emphasis is still carried on the runs, not lost in the merge.
        assert!(p.iter().any(|i| matches!(i, Inline::Text { emph, .. } if emph.bold)));
    }

    #[test]
    fn a_tight_item_with_a_nested_list_keeps_both() {
        // The implicit paragraph must close when a nested block starts, or the nested list would
        // land inside the text run.
        let d = parse("- lead text\n  - nested one\n  - nested two\n");
        let Block::List { items, .. } = &d.blocks[0] else {
            panic!("expected list");
        };
        let kinds = &items[0].blocks;
        assert!(matches!(kinds.first(), Some(Block::Para(_))), "{kinds:?}");
        assert!(kinds.iter().any(|b| matches!(b, Block::List { .. })), "{kinds:?}");
    }

    #[test]
    fn ordered_list_keeps_its_start_number() {
        let d = parse("3. three\n4. four\n");
        let Block::List { ordered, start, items } = &d.blocks[0] else {
            panic!("expected list");
        };
        assert!(ordered);
        assert_eq!(*start, 3);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn gfm_table_splits_head_rows_and_alignment() {
        let d = parse("| a | b |\n|:--|--:|\n| 1 | 2 |\n| 3 | 4 |\n");
        let Block::Table { align, head, rows } = &d.blocks[0] else {
            panic!("expected table, got {:?}", d.blocks[0]);
        };
        assert_eq!(align, &vec![Align::Left, Align::Right]);
        assert_eq!(head.len(), 2);
        assert_eq!(inline_text(&head[0]), "a");
        // The head is NOT a row: pulldown emits no TableRow for it, and double-counting it would
        // render the header twice.
        assert_eq!(rows.len(), 2);
        assert_eq!(inline_text(&rows[1][1]), "4");
    }

    #[test]
    fn blockquote_alert_is_recognised() {
        let d = parse("> [!WARNING]\n> careful\n");
        let Block::Quote { alert, blocks } = &d.blocks[0] else {
            panic!("expected quote, got {:?}", d.blocks[0]);
        };
        assert_eq!(*alert, Some(Alert::Warning));
        assert!(!blocks.is_empty());
        // A plain quote stays a plain quote.
        let d = parse("> just a quote\n");
        assert!(matches!(d.blocks[0], Block::Quote { alert: None, .. }));
    }

    #[test]
    fn front_matter_is_metadata_not_a_rule_plus_yaml() {
        // Without metadata parsing this is a thematic break followed by a stray paragraph.
        let d = parse("---\ntitle: Hi\n---\n\n# Real\n");
        let Block::FrontMatter(f) = &d.blocks[0] else {
            panic!("expected front matter, got {:?}", d.blocks[0]);
        };
        assert!(f.contains("title: Hi"));
        assert!(matches!(d.blocks[1], Block::Heading { level: 1, .. }));
    }

    #[test]
    fn footnotes_are_collected_at_the_end() {
        let d = parse("See[^a].\n\n[^a]: the note\n");
        assert!(matches!(d.blocks.last(), Some(Block::Footnote { .. })));
        let Block::Para(p) = &d.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(p.iter().any(|i| matches!(i, Inline::FootRef(l) if l == "a")));
    }

    #[test]
    fn html_block_keeps_the_image_and_its_link() {
        // The centered-logo-and-badges opening of a great many READMEs. Printing the tags would
        // read as a rendering bug; dropping the block would lose the logo.
        let d = parse(
            "<p align=\"center\">\n<a href=\"https://x.test\"><img src=\"logo.png\" alt=\"L\"></a>\n</p>\n",
        );
        let imgs: Vec<_> = d
            .blocks
            .iter()
            // `align="center"` wraps the paragraph in a Center block, so look inside one.
            .flat_map(|b| match b {
                Block::Para(p) => p.clone(),
                Block::Center(inner) => inner
                    .iter()
                    .flat_map(|b| match b {
                        Block::Para(p) => p.clone(),
                        _ => Vec::new(),
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .filter_map(|i| match i {
                Inline::Image { url, link, .. } => Some((url, link)),
                _ => None,
            })
            .collect();
        assert_eq!(imgs.len(), 1, "blocks: {:?}", d.blocks);
        assert_eq!(imgs[0].0, "logo.png");
        assert_eq!(imgs[0].1.as_deref(), Some("https://x.test"));
    }

    #[test]
    fn details_becomes_a_collapsing_section() {
        let d = parse("<details>\n<summary>More</summary>\n\nHidden *text*\n\n</details>\n");
        let Some(Block::Details { summary, blocks }) =
            d.blocks.iter().find(|b| matches!(b, Block::Details { .. }))
        else {
            panic!("expected details, got {:?}", d.blocks);
        };
        assert_eq!(inline_text(summary), "More");
        assert!(blocks.iter().any(|b| matches!(b, Block::Para(_))));
    }

    #[test]
    fn a_centred_html_block_is_modelled_as_centred() {
        // The opening of almost every README: a logo (and usually badges) in a centred <p>.
        for src in [
            "<p align=\"center\">\n<img src=\"logo.png\">\n</p>\n",
            "<div align=center>\n<img src=\"logo.png\">\n</div>\n",
            "<center>\n<img src=\"logo.png\">\n</center>\n",
        ] {
            let d = parse(src);
            assert!(
                d.blocks.iter().any(|b| matches!(b, Block::Center(_))),
                "not centred for {src:?}: {:?}",
                d.blocks
            );
        }
        // …and a plain HTML block is NOT centred.
        let d = parse("<p>\n<img src=\"logo.png\">\n</p>\n");
        assert!(!d.blocks.iter().any(|b| matches!(b, Block::Center(_))));
    }

    #[test]
    fn html_attributes_parse_quoted_bare_and_adjacent() {
        assert_eq!(attr("<img src=\"a.png\">", "src").as_deref(), Some("a.png"));
        assert_eq!(attr("<img src='a.png'>", "src").as_deref(), Some("a.png"));
        assert_eq!(attr("<img src=a.png>", "src").as_deref(), Some("a.png"));
        assert_eq!(attr("<img width=20 src=\"a.png\">", "src").as_deref(), Some("a.png"));
        // A word-boundary match, so a longer attribute name doesn't answer for a shorter one.
        assert_eq!(attr("<img data-src=\"b.png\">", "src"), None);
        assert_eq!(attr("<img alt=\"x\">", "src"), None);
    }

    #[test]
    fn hard_break_from_html_and_from_markdown() {
        let d = parse("one<br>two\n");
        let Block::Para(p) = &d.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(p.iter().any(|i| matches!(i, Inline::HardBreak)));
        let d = parse("one  \ntwo\n");
        let Block::Para(p) = &d.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(p.iter().any(|i| matches!(i, Inline::HardBreak)));
    }

    #[test]
    fn strikethrough_and_autolink() {
        let d = parse("~~gone~~ <https://auto.test>\n");
        let Block::Para(p) = &d.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(p.iter().any(|i| matches!(i, Inline::Text { emph, .. } if emph.strike)));
        assert!(p
            .iter()
            .any(|i| matches!(i, Inline::Text { link: Some(u), .. } if u == "https://auto.test")));
    }

    #[test]
    fn bare_urls_in_prose_become_links() {
        // GitHub linkifies these; pulldown-cmark does not, so we do it ourselves.
        let d = parse("see https://example.com/a?b=1 for more.\n");
        let Block::Para(p) = &d.blocks[0] else {
            panic!("expected paragraph");
        };
        let links: Vec<&str> = p
            .iter()
            .filter_map(|i| match i {
                Inline::Text { link: Some(u), .. } => Some(u.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(links, vec!["https://example.com/a?b=1"]);
        // The sentence survives intact around it.
        assert_eq!(inline_text(p), "see https://example.com/a?b=1 for more.");
    }

    #[test]
    fn autolink_leaves_trailing_punctuation_and_existing_links_alone() {
        // A full stop ends the sentence, not the URL.
        let d = parse("go to https://example.com.\n");
        let Block::Para(p) = &d.blocks[0] else { panic!("para") };
        let u = p
            .iter()
            .find_map(|i| match i {
                Inline::Text { link: Some(u), .. } => Some(u.clone()),
                _ => None,
            })
            .expect("a link");
        assert_eq!(u, "https://example.com");
        assert_eq!(inline_text(p), "go to https://example.com.");
        // A URL inside `code` is being shown, not offered — and a real link is not re-wrapped.
        let d = parse("`https://example.com` and [x](https://other.test)\n");
        let Block::Para(p) = &d.blocks[0] else { panic!("para") };
        assert!(p
            .iter()
            .all(|i| !matches!(i, Inline::Text { emph, link: Some(_), .. } if emph.code)));
        let links: Vec<&str> = p
            .iter()
            .filter_map(|i| match i {
                Inline::Text { link: Some(u), .. } => Some(u.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(links, vec!["https://other.test"]);
    }

    #[test]
    fn an_empty_document_parses_to_nothing() {
        // The viewer must not special-case emptiness; it should just render no blocks.
        assert_eq!(parse("").blocks.len(), 0);
        assert_eq!(parse("   \n\n").blocks.len(), 0);
    }

    #[test]
    fn unbalanced_details_still_yields_its_content() {
        // A truncated or hand-broken file must not swallow the rest of the document: `finish`
        // flushes every container the walk left open.
        let d = parse("<details>\n<summary>S</summary>\n\nkept\n");
        assert!(
            d.blocks.iter().any(|b| match b {
                Block::Para(p) => inline_text(p).contains("kept"),
                Block::Details { blocks, .. } => blocks.iter().any(|b| matches!(b, Block::Para(p) if inline_text(p).contains("kept"))),
                _ => false,
            }),
            "content lost: {:?}",
            d.blocks
        );
    }
}
