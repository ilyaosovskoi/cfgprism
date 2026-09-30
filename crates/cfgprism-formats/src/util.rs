//! Shared helpers for line-based formats (dotenv, INI, Properties).

/// One logical line: content without terminator + verbatim terminator.
pub struct RawLine<'a> {
    /// Line text without the line ending.
    pub content: &'a str,
    /// Verbatim line ending: `"\n"`, `"\r\n"` or `""` (last line).
    pub ending: &'a str,
}

/// Split `src` into lines, keeping each line ending verbatim.
/// The returned slices borrow `src`; `"\n"`/`"\r\n"` endings are subslices
/// of it, so no allocation happens here.
pub fn split_lines(src: &str) -> Vec<RawLine<'_>> {
    let mut out = Vec::new();
    let mut rest = src;
    while !rest.is_empty() {
        match rest.find('\n') {
            Some(i) => {
                let raw = &rest[..i]; // may end with '\r'
                let content = raw.strip_suffix('\r').unwrap_or(raw);
                let ending = &rest[content.len()..i + 1]; // "\n" or "\r\n"
                out.push(RawLine { content, ending });
                rest = &rest[i + 1..];
            }
            None => {
                out.push(RawLine {
                    content: rest,
                    ending: "",
                });
                break;
            }
        }
    }
    out
}

/// Byte offset of the start of each line in `src`.
pub fn line_starts(src: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut o = 0;
    for l in split_lines(src) {
        out.push(o);
        o += l.content.len() + l.ending.len();
    }
    out
}
