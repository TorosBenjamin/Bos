/// Standalone test tool for debugging HTTP fetches and HTML parsing.
///
/// Usage:
///   cargo run -p tls_test -- <url>
///   cargo run -p tls_test -- https://search.marginalia.nu/search?query=operating+system
///
/// Saves:
///   /tmp/boser_raw.html   — raw HTTP response body
///   /tmp/boser_parsed.txt — parsed output (what boser would render)

use std::fs;
use html_renderer::ContentBlock;

fn main() {
    let url = match std::env::args().nth(1) {
        Some(u) => u,
        None => {
            eprintln!("Usage: tls_test <url>");
            eprintln!("  e.g. tls_test https://en.wikipedia.org/wiki/Operating_system");
            std::process::exit(1);
        }
    };

    eprintln!("Fetching: {url}");

    let resp = match http_client::http_get(&url) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("HTTP error: {e:?}");
            std::process::exit(1);
        }
    };

    eprintln!("HTTP {} — {} bytes", resp.status, resp.body.len());

    // Save raw HTML
    fs::write("/tmp/boser_raw.html", &resp.body).unwrap();
    eprintln!("Saved raw HTML to /tmp/boser_raw.html");

    // Parse with html_renderer
    let body_text = std::str::from_utf8(&resp.body).unwrap_or("(binary body)");
    let blocks = html_renderer::parse_html(body_text, 100);

    // Build text output
    let mut output = String::new();
    let mut line_count = 0;
    for block in &blocks {
        match block {
            ContentBlock::Text(line) => {
                if line.spans.is_empty() {
                    output.push('\n');
                    line_count += 1;
                    continue;
                }
                for span in &line.spans {
                    let mut tags = Vec::new();
                    if span.style.heading { tags.push("H"); }
                    if span.style.bold { tags.push("B"); }
                    if span.style.link { tags.push("A"); }
                    if span.style.emphasis { tags.push("I"); }
                    if span.style.code { tags.push("C"); }
                    if span.style.indent > 0 {
                        tags.push(">");
                    }

                    if tags.is_empty() {
                        output.push_str(&span.text);
                    } else {
                        output.push('[');
                        output.push_str(&tags.join(","));
                        if let Some(href) = &span.href {
                            output.push_str(" href=");
                            output.push_str(href);
                        }
                        output.push(':');
                        output.push_str(&span.text);
                        output.push(']');
                    }
                }
                output.push('\n');
                line_count += 1;
            }
            ContentBlock::Image { url } => {
                output.push_str(&format!("[IMG: {url}]\n"));
                line_count += 1;
            }
        }
    }

    fs::write("/tmp/boser_parsed.txt", &output).unwrap();
    eprintln!("Saved parsed output to /tmp/boser_parsed.txt ({line_count} lines, {} blocks)", blocks.len());

    // Print first 60 lines to stdout
    for line in output.lines().take(60) {
        println!("{line}");
    }
    if line_count > 60 {
        println!("... ({} more lines)", line_count - 60);
    }
}
