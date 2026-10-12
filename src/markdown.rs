//! Markdown to Pango markup, for showing release notes in a `gtk::Label`.
//!
//! Labels cannot lay out block elements, so this covers what release notes
//! use in practice: headings, emphasis, code, links, lists, quotes and rules.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// Start a new block, leaving a blank line after the previous one.
fn block_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        while !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
    }
}

/// Start a new line, without a blank one.
fn line_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

pub fn to_pango(markdown: &str) -> String {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut out = String::new();
    // One entry per open list: the next number, or `None` for bullets.
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote_depth = 0usize;
    let mut at_line_start = true;

    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    // A tight list item's text follows its marker directly.
                    if !at_line_start || lists.is_empty() {
                        block_break(&mut out);
                    }
                }
                Tag::Heading { level, .. } => {
                    block_break(&mut out);
                    let size = match level {
                        HeadingLevel::H1 => "xx-large",
                        HeadingLevel::H2 => "x-large",
                        HeadingLevel::H3 => "large",
                        _ => "medium",
                    };
                    out.push_str(&format!("<span size=\"{size}\" weight=\"bold\">"));
                }
                Tag::BlockQuote(_) => {
                    block_break(&mut out);
                    quote_depth += 1;
                }
                Tag::CodeBlock(_) => {
                    block_break(&mut out);
                    out.push_str("<tt>");
                }
                Tag::List(start) => {
                    if lists.is_empty() {
                        block_break(&mut out);
                    } else {
                        line_break(&mut out);
                    }
                    lists.push(start);
                }
                Tag::Item => {
                    line_break(&mut out);
                    out.push_str(&"    ".repeat(lists.len().saturating_sub(1)));
                    match lists.last_mut() {
                        Some(Some(n)) => {
                            out.push_str(&format!("{n}. "));
                            *n += 1;
                        }
                        _ => out.push_str("• "),
                    }
                    at_line_start = true;
                    continue;
                }
                Tag::Emphasis => out.push_str("<i>"),
                Tag::Strong => out.push_str("<b>"),
                Tag::Strikethrough => out.push_str("<s>"),
                Tag::Link { dest_url, .. } => {
                    out.push_str("<a href=\"");
                    escape(&dest_url, &mut out);
                    out.push_str("\">");
                }
                Tag::Image { .. } => {}
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => out.push_str("</span>"),
                TagEnd::BlockQuote(_) => quote_depth = quote_depth.saturating_sub(1),
                TagEnd::CodeBlock => {
                    out.push_str("</tt>");
                }
                TagEnd::List(_) => {
                    lists.pop();
                }
                TagEnd::Emphasis => out.push_str("</i>"),
                TagEnd::Strong => out.push_str("</b>"),
                TagEnd::Strikethrough => out.push_str("</s>"),
                TagEnd::Link => out.push_str("</a>"),
                _ => {}
            },
            Event::Text(text) => {
                if quote_depth > 0 && at_line_start {
                    out.push_str(&"▎ ".repeat(quote_depth));
                }
                escape(&text, &mut out);
            }
            Event::Code(code) => {
                out.push_str("<tt>");
                escape(&code, &mut out);
                out.push_str("</tt>");
            }
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => {
                block_break(&mut out);
                out.push_str("────────────");
            }
            Event::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),
            // Raw HTML (comments, tags) has no place in a label.
            Event::Html(_) | Event::InlineHtml(_) => {}
            _ => {}
        }
        at_line_start = out.ends_with('\n') || out.is_empty();
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_inline_and_blocks() {
        let md = "# Title\n\nSome **bold**, *soft* and `code` <b>.\n\n- one\n- two\n  - deep\n\n1. a\n2. b\n\n[site](https://x.y/?a=1&b=2)";
        let out = to_pango(md);
        assert!(out.starts_with("<span size=\"xx-large\" weight=\"bold\">Title</span>\n\n"));
        assert!(out.contains("<b>bold</b>, <i>soft</i> and <tt>code</tt>"));
        assert!(out.contains("• one\n• two\n    • deep"));
        assert!(out.contains("1. a\n2. b"));
        assert!(out.contains("<a href=\"https://x.y/?a=1&amp;b=2\">site</a>"));
    }

    #[test]
    fn escapes_markup_and_drops_html() {
        assert_eq!(to_pango("a < b & c"), "a &lt; b &amp; c");
        assert_eq!(to_pango("<!-- hidden -->\n\ntext"), "text");
    }
}
