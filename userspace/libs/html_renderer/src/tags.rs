//! HTML tag classification.

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Tag {
    H1, H2, H3, H4, H5, H6,
    B, Strong,
    Em, I,
    A,
    Code, Pre,
    Ul, Ol, Blockquote,
    Li,
    P, Div, Span, Section, Article, Main, Header, Footer,
    Table, Tr, Td, Th, Tbody, Thead,
    Form, Head, Script, Style, Nav, Noscript, Svg, Math,
    Other,
}

impl Tag {
    pub fn from_name(name: &[u8]) -> Self {
        let mut buf = [0u8; 16];
        let len = name.len().min(16);
        for j in 0..len {
            buf[j] = name[j].to_ascii_lowercase();
        }
        let s = &buf[..len];
        match s {
            b"h1" => Tag::H1, b"h2" => Tag::H2, b"h3" => Tag::H3,
            b"h4" => Tag::H4, b"h5" => Tag::H5, b"h6" => Tag::H6,
            b"b" => Tag::B, b"strong" => Tag::Strong,
            b"em" => Tag::Em, b"i" => Tag::I,
            b"a" => Tag::A,
            b"code" => Tag::Code, b"pre" => Tag::Pre,
            b"ul" => Tag::Ul, b"ol" => Tag::Ol, b"blockquote" => Tag::Blockquote,
            b"li" => Tag::Li,
            b"p" => Tag::P, b"div" => Tag::Div,
            b"span" => Tag::Span,
            b"section" => Tag::Section, b"article" => Tag::Article,
            b"main" => Tag::Main,
            b"header" => Tag::Header, b"footer" => Tag::Footer,
            b"table" => Tag::Table, b"tr" => Tag::Tr, b"td" => Tag::Td,
            b"th" => Tag::Th, b"tbody" => Tag::Tbody, b"thead" => Tag::Thead,
            b"form" => Tag::Form,
            b"head" => Tag::Head,
            b"script" => Tag::Script, b"style" => Tag::Style,
            b"nav" => Tag::Nav, b"noscript" => Tag::Noscript,
            b"svg" => Tag::Svg, b"math" => Tag::Math,
            _ => Tag::Other,
        }
    }

    pub fn is_block(self) -> bool {
        matches!(self,
            Tag::H1 | Tag::H2 | Tag::H3 | Tag::H4 | Tag::H5 | Tag::H6 |
            Tag::P | Tag::Div | Tag::Li | Tag::Ul | Tag::Ol | Tag::Blockquote |
            Tag::Pre | Tag::Section | Tag::Article | Tag::Main |
            Tag::Header | Tag::Footer |
            Tag::Table | Tag::Tr | Tag::Td | Tag::Th
        )
    }

    pub fn is_heading(self) -> bool {
        matches!(self, Tag::H1 | Tag::H2 | Tag::H3 | Tag::H4 | Tag::H5 | Tag::H6)
    }

    /// Tags whose entire content should be skipped (not rendered).
    pub fn is_opaque(self) -> bool {
        matches!(self, Tag::Script | Tag::Style | Tag::Head | Tag::Nav | Tag::Noscript | Tag::Svg | Tag::Math | Tag::Form | Tag::Footer)
    }
}
