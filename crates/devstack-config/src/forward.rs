//! `forward` of a `kind = "static"` service (ADR-0110): which requests go to
//! another server, and where. The check and the static server share these
//! rules, so a file `devstack check` passes is one the server takes.

/// A prefix such as `/api` or `/api/v2`: a path of plain segments, without a
/// trailing `/` (it matches at a segment boundary anyway).
pub fn prefix(p: &str) -> Result<(), String> {
    let Some(rest) = p.strip_prefix('/') else {
        return Err("a path prefix starts with `/`, e.g. `/api`".into());
    };
    if rest.is_empty() {
        return Err(
            "`/` would forward every request, the bundle's files too: name a prefix below it"
                .into(),
        );
    }
    for seg in rest.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(
                "plain path segments only, without `//`, `.`, `..` or a trailing `/`".into(),
            );
        }
        if let Some(c) = seg
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || "-._~".contains(*c)))
        {
            return Err(format!("`{c}`: a prefix takes letters, digits and `-._~`"));
        }
    }
    Ok(())
}

/// Whether request path `path` lies below `prefix`: the prefix itself, or
/// the prefix followed by `/` (`/api` takes `/api/x`, not `/apix`).
pub fn matches(prefix: &str, path: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The forward whose prefix takes `path`: the longest one.
pub fn pick<'a, T>(forwards: &'a [(String, T)], path: &str) -> Option<&'a (String, T)> {
    forwards
        .iter()
        .filter(|(p, _)| matches(p, path))
        .max_by_key(|(p, _)| p.len())
}

/// A target: an origin `http://host[:port]` (one trailing `/` allowed),
/// as host and port.
pub fn origin(t: &str) -> Result<(String, u16), String> {
    if t.starts_with("https://") {
        return Err(
            "devstack forwards to plain HTTP only: give the target's `http://` port (a service with `health.insecure_tls` speaks HTTPS)".into(),
        );
    }
    let Some(rest) = t.strip_prefix("http://") else {
        return Err("a target is an origin, `http://host:port`, e.g. `{service.api.url}`".into());
    };
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() {
        return Err("names no host: `http://host:port`".into());
    }
    if authority.contains(['/', '?', '#', '@']) {
        return Err(
            "a target is an origin without a path: the request's path goes on unchanged".into(),
        );
    }
    let (host, port) = match authority.strip_prefix('[') {
        // An IPv6 address, `[::1]:8080`.
        Some(v6) => {
            let (h, after) = v6
                .split_once(']')
                .ok_or_else(|| "an unclosed `[` in the host".to_string())?;
            (h, after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        },
    };
    if host.is_empty() {
        return Err("names no host: `http://host:port`".into());
    }
    let port = match port {
        None => 80,
        Some(p) => p
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| format!("`{p}` is not a port"))?,
    };
    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        for ok in ["/api", "/api/v2", "/file", "/otel", "/a-b_c.d~e"] {
            assert_eq!(prefix(ok), Ok(()), "{ok}");
        }
        for bad in [
            "api", "/", "/api/", "//api", "/a/../b", "/a b", "/a?x", "/a%20",
        ] {
            assert!(prefix(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_longest_prefix_wins_at_a_segment_boundary() {
        let f = vec![
            ("/api".to_string(), 1),
            ("/api/files".to_string(), 2),
            ("/file".to_string(), 3),
        ];
        let to = |p: &str| pick(&f, p).map(|(_, n)| *n);
        assert_eq!(to("/api"), Some(1));
        assert_eq!(to("/api/users/7"), Some(1));
        assert_eq!(to("/api/files"), Some(2));
        assert_eq!(to("/api/files/a.pdf"), Some(2));
        assert_eq!(to("/api/filesystem"), Some(1));
        assert_eq!(to("/apix"), None, "not at a segment boundary");
        assert_eq!(to("/files"), None);
        assert_eq!(to("/"), None);
        assert_eq!(to("/main.js"), None);
    }

    #[test]
    fn origins() {
        assert_eq!(
            origin("http://127.0.0.1:5000"),
            Ok(("127.0.0.1".into(), 5000))
        );
        assert_eq!(
            origin("http://127.0.0.1:5000/"),
            Ok(("127.0.0.1".into(), 5000))
        );
        assert_eq!(origin("http://localhost"), Ok(("localhost".into(), 80)));
        assert_eq!(origin("http://[::1]:8080"), Ok(("::1".into(), 8080)));
        for bad in [
            "https://127.0.0.1:5001",
            "127.0.0.1:5000",
            "http://127.0.0.1:5000/api",
            "http://127.0.0.1:x",
            "http://:5000",
            "http://",
            "http://user@host:1",
        ] {
            assert!(origin(bad).is_err(), "{bad}");
        }
    }
}
