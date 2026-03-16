/// Smoke test for bos_image decoding and html_renderer img parsing.
///
/// Usage:
///   cargo run -p image_test -- <image_file>      — test image decoding
///   cargo run -p image_test -- --html <html_file> — test HTML img parsing

fn main() {
    let arg1 = std::env::args().nth(1).unwrap_or_default();

    if arg1 == "--html" {
        let path = std::env::args().nth(2).expect("Usage: image_test --html <file>");
        let html = std::fs::read_to_string(&path).expect("failed to read file");
        let blocks = html_renderer::parse_html(&html, 100);
        for (i, block) in blocks.iter().enumerate() {
            match block {
                html_renderer::ContentBlock::Text(line) => {
                    let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
                    if !text.is_empty() {
                        println!("[{i}] Text: {text:?}");
                    }
                }
                html_renderer::ContentBlock::Image { url } => {
                    println!("[{i}] IMG: {url}");
                }
            }
        }
        return;
    }

    if arg1.is_empty() {
        eprintln!("Usage: image_test <image_file>");
        eprintln!("       image_test --html <html_file>");
        std::process::exit(1);
    }

    let data = std::fs::read(&arg1).expect("failed to read file");
    eprintln!("Read {} bytes from {arg1}", data.len());

    match bos_image::decode(&data) {
        Ok(img) => {
            eprintln!(
                "Decoded: {}x{}, {} bytes of RGBA pixels",
                img.width, img.height, img.pixels.len()
            );
            assert_eq!(
                img.pixels.len(),
                (img.width * img.height * 4) as usize,
                "pixel buffer size mismatch"
            );
            eprintln!("OK");
        }
        Err(e) => {
            eprintln!("Decode error: {e:?}");
            std::process::exit(1);
        }
    }
}
