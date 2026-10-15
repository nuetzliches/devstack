//! Terminal escape sequences in service output. Processes write colours for a
//! terminal (`ESC [ 33 m`); devstack keeps them in the log files as written and
//! strips them where text is compared or shown without a terminal: `grep`
//! matches the visible text, the MCP tools hand agents plain text.

use std::borrow::Cow;

/// `text` without escape sequences: CSI (`ESC [ … final`), OSC (`ESC ] … BEL`
/// or `ESC \`), and any other two-byte `ESC x`.
pub fn strip(text: &str) -> Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then one final byte @..~
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or the string terminator ESC \
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colours_and_other_sequences() {
        let line = "\u{1b}[33m▲ \u{1b}[43;33m[\u{1b}[43;30mWARNING\u{1b}[43;33m]\u{1b}[0m \u{1b}[1mNG8113\u{1b}[0m";
        assert_eq!(strip(line), "▲ [WARNING] NG8113");
        assert_eq!(strip("\u{1b}]0;title\u{7}text\u{1b}[2K"), "text");
        assert_eq!(
            strip("\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert!(matches!(strip("plain"), Cow::Borrowed("plain")));
    }
}
