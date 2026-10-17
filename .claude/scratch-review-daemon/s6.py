import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'logfiles.rs',[
("""/// The line that separates one log from the next in a combined view.""",
"""/// The last `n` lines of a file, oldest first; with `grep` only those that
/// contain it (without case, and without the colour codes around the text).
/// Read backwards from the end in chunks: a long log is never read whole.
/// Output that is not UTF-8 (an OEM code page) is shown lossily.
pub fn last_lines(path: &Path, n: usize, grep: Option<&str>) -> Vec<String> {
    last_lines_at(path, n, grep).0
}

/// [`last_lines`], and the length of the file as it was read (where to
/// follow it from).
pub fn last_lines_at(path: &Path, n: usize, grep: Option<&str>) -> (Vec<String>, u64) {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 64 * 1024;
    let Ok(mut f) = std::fs::File::open(path) else {
        return (Vec::new(), 0);
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let needle = grep.map(str::to_lowercase);
    let mut out: Vec<String> = Vec::new(); // newest first
    let mut push = |seg: &[u8], out: &mut Vec<String>| {
        let text = String::from_utf8_lossy(seg);
        let line = text.strip_suffix('\\r').unwrap_or(&text);
        let hit = needle
            .as_ref()
            .is_none_or(|g| crate::ansi::strip(line).to_lowercase().contains(g));
        if hit && out.len() < n {
            out.push(line.to_string());
        }
    };
    let mut pos = len;
    // The bytes after `pos` that belong to a line whose start is not read yet.
    let mut rest: Vec<u8> = Vec::new();
    // The empty "line" after a final newline is none.
    let mut at_end = true;
    while pos > 0 && out.len() < n {
        let start = pos.saturating_sub(CHUNK);
        let mut buf = vec![0u8; (pos - start) as usize];
        if f.seek(SeekFrom::Start(start)).is_err() || f.read_exact(&mut buf).is_err() {
            break;
        }
        buf.extend_from_slice(&rest);
        pos = start;
        let mut segs = buf.split(|b| *b == b'\\n');
        let head = segs.next().unwrap_or_default().to_vec();
        for seg in segs.rev() {
            if std::mem::take(&mut at_end) && seg.is_empty() {
                continue;
            }
            push(seg, &mut out);
        }
        rest = head;
    }
    if pos == 0 && !(at_end && rest.is_empty()) {
        push(&rest, &mut out);
    }
    out.reverse();
    (out, len)
}

/// The line that separates one log from the next in a combined view."""),
("""    #[test]
    fn names_round_trip() {""",
"""    #[test]
    fn last_lines_are_read_from_the_end() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("run.log");
        let mut text = String::new();
        for i in 0..20_000 {
            text.push_str(&format!("line {i}\\r\\n"));
        }
        std::fs::write(&p, &text).unwrap();
        assert_eq!(last_lines(&p, 2, None), ["line 19998", "line 19999"]);
        assert_eq!(last_lines(&p, 3, Some("LINE 1234")).len(), 3);
        assert_eq!(last_lines(&p, 1, Some("line 5\\u{0}")), Vec::<String>::new());
        assert_eq!(last_lines(&p, 100_000, None).len(), 20_000);
        assert_eq!(last_lines(&p, 1, Some("line 0")), ["line 0"]);
        // Without a final newline, and with bytes that are not UTF-8.
        std::fs::write(&p, b"a\\nb\\xff\\nc").unwrap();
        assert_eq!(last_lines(&p, 5, None), ["a", "b\\u{fffd}", "c"]);
        std::fs::write(&p, b"").unwrap();
        assert!(last_lines(&p, 5, None).is_empty());
        assert!(last_lines(&d.path().join("none"), 5, None).is_empty());
    }

    #[test]
    fn names_round_trip() {"""),
])
apply(W+'supervisor.rs',[
("""fn build_errors(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();""",
"""fn build_errors(path: &Path) -> String {
    let text = std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();"""),
])
print("ok")
