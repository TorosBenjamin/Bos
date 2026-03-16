//! Tag-aware HTML renderer: parses HTML into styled, word-wrapped lines.
//!
//! Pure data transformation — no rendering, no external dependencies.
//! Works in both `std` and `no_std + alloc` environments.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod entity;
mod tags;

use alloc::{string::String, vec, vec::Vec};
use entity::{decode_entity, extract_attr_value, has_attr};
use tags::Tag;

// ── Data model ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct SpanStyle {
    pub bold: bool,
    pub heading: bool,
    pub link: bool,
    pub emphasis: bool,
    pub code: bool,
    pub indent: u8,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub text: String,
    pub style: SpanStyle,
    /// Target URL for link spans (`<a href="...">`).
    pub href: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct StyledLine {
    pub spans: Vec<Span>,
}

impl StyledLine {
    fn is_empty(&self) -> bool {
        self.spans.is_empty() || self.spans.iter().all(|s| s.text.is_empty())
    }
}

// ── Parser ───────────────────────────────────────────────────────────────────

/// Parse HTML into styled, word-wrapped lines suitable for rendering.
///
/// `cols` is the maximum number of characters per line (for word wrapping).
pub fn parse_html(html: &str, cols: usize) -> Vec<StyledLine> {
    let bytes = html.as_bytes();
    let mut lines: Vec<StyledLine> = vec![StyledLine::default()];
    let mut tag_stack: Vec<(Tag, Option<String>)> = Vec::new();
    let mut col: usize = 0;
    let mut i: usize = 0;
    let mut prev_space = true;

    // Nesting-aware skip: when > 0, we are inside an opaque element and
    // skip all content. The tag kind we're skipping is stored in skip_tag.
    let mut skip_depth: usize = 0;
    let mut skip_tag: Tag = Tag::Other;

    // If the HTML contains a <main> tag, skip everything before it.
    // This removes site chrome (skip-links, search bars, site headers).
    let has_main = find_tag_ci(bytes, b"<main");
    let mut found_main = has_main.is_none(); // if no <main>, render everything

    while i < bytes.len() {
        let b = bytes[i];

        // ── Pre-<main> skip ──────────────────────────────────────────────
        if !found_main {
            if b == b'<' {
                let name = parse_tag_name(&bytes[i + 1..]);
                if Tag::from_name(&name) == Tag::Main {
                    found_main = true;
                    if let Some(gt) = find_byte(&bytes[i..], b'>') {
                        i += gt + 1;
                    } else {
                        i += 1;
                    }
                    tag_stack.push((Tag::Main, None));
                    continue;
                }
            }
            i += 1;
            continue;
        }

        // ── Opaque element skip (script/style/head/nav/noscript/svg) ─────
        if skip_depth > 0 {
            if b == b'<' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    let close_name = parse_tag_name(&bytes[i + 2..]);
                    if Tag::from_name(&close_name) == skip_tag {
                        skip_depth -= 1;
                        if let Some(gt) = find_byte(&bytes[i..], b'>') {
                            i += gt + 1;
                        } else {
                            i += 1;
                        }
                        continue;
                    }
                } else {
                    let open_name = parse_tag_name(&bytes[i + 1..]);
                    let open_tag = Tag::from_name(&open_name);
                    if open_tag == skip_tag {
                        let gt = find_byte(&bytes[i..], b'>');
                        let self_closing = gt.map_or(false, |p| p > 0 && bytes[i + p - 1] == b'/');
                        if !self_closing {
                            skip_depth += 1;
                        }
                    }
                }
                if let Some(gt) = find_byte(&bytes[i..], b'>') {
                    i += gt + 1;
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }

        // ── Tag ──────────────────────────────────────────────────────────
        if b == b'<' {
            // Comment: <!-- ... -->
            if bytes[i..].starts_with(b"<!--") {
                if let Some(end) = find_substr(&bytes[i..], b"-->") {
                    i += end + 3;
                } else {
                    i += 1;
                }
                continue;
            }

            // Declarations: <!DOCTYPE ...>, <![CDATA[...]]>, etc.
            if i + 1 < bytes.len() && bytes[i + 1] == b'!' {
                if let Some(gt) = find_byte(&bytes[i..], b'>') {
                    i += gt + 1;
                } else {
                    i += 1;
                }
                continue;
            }

            // Processing instructions: <?xml ...?>
            if i + 1 < bytes.len() && bytes[i + 1] == b'?' {
                if let Some(gt) = find_byte(&bytes[i..], b'>') {
                    i += gt + 1;
                } else {
                    i += 1;
                }
                continue;
            }

            let is_closing = i + 1 < bytes.len() && bytes[i + 1] == b'/';
            let name_start = if is_closing { i + 2 } else { i + 1 };
            let tag_name = parse_tag_name(&bytes[name_start..]);

            let gt_pos = find_byte(&bytes[i..], b'>');
            let is_self_closing = gt_pos.map_or(false, |p| p > 0 && bytes[i + p - 1] == b'/');

            if tag_name.is_empty() {
                i += 1;
                continue;
            }

            let tag = Tag::from_name(&tag_name);
            let tag_content = gt_pos.map(|p| &bytes[i..i + p + 1]);

            // Handle <br> and <hr>
            if !is_closing {
                let name_lower: Vec<u8> = tag_name.iter().map(|b| b.to_ascii_lowercase()).collect();
                if name_lower == b"br" {
                    flush_line(&mut lines, &mut col);
                    if let Some(p) = gt_pos { i += p + 1; } else { i += 1; }
                    prev_space = true;
                    continue;
                }
                if name_lower == b"hr" {
                    flush_line(&mut lines, &mut col);
                    let indent = compute_indent(&tag_stack);
                    let indent_chars = (indent as usize) * 2;
                    let dash_count = cols.saturating_sub(indent_chars).min(60);
                    let style = style_from_stack(&tag_stack);
                    let mut dashes = String::new();
                    for _ in 0..indent_chars { dashes.push(' '); }
                    for _ in 0..dash_count { dashes.push('-'); }
                    lines.last_mut().unwrap().spans.push(Span { text: dashes, style, href: None });
                    flush_line(&mut lines, &mut col);
                    if let Some(p) = gt_pos { i += p + 1; } else { i += 1; }
                    prev_space = true;
                    continue;
                }
            }

            if is_closing {
                if let Some(pos) = tag_stack.iter().rposition(|(t, _)| *t == tag) {
                    tag_stack.remove(pos);
                }
                if tag.is_block() || tag.is_heading() {
                    flush_line(&mut lines, &mut col);
                    if !lines.last().map_or(true, |l| l.is_empty()) {
                        lines.push(StyledLine::default());
                        col = 0;
                    }
                }
            } else if !is_self_closing {
                let has_hidden = tag_content.map_or(false, |c| has_attr(c, b"hidden"));

                if tag.is_opaque() || has_hidden {
                    skip_depth = 1;
                    skip_tag = tag;
                    if let Some(p) = gt_pos { i += p + 1; } else { i += 1; }
                    continue;
                }

                if tag.is_block() || tag.is_heading() {
                    flush_line(&mut lines, &mut col);
                    if !lines.last().map_or(true, |l| l.is_empty()) {
                        lines.push(StyledLine::default());
                        col = 0;
                    }
                }

                // <li>: add bullet prefix
                if tag == Tag::Li {
                    let style = SpanStyle {
                        indent: compute_indent(&tag_stack),
                        ..style_from_stack(&tag_stack)
                    };
                    let indent_chars = (style.indent as usize) * 2;
                    let mut bullet = String::new();
                    for _ in 0..indent_chars { bullet.push(' '); }
                    bullet.push_str("- ");
                    col += indent_chars + 2;
                    lines.last_mut().unwrap().spans.push(Span { text: bullet, style, href: None });
                }

                // Extract href for <a> tags
                let href = if tag == Tag::A {
                    tag_content.and_then(|c| extract_attr_value(c, b"href"))
                } else {
                    None
                };

                tag_stack.push((tag, href));
            }

            if let Some(p) = gt_pos { i += p + 1; } else { i += 1; }
            prev_space = true;
            continue;
        }

        // ── Entity ───────────────────────────────────────────────────────
        if b == b'&' {
            let (ch, advance) = decode_entity(&bytes[i..]);
            if advance > 1 {
                if let Some(c) = ch {
                    if c == ' ' {
                        if !prev_space {
                            emit_char(&mut lines, &mut col, ' ', &tag_stack, cols);
                            prev_space = true;
                        }
                    } else {
                        emit_char(&mut lines, &mut col, c, &tag_stack, cols);
                        prev_space = false;
                    }
                }
                i += advance;
                continue;
            }
            emit_char(&mut lines, &mut col, '&', &tag_stack, cols);
            prev_space = false;
            i += 1;
            continue;
        }

        // ── Whitespace ───────────────────────────────────────────────────
        if b == b' ' || b == b'\n' || b == b'\r' || b == b'\t' {
            if !prev_space {
                emit_char(&mut lines, &mut col, ' ', &tag_stack, cols);
                prev_space = true;
            }
            i += 1;
            continue;
        }

        // ── Regular text ─────────────────────────────────────────────────
        if b >= 0x20 && b < 0x80 {
            emit_char(&mut lines, &mut col, b as char, &tag_stack, cols);
            prev_space = false;
        }
        i += 1;
    }

    // Remove trailing empty lines
    while lines.last().map_or(false, |l| l.is_empty()) {
        lines.pop();
    }

    // Collapse runs of consecutive blank lines into a single blank line
    let mut collapsed = Vec::with_capacity(lines.len());
    let mut prev_blank = false;
    for line in lines {
        if line.is_empty() {
            if !prev_blank {
                collapsed.push(line);
            }
            prev_blank = true;
        } else {
            prev_blank = false;
            collapsed.push(line);
        }
    }

    collapsed
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn emit_char(
    lines: &mut Vec<StyledLine>,
    col: &mut usize,
    ch: char,
    tag_stack: &[(Tag, Option<String>)],
    cols: usize,
) {
    if *col >= cols && ch == ' ' {
        flush_line(lines, col);
        return;
    }

    let style = style_from_stack(tag_stack);
    let href = href_from_stack(tag_stack);
    let line = lines.last_mut().unwrap();

    if let Some(last) = line.spans.last_mut() {
        if spans_style_eq(&last.style, &style) && last.href == href {
            last.text.push(ch);
            *col += 1;
            return;
        }
    }

    let mut text = String::new();
    text.push(ch);
    line.spans.push(Span { text, style, href });
    *col += 1;
}

fn flush_line(lines: &mut Vec<StyledLine>, col: &mut usize) {
    if let Some(line) = lines.last_mut() {
        if let Some(last) = line.spans.last_mut() {
            let trimmed = last.text.trim_end();
            if trimmed.len() != last.text.len() {
                last.text = String::from(trimmed);
            }
        }
    }
    lines.push(StyledLine::default());
    *col = 0;
}

fn style_from_stack(stack: &[(Tag, Option<String>)]) -> SpanStyle {
    let mut style = SpanStyle::default();
    for (tag, _) in stack {
        match tag {
            Tag::H1 | Tag::H2 | Tag::H3 | Tag::H4 | Tag::H5 | Tag::H6 => {
                style.heading = true;
                style.bold = true;
            }
            Tag::B | Tag::Strong => style.bold = true,
            Tag::Em | Tag::I => style.emphasis = true,
            Tag::A => style.link = true,
            Tag::Code | Tag::Pre => style.code = true,
            Tag::Ul | Tag::Ol | Tag::Blockquote => style.indent += 1,
            _ => {}
        }
    }
    style
}

fn href_from_stack(stack: &[(Tag, Option<String>)]) -> Option<String> {
    for (tag, href) in stack.iter().rev() {
        if *tag == Tag::A {
            return href.clone();
        }
    }
    None
}

fn compute_indent(stack: &[(Tag, Option<String>)]) -> u8 {
    let mut indent: u8 = 0;
    for (tag, _) in stack {
        if matches!(tag, Tag::Ul | Tag::Ol | Tag::Blockquote) {
            indent = indent.saturating_add(1);
        }
    }
    indent
}

fn spans_style_eq(a: &SpanStyle, b: &SpanStyle) -> bool {
    a.bold == b.bold
        && a.heading == b.heading
        && a.link == b.link
        && a.emphasis == b.emphasis
        && a.code == b.code
        && a.indent == b.indent
}

fn parse_tag_name(bytes: &[u8]) -> Vec<u8> {
    let mut name = Vec::new();
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b == b'-' {
            name.push(b);
        } else {
            break;
        }
    }
    name
}

fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

fn find_tag_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() { return Some(0); }
    haystack.windows(needle.len()).position(|w| {
        w.iter().zip(needle).all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
    })
}

fn find_substr(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() { return Some(0); }
    haystack.windows(needle.len()).position(|w| w == needle)
}
