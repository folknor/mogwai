// Markdown -> ANSI terminal renderer for `mogwai man` (bundled reference docs).
//
// A pulldown_cmark event emitter that styles headings, code, tables, lists,
// block quotes / GitHub alerts, and footnotes for a terminal, and strips all
// ANSI when colour is disabled.

use owo_colors::{OwoColorize, Style};
use pulldown_cmark::{
    BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd,
};
use std::collections::HashMap;

/// ANSI escape that turns on the terminal "crossed out" attribute.
const STRIKE_ON: &str = "\x1b[9m";
/// ANSI escape that turns off the "crossed out" attribute.
const STRIKE_OFF: &str = "\x1b[29m";

/// Render `markdown` to a styled terminal string. When `no_color` is set
/// (`--no-color` not a TTY / `NO_COLOR`), all ANSI escapes are stripped from
/// the result - the emitter hardcodes some styling (alert labels, H1 inverse)
/// that a "plain theme" can't suppress, so stripping is the reliable path.
pub(crate) fn render(markdown: &str, no_color: bool) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    let out = TerminalEmitter::new(Parser::new_ext(markdown, options), Theme::glow()).run();
    if no_color { strip_ansi(&out) } else { out }
}

/// Remove ANSI CSI escape sequences (`\x1b[ ... <letter>`) from `s`.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Consume `[` and everything up to and including the terminating
            // letter (e.g. `m`). Lone ESC without `[` is dropped too.
            if chars.peek() == Some(&'[') {
                chars.next();
                for nc in chars.by_ref() {
                    if nc.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

enum ListState {
    Ordered { index: usize },
    Unordered,
}

#[derive(Clone, Debug)]
struct Theme {
    heading: Style,
    block_quote: Style,
    quote_bar: Style,
    code: Style,
    link: Style,
    list_marker: Style,
    rule: Style,
    table_header: Style,
    footnote: Style,
}

impl Theme {
    fn glow() -> Self {
        Self {
            heading: Style::new().fg_rgb::<0xf0, 0x71, 0x78>().bold(),
            block_quote: Style::new().fg_rgb::<0xd4, 0xbf, 0xff>(),
            quote_bar: Style::new().fg_rgb::<0xcb, 0xcc, 0xc6>().bold(),
            code: Style::new().bright_yellow(),
            link: Style::new().fg_rgb::<0x95, 0xe6, 0xcb>(),
            list_marker: Style::new().fg_rgb::<0xba, 0xe6, 0x7e>().bold(),
            rule: Style::new().bright_black(),
            table_header: Style::new().bright_white().bold(),
            footnote: Style::new().bright_black(),
        }
    }
}

struct TerminalEmitter<I> {
    iter: I,
    theme: Theme,
    end_newline: bool,
    in_non_writing_block: bool,
    in_heading: bool,
    heading_level: Option<HeadingLevel>,
    h1_started: bool,
    in_block_quote: bool,
    in_code_block: bool,
    in_link: bool,
    in_table_head: bool,
    in_table: bool,
    in_table_cell: bool,
    list_stack: Vec<ListState>,
    table_header_row: Option<Vec<String>>,
    table_rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    pending_list_marker: Option<String>,
    numbers: HashMap<String, usize>,
    link_stack: Vec<String>,
    code_block_lang: String,
    code_block_buf: String,
}

impl<'a, I> TerminalEmitter<I>
where
    I: Iterator<Item = Event<'a>>,
{
    fn new(iter: I, theme: Theme) -> Self {
        Self {
            iter,
            theme,
            end_newline: false,
            in_non_writing_block: false,
            in_heading: false,
            heading_level: None,
            h1_started: false,
            in_block_quote: false,
            in_code_block: false,
            in_link: false,
            in_table_head: false,
            in_table: false,
            in_table_cell: false,
            list_stack: Vec::new(),
            table_header_row: None,
            table_rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            pending_list_marker: None,
            numbers: HashMap::new(),
            link_stack: Vec::new(),
            code_block_lang: String::new(),
            code_block_buf: String::new(),
        }
    }

    fn run(&mut self) -> String {
        let mut out = String::new();
        while let Some(event) = self.iter.next() {
            match event {
                Event::Start(tag) => self.start_tag(&mut out, tag),
                Event::End(tag) => self.end_tag(&mut out, tag),
                Event::Text(text) => {
                    if !self.in_non_writing_block {
                        if self.in_code_block {
                            self.code_block_buf.push_str(&text);
                        } else if self.in_table_cell {
                            self.push_table_text(&text);
                        } else {
                            self.flush_pending_marker(&mut out);
                            self.push_text(&mut out, &text);
                        }
                        self.end_newline = text.ends_with('\n');
                    }
                }
                Event::Code(text) => {
                    if self.in_table_cell {
                        self.push_table_text(&text);
                    } else {
                        self.flush_pending_marker(&mut out);
                        out.push('`');
                        out.push_str(&text.style(self.theme.code).to_string());
                        out.push('`');
                    }
                }
                Event::InlineMath(text) => {
                    if self.in_table_cell {
                        self.push_table_text("$");
                        self.push_table_text(&text);
                        self.push_table_text("$");
                    } else {
                        self.flush_pending_marker(&mut out);
                        out.push('$');
                        self.push_text(&mut out, &text);
                        out.push('$');
                    }
                }
                Event::DisplayMath(text) => {
                    if self.in_table_cell {
                        self.push_table_text("$$");
                        self.push_table_text(&text);
                        self.push_table_text("$$");
                    } else {
                        self.flush_pending_marker(&mut out);
                        out.push_str("$$");
                        self.push_text(&mut out, &text);
                        out.push_str("$$");
                    }
                }
                Event::Html(_) | Event::InlineHtml(_) => {
                    // Skip raw HTML for terminal output.
                }
                Event::SoftBreak => {
                    if self.in_table_cell {
                        self.push_table_text(" ");
                    } else {
                        self.flush_pending_marker(&mut out);
                        out.push(' ');
                    }
                }
                Event::HardBreak => {
                    if self.in_table_cell {
                        self.push_table_text(" ");
                    } else {
                        self.flush_pending_marker(&mut out);
                        out.push('\n');
                        self.end_newline = true;
                    }
                }
                Event::Rule => {
                    if self.in_table_cell {
                        self.push_table_text("-");
                    } else {
                        if !self.end_newline {
                            out.push('\n');
                        }
                        let rule = "----------------------------------------\n";
                        out.push_str(&rule.style(self.theme.rule).to_string());
                        self.end_newline = true;
                    }
                }
                Event::FootnoteReference(name) => {
                    let len = self.numbers.len() + 1;
                    let number = *self.numbers.entry(name.to_string()).or_insert(len);
                    if self.in_table_cell {
                        self.push_table_text(&format!("[^{number}]"));
                    } else {
                        self.flush_pending_marker(&mut out);
                        out.push_str(&format!("[^{number}]"));
                    }
                }
                Event::TaskListMarker(checked) => {
                    if self.in_table_cell {
                        self.push_table_text(if checked { "[x] " } else { "[ ] " });
                    } else {
                        self.render_task_marker(&mut out, checked);
                    }
                }
            }
        }
        out
    }

    fn start_tag(&mut self, out: &mut String, tag: Tag<'a>) {
        match tag {
            Tag::HtmlBlock => (),
            Tag::Paragraph | Tag::DefinitionList | Tag::DefinitionListTitle => {
                if !self.end_newline {
                    out.push('\n');
                }
            }
            Tag::Heading { level, .. } => {
                if !self.end_newline {
                    out.push('\n');
                }
                self.in_heading = true;
                self.heading_level = Some(level);
                self.h1_started = false;
                if level != HeadingLevel::H1 {
                    let head = "#"
                        .repeat(level as usize)
                        .style(self.theme.heading)
                        .to_string();
                    out.push_str(&head);
                    out.push(' ');
                }
            }
            Tag::Table(_) => {
                if !self.end_newline {
                    out.push('\n');
                }
                self.in_table = true;
                self.table_header_row = None;
                self.table_rows.clear();
            }
            Tag::TableHead => {
                self.in_table_head = true;
            }
            Tag::TableRow => {
                self.current_row.clear();
                if !self.end_newline {
                    out.push('\n');
                }
            }
            Tag::TableCell => {
                self.in_table_cell = true;
                self.current_cell.clear();
            }
            Tag::BlockQuote(kind) => {
                // GitHub-style alerts: icon + coloured label per kind.
                let alert = match kind {
                    None => None,
                    Some(BlockQuoteKind::Note) => {
                        Some(("i", "NOTE", Style::new().bright_blue().bold()))
                    }
                    Some(BlockQuoteKind::Tip) => {
                        Some(("*", "TIP", Style::new().bright_green().bold()))
                    }
                    Some(BlockQuoteKind::Important) => {
                        Some(("!", "IMPORTANT", Style::new().bright_magenta().bold()))
                    }
                    Some(BlockQuoteKind::Warning) => {
                        Some(("!", "WARNING", Style::new().bright_yellow().bold()))
                    }
                    Some(BlockQuoteKind::Caution) => {
                        Some(("x", "CAUTION", Style::new().bright_red().bold()))
                    }
                };
                if out.ends_with("\n\n") {
                    out.pop();
                }
                if !self.end_newline {
                    out.push('\n');
                }
                self.in_block_quote = true;
                out.push_str(&"|".style(self.theme.quote_bar).to_string());
                if let Some((icon, label, style)) = alert {
                    out.push(' ');
                    out.push_str(&format!("{icon} {}", label.style(style)));
                    out.push('\n');
                    out.push_str(&"|".style(self.theme.quote_bar).to_string());
                }
                out.push(' ');
            }
            Tag::CodeBlock(info) => {
                if !self.end_newline {
                    out.push('\n');
                }
                self.in_code_block = true;
                self.code_block_buf.clear();
                match info {
                    CodeBlockKind::Fenced(lang) => {
                        self.code_block_lang = lang.split(' ').next().unwrap_or("").to_string();
                        out.push_str("```");
                        out.push_str(&self.code_block_lang);
                        out.push('\n');
                    }
                    CodeBlockKind::Indented => {
                        self.code_block_lang.clear();
                        out.push_str("```\n");
                    }
                }
            }
            Tag::List(Some(start)) => {
                self.list_stack.push(ListState::Ordered {
                    index: start as usize,
                });
            }
            Tag::List(None) => {
                self.list_stack.push(ListState::Unordered);
            }
            Tag::Item => {
                if !self.end_newline {
                    out.push('\n');
                }
                // The marker opens a fresh line; record that so a LOOSE item's
                // following `Paragraph` does not push a SECOND newline and open a
                // stray blank line between the item and its body (S23).
                self.end_newline = true;
                // Indent nested bullets by list depth so architecture.md's nested
                // lists keep their hierarchy instead of rendering flat.
                // `list_stack` already holds the current list (pushed on
                // `Tag::List`), so depth 1 is top-level and gets no indent.
                let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
                let bullet = match self.list_stack.last() {
                    Some(ListState::Ordered { index }) => format!("{index}. ")
                        .style(self.theme.list_marker)
                        .to_string(),
                    Some(ListState::Unordered) | None => {
                        "* ".style(self.theme.list_marker).to_string()
                    }
                };
                self.pending_list_marker = Some(format!("{indent}{bullet}"));
            }
            Tag::DefinitionListDefinition => {
                if !self.end_newline {
                    out.push('\n');
                }
                out.push_str(": ");
            }
            // Inline formatting can be the FIRST child of a list item (e.g.
            // `- **Bold lead** - ...`). These arms emit their markup directly,
            // so flush any pending list marker first or it lands mid-markup
            // (`* ` + `**` rendering as `*** `).
            Tag::Subscript => {
                self.flush_pending_marker(out);
                out.push('~');
            }
            Tag::Superscript => {
                self.flush_pending_marker(out);
                out.push('^');
            }
            Tag::Emphasis => {
                self.flush_pending_marker(out);
                out.push_str(&"*".style(self.theme.code).to_string());
            }
            Tag::Strong => {
                self.flush_pending_marker(out);
                out.push_str(&"**".style(self.theme.code).to_string());
            }
            Tag::Strikethrough => {
                self.flush_pending_marker(out);
                out.push_str(STRIKE_ON);
            }
            Tag::Link {
                link_type: LinkType::Email,
                dest_url,
                ..
            } => {
                self.in_link = true;
                self.link_stack.push(format!("mailto:{dest_url}"));
            }
            Tag::Link { dest_url, .. } => {
                self.in_link = true;
                self.link_stack.push(dest_url.to_string());
            }
            Tag::Image { .. } => {
                // A terminal can't show images. Never dump the URL; emit a bare
                // `[image]` unless the alt text is actually informative.
                // `raw_text` drains the alt-text events.
                self.flush_pending_marker(out);
                let mut alt = String::new();
                self.raw_text(&mut alt);
                let alt = alt.trim();
                let marker = if alt.is_empty() || alt.eq_ignore_ascii_case("image") {
                    "[image]".to_string()
                } else {
                    format!("[image: {alt}]")
                };
                out.push_str(&marker.style(self.theme.code).to_string());
            }
            Tag::FootnoteDefinition(name) => {
                if !self.end_newline {
                    out.push('\n');
                }
                let len = self.numbers.len() + 1;
                let number = *self.numbers.entry(name.to_string()).or_insert(len);
                let label = format!("[^{number}]: ");
                out.push_str(&label.style(self.theme.footnote).to_string());
            }
            Tag::MetadataBlock(_) => {
                self.in_non_writing_block = true;
            }
        }
    }

    fn end_tag(&mut self, out: &mut String, tag: TagEnd) {
        match tag {
            TagEnd::HtmlBlock | TagEnd::Image => {}
            TagEnd::Paragraph => {
                out.push_str("\n\n");
                self.end_newline = true;
            }
            TagEnd::Heading(level) => {
                if level == HeadingLevel::H1 {
                    let style = self.theme.heading.bold().on_bright_black();
                    out.push_str(&" ".style(style).to_string());
                }
                out.push_str("\n\n");
                self.in_heading = false;
                self.heading_level = None;
                self.h1_started = false;
                self.end_newline = true;
            }
            TagEnd::Table => {
                if self.in_table {
                    if let Some(header) = self.table_header_row.take() {
                        self.table_rows.insert(0, header);
                    }
                    self.render_table(out);
                }
                out.push('\n');
                self.in_table = false;
                self.end_newline = true;
            }
            TagEnd::TableHead => {
                // In pulldown_cmark the header cells sit directly inside
                // TableHead (no TableRow wrapper), so capture the accumulated
                // row here.
                self.in_table_head = false;
                self.table_header_row = Some(std::mem::take(&mut self.current_row));
                self.end_newline = true;
            }
            TagEnd::TableRow => {
                if self.in_table_cell {
                    self.current_row.push(self.current_cell.trim().to_string());
                    self.in_table_cell = false;
                }
                self.table_rows.push(std::mem::take(&mut self.current_row));
                self.end_newline = true;
            }
            TagEnd::TableCell => {
                if self.in_table_cell {
                    self.current_row.push(self.current_cell.trim().to_string());
                    self.in_table_cell = false;
                }
            }
            TagEnd::BlockQuote(_) => {
                out.push('\n');
                self.in_block_quote = false;
                self.end_newline = true;
            }
            TagEnd::CodeBlock => {
                let buf = std::mem::take(&mut self.code_block_buf);
                self.code_block_lang.clear();
                out.push_str(&buf);
                if !buf.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str("```\n\n");
                self.in_code_block = false;
                self.end_newline = true;
            }
            TagEnd::List(_) => {
                let _ = self.list_stack.pop();
                out.push('\n');
                self.end_newline = true;
            }
            TagEnd::Item => {
                out.push('\n');
                self.pending_list_marker = None;
                if let Some(ListState::Ordered { index }) = self.list_stack.last_mut() {
                    *index += 1;
                }
                self.end_newline = true;
            }
            TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::FootnoteDefinition => {
                out.push('\n');
                self.end_newline = true;
            }
            TagEnd::Emphasis => out.push_str(&"*".style(self.theme.code).to_string()),
            TagEnd::Strong => out.push_str(&"**".style(self.theme.code).to_string()),
            TagEnd::Superscript => out.push('^'),
            TagEnd::Subscript => out.push('~'),
            TagEnd::Strikethrough => out.push_str(STRIKE_OFF),
            TagEnd::Link => {
                // Drop the destination URL. The link text already names the
                // target, so appending the full URL after every inline link
                // just drowns the prose in repeated URLs. The styled link text
                // stays (set in `push_text`). Pop to keep the nesting balanced.
                let _ = self.link_stack.pop();
                self.in_link = false;
            }
            TagEnd::MetadataBlock(_) => {
                self.in_non_writing_block = false;
            }
        }
    }

    fn push_text(&mut self, out: &mut String, text: &str) {
        let style = if self.in_code_block {
            Some(self.theme.code)
        } else if self.in_heading {
            if self.heading_level == Some(HeadingLevel::H1) {
                Some(self.theme.heading.bold().on_bright_black())
            } else {
                Some(self.theme.heading.bold())
            }
        } else if self.in_block_quote {
            Some(self.theme.block_quote)
        } else if self.in_table_head {
            Some(self.theme.table_header)
        } else if self.in_link {
            Some(self.theme.link.bold())
        } else {
            None
        };
        if let Some(style) = style {
            if self.heading_level == Some(HeadingLevel::H1) && !self.h1_started {
                out.push_str(&" ".style(style).to_string());
                self.h1_started = true;
            }
            out.push_str(&text.style(style).to_string());
        } else {
            out.push_str(text);
        }
    }

    fn push_table_text(&mut self, text: &str) {
        self.current_cell.push_str(text);
    }

    fn flush_pending_marker(&mut self, out: &mut String) {
        if let Some(marker) = self.pending_list_marker.take() {
            out.push_str(&marker);
        }
    }

    fn render_task_marker(&mut self, out: &mut String, checked: bool) {
        if let Some(marker) = self.pending_list_marker.take() {
            if marker.contains('*') {
                out.push_str("  ");
            } else {
                out.push_str(&marker);
            }
        }
        out.push_str(if checked { "[x] " } else { "[ ] " });
    }

    fn render_table(&self, out: &mut String) {
        if self.table_rows.is_empty() {
            return;
        }
        let mut widths = Vec::new();
        for row in &self.table_rows {
            for (i, cell) in row.iter().enumerate() {
                if widths.len() <= i {
                    widths.push(0usize);
                }
                let len = cell.chars().count();
                if len > widths[i] {
                    widths[i] = len;
                }
            }
        }
        let indent = " ";
        let header = &self.table_rows[0];
        out.push_str(indent);
        for (i, cell) in header.iter().enumerate() {
            if i > 0 {
                out.push_str(" | ");
            }
            out.push_str(&pad(cell, widths[i]));
        }
        out.push('\n');
        out.push_str(indent);
        for (i, width) in widths.iter().enumerate() {
            if i > 0 {
                out.push('+');
            }
            out.push_str(&"-".repeat(*width + if i > 0 { 2 } else { 0 }));
        }
        out.push('\n');
        for row in self.table_rows.iter().skip(1) {
            out.push_str(indent);
            for (i, cell) in row.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                out.push_str(&pad(cell, widths[i]));
            }
            out.push('\n');
        }
    }

    fn raw_text(&mut self, out: &mut String) {
        let mut nest = 0;
        for event in self.iter.by_ref() {
            match event {
                Event::Start(_) => nest += 1,
                Event::End(_) => {
                    if nest == 0 {
                        break;
                    }
                    nest -= 1;
                }
                Event::Html(_) | Event::InlineHtml(_) => {}
                Event::InlineMath(text)
                | Event::DisplayMath(text)
                | Event::Code(text)
                | Event::Text(text) => {
                    out.push_str(&text);
                    self.end_newline = text.ends_with('\n');
                }
                Event::SoftBreak | Event::HardBreak | Event::Rule => out.push(' '),
                Event::FootnoteReference(name) => {
                    let len = self.numbers.len() + 1;
                    let number = *self.numbers.entry(name.to_string()).or_insert(len);
                    out.push_str(&format!("[^{number}]"));
                }
                Event::TaskListMarker(checked) => {
                    out.push_str(if checked { "[x]" } else { "[ ]" });
                }
            }
        }
    }
}

fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strikethrough_uses_ansi_crossed_out() {
        let out = render("~~gone~~", false);
        assert!(out.contains(STRIKE_ON), "missing strike-on escape");
        assert!(out.contains(STRIKE_OFF), "missing strike-off escape");
        assert!(out.contains("gone"));
        assert!(!out.contains("~~"), "literal tildes should not leak");
    }

    #[test]
    fn heading_is_styled() {
        let out = render("## Operator precedence\n\nbody", false);
        assert!(out.contains("Operator precedence"));
        assert!(out.contains("\x1b["), "heading should carry ANSI styling");
    }

    #[test]
    fn table_is_aligned() {
        let out = render("| A | B |\n| --- | --- |\n| 1 | 2 |\n", false);
        assert!(
            out.contains('A') && out.contains('2'),
            "table cells missing: {out:?}"
        );
    }

    #[test]
    fn code_fence_keeps_language_and_body() {
        let out = render("```pine\nplot(close)\n```", false);
        assert!(out.contains("```pine"), "fence language preserved");
        assert!(out.contains("plot(close)"), "code body preserved");
    }

    #[test]
    fn alert_note_has_label() {
        let out = render("> [!NOTE]\n> body", false);
        assert!(out.contains("NOTE"), "alert label missing: {out:?}");
    }

    #[test]
    fn no_color_is_escape_free() {
        let out = render("## Heading\n\n**bold** `code`", true);
        assert!(
            !out.contains('\x1b'),
            "mono theme must emit no ANSI: {out:?}"
        );
        assert!(out.contains("Heading") && out.contains("bold"));
    }

    #[test]
    fn inline_link_keeps_text_drops_url() {
        // Reference docs link to sections and symbols; dumping the URL after
        // each link drowns the prose, so we render the text only.
        let out = render("See [the havoc model](havoc.md).", true);
        assert!(
            out.contains("the havoc model"),
            "link text missing: {out:?}"
        );
        assert!(
            !out.contains("havoc.md"),
            "link URL should be dropped: {out:?}"
        );
    }

    #[test]
    fn image_drops_url_and_uninformative_alt() {
        let out = render("![image](https://example.com/x.webp)", true);
        assert!(out.contains("[image]"), "image marker missing: {out:?}");
        assert!(
            !out.contains("example.com") && !out.contains(".webp"),
            "image URL should be dropped: {out:?}"
        );
        assert!(!out.contains("image: image"), "no doubled alt: {out:?}");
    }

    #[test]
    fn image_keeps_informative_alt() {
        let out = render("![a price chart](https://example.com/x.webp)", true);
        assert!(
            out.contains("[image: a price chart]"),
            "informative alt should survive: {out:?}"
        );
        assert!(!out.contains("example.com"), "url still dropped: {out:?}");
    }

    #[test]
    fn bold_led_list_item_keeps_marker_before_markup() {
        // A list item starting with bold must render the marker first, not glue
        // the `*` marker to the `**` strong open into `*** `.
        let out = render("- **Lead.** body\n", true);
        assert!(out.contains("* **Lead.**"), "marker before bold: {out:?}");
        assert!(!out.contains("***"), "marker glued to markup: {out:?}");
    }

    #[test]
    fn ordered_bold_led_item_keeps_marker() {
        let out = render("1. **Harvest** - domains\n", true);
        assert!(
            out.contains("1. **Harvest**"),
            "ordered marker first: {out:?}"
        );
    }

    #[test]
    fn nested_list_items_are_indented() {
        // S23: nested bullets used to render flat (the tracked list depth went
        // unused for indent). A 2-space-indented sub-item must keep its hierarchy.
        let out = render("- top\n  - nested\n", true);
        assert!(out.contains("* top"), "top-level marker present: {out:?}");
        assert!(
            out.contains("  * nested"),
            "nested marker is indented by depth: {out:?}"
        );
    }

    #[test]
    fn loose_list_item_has_no_stray_blank_line_before_marker() {
        // S23: a loose list (blank line between items) wraps each item in a
        // Paragraph. The item's own leading newline plus the Paragraph's used to
        // double up into a blank line before the marker; the fix keeps one.
        let out = render("- alpha\n\n- beta\n", true);
        assert!(
            out.starts_with("\n* alpha"),
            "no stray blank line before the first item: {out:?}"
        );
    }

    #[test]
    fn footnote_reference_and_definition() {
        let out = render("text[^1]\n\n[^1]: note", false);
        assert!(out.contains("[^1]"), "footnote ref missing");
        assert!(out.contains("note"), "footnote def missing");
    }
}
