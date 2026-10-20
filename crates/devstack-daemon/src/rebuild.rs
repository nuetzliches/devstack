//! A dev server's rebuilds, read from its output (ADR-0042): it builds and
//! serves in one process, so devstack learns of a rebuild only from what it
//! prints. Each known tool's lines are matched (ANSI colours removed); the
//! first line one of them matches fixes the tool for the run (`auto`), or
//! the stack file names it, or gives its own lines.

use devstack_config::model::Rebuild as Spec;
use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;

/// How long a finished rebuild stays on the service's view.
pub const SHOWN_MS: u128 = 60_000;
/// Error lines kept of a failed rebuild.
const MAX_ERRORS: usize = 10;

/// What the service's view says of its last rebuild.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RebuildView {
    /// `rebuilding`, `ok`, `failed`.
    pub state: String,
    /// Unix ms: when it started, or when it was over for one that said only
    /// that.
    pub since: u128,
    /// How long it took, the tool's own figure where it prints one, else
    /// from its start line to its end line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    /// The tool recognised (`angular`, `vite`, …, `custom`).
    pub tool: String,
    /// A failed one's error lines, as the dev server printed them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// A tool's lines: a rebuild starts, is done (a group `ms` or `s` its
/// time), failed; and what an error line of it looks like.
struct Rules {
    name: &'static str,
    start: Vec<Regex>,
    ok: Vec<Regex>,
    failed: Vec<Regex>,
    error: Option<Regex>,
}

fn re(p: &str) -> Regex {
    Regex::new(p).expect("a known tool's pattern compiles")
}

fn known() -> &'static [Rules] {
    static K: OnceLock<Vec<Rules>> = OnceLock::new();
    K.get_or_init(|| {
        vec![
            Rules {
                // The application builder (esbuild/Vite) and the webpack one.
                name: "angular",
                start: vec![re(r"Changes detected\. Rebuilding")],
                ok: vec![
                    re(r"Application bundle generation complete\.(?: \[(?P<s>[\d.]+) seconds\])?"),
                    re(r"Browser application bundle generation complete"),
                ],
                failed: vec![
                    re(r"Application bundle generation failed\.(?: \[(?P<s>[\d.]+) seconds\])?"),
                    re(r"Browser application bundle generation failed"),
                ],
                error: Some(re(r"\[ERROR\]|^\s*Error: |error (?:TS|NG)\d+")),
            },
            Rules {
                name: "next",
                start: vec![re(r"[○◌] Compiling ")],
                ok: vec![re(r"✓ Compiled(?: .*?)? in (?:(?P<ms>\d+)ms|(?P<s>[\d.]+)s)")],
                failed: vec![re(r"Failed to compile"), re(r"^\s*⨯ ")],
                error: Some(re(r"⨯|Error:|error")),
            },
            Rules {
                name: "webpack",
                start: vec![re(r"webpack-dev-middleware.*wait until bundle finished"), re(r"^\s*Compiling\.\.\.")],
                ok: vec![re(r"compiled successfully in (?P<ms>\d+) ?ms")],
                failed: vec![
                    re(r"compiled with \d+ errors?(?: and \d+ warnings?)? in (?P<ms>\d+) ?ms"),
                    re(r"Failed to compile"),
                ],
                error: Some(re(r"ERROR in|Module not found|error")),
            },
            Rules {
                // Vite says what it reloaded, not that it rebuilt: a done-only
                // tool. Nuxt, SvelteKit, Astro print Vite's lines.
                name: "vite",
                start: vec![],
                ok: vec![re(r"\[vite\].*(?:hmr update|page reload|hmr invalidate)")],
                failed: vec![re(r"\[vite\].*(?:Internal server error|Pre-transform error|error while updating)")],
                error: Some(re(r"\[vite\].*error|Error:|✘ \[ERROR\]")),
            },
            Rules {
                name: "dotnet-watch",
                start: vec![re(r"dotnet watch .*(?:File changed|Building)")],
                ok: vec![
                    re(r"dotnet watch .*Hot reload (?:of changes )?succeeded"),
                    re(r"dotnet watch .*Build succeeded"),
                    re(r"dotnet watch .*Started"),
                ],
                failed: vec![
                    re(r"dotnet watch .*Build failed"),
                    re(r"dotnet watch .*(?:Unable to apply hot reload|Hot reload (?:of changes )?failed)"),
                ],
                error: Some(re(r"error [A-Z]+\d+")),
            },
            Rules {
                name: "tsc",
                start: vec![re(r"File change detected\. Starting incremental compilation")],
                ok: vec![re(r"Found 0 errors\. Watching for file changes")],
                failed: vec![re(r"Found [1-9]\d* errors?\. Watching for file changes")],
                error: Some(re(r"error TS\d+")),
            },
        ]
    })
}

/// The line without ANSI escape sequences (colours, cursor moves).
fn plain(line: &str) -> String {
    crate::ansi::strip(line).trim_end_matches('\r').to_string()
}

enum Mode {
    /// Not known yet: every known tool is tried.
    Auto,
    Fixed(usize),
    Custom(Rules),
    Off,
}

/// Follows one run of a dev server: fed its lines, says when the state of a
/// rebuild changed.
pub struct Tracker {
    mode: Mode,
    current: Option<RebuildView>,
    /// Error-looking lines since the rebuild started.
    errors: Vec<String>,
    started: Option<u128>,
    /// A failed rebuild's errors are only its marker line so far.
    marker_only: bool,
}

impl Tracker {
    /// `None` when the stack file asks for no detection.
    pub fn new(spec: Option<&Spec>) -> Option<Self> {
        let mode = match spec {
            None => Mode::Auto,
            Some(Spec::Named(n)) if n == "auto" => Mode::Auto,
            Some(Spec::Named(n)) if n == "none" => Mode::Off,
            Some(Spec::Named(n)) => match known().iter().position(|r| r.name == n) {
                Some(i) => Mode::Fixed(i),
                None => Mode::Auto,
            },
            Some(Spec::Lines(l)) => {
                let compile = |p: &str| Regex::new(p).ok();
                Mode::Custom(Rules {
                    name: "custom",
                    start: l.start.as_deref().and_then(compile).into_iter().collect(),
                    ok: compile(&l.ok).into_iter().collect(),
                    failed: compile(&l.failed).into_iter().collect(),
                    error: None,
                })
            }
        };
        (!matches!(mode, Mode::Off)).then(|| Self {
            mode,
            current: None,
            errors: Vec::new(),
            started: None,
            marker_only: false,
        })
    }

    fn rules(&self) -> Option<&Rules> {
        match &self.mode {
            Mode::Fixed(i) => known().get(*i),
            Mode::Custom(r) => Some(r),
            Mode::Auto | Mode::Off => None,
        }
    }

    /// One line of output at `now` (Unix ms); the rebuild's new state when
    /// this line changed it.
    pub fn feed(&mut self, line: &str, now: u128) -> Option<RebuildView> {
        let line = plain(line);
        if line.trim().is_empty() {
            return None;
        }
        if matches!(self.mode, Mode::Auto) {
            let hit = known().iter().position(|r| {
                r.start
                    .iter()
                    .chain(&r.ok)
                    .chain(&r.failed)
                    .any(|re| re.is_match(&line))
            })?;
            self.mode = Mode::Fixed(hit);
        }
        let rules = self.rules()?;
        let name = rules.name;
        if rules.start.iter().any(|r| r.is_match(&line)) {
            // Said twice (Angular does): the first start counts.
            if self.started.is_some() {
                return None;
            }
            self.errors.clear();
            self.started = Some(now);
            return self.set(RebuildView {
                state: "rebuilding".into(),
                since: now,
                ms: None,
                tool: name.into(),
                errors: Vec::new(),
            });
        }
        if let Some(caps) = rules.failed.iter().find_map(|r| r.captures(&line)) {
            let ms = took(&caps).or_else(|| self.elapsed(now));
            // The marker until the error lines follow (Angular prints them
            // after it); replaced by the first of them.
            let mut errors = std::mem::take(&mut self.errors);
            if errors.is_empty() {
                errors.push(line.trim().to_string());
                self.marker_only = true;
            }
            let since = self.started.take().unwrap_or(now);
            return self.set(RebuildView {
                state: "failed".into(),
                since,
                ms,
                tool: name.into(),
                errors,
            });
        }
        if let Some(caps) = rules.ok.iter().find_map(|r| r.captures(&line)) {
            // A rebuild done with errors printed before (Angular prints them
            // above its last line on some failures): failed.
            let ms = took(&caps).or_else(|| self.elapsed(now));
            let since = self.started.take().unwrap_or(now);
            let errors = std::mem::take(&mut self.errors);
            return self.set(RebuildView {
                state: if errors.is_empty() { "ok" } else { "failed" }.into(),
                since,
                ms,
                tool: name.into(),
                errors,
            });
        }
        // An error line: kept for the rebuild under way, or added to one
        // that failed a moment ago (the details follow the marker).
        if rules.error.as_ref().is_some_and(|r| r.is_match(&line)) {
            let text = line.trim().to_string();
            match &mut self.current {
                Some(c) if c.state == "failed" && self.started.is_none() => {
                    if std::mem::take(&mut self.marker_only) {
                        c.errors.clear();
                    }
                    if c.errors.len() < MAX_ERRORS && !c.errors.contains(&text) {
                        c.errors.push(text);
                        return self.current.clone();
                    }
                }
                _ => {
                    if self.errors.len() < MAX_ERRORS {
                        self.errors.push(text);
                    }
                }
            }
        }
        None
    }

    fn elapsed(&self, now: u128) -> Option<u64> {
        self.started.map(|t| now.saturating_sub(t) as u64)
    }

    fn set(&mut self, v: RebuildView) -> Option<RebuildView> {
        self.current = Some(v);
        self.current.clone()
    }

    /// The last rebuild, as the view shows it at `now`: a finished one for
    /// [`SHOWN_MS`], a failed one until the next.
    pub fn view(&self, now: u128) -> Option<RebuildView> {
        self.current.clone().filter(|c| {
            c.state != "ok" || now.saturating_sub(c.since) < SHOWN_MS + c.ms.unwrap_or(0) as u128
        })
    }
}

fn took(caps: &regex::Captures) -> Option<u64> {
    if let Some(ms) = caps.name("ms").and_then(|m| m.as_str().parse::<u64>().ok()) {
        return Some(ms);
    }
    caps.name("s")
        .and_then(|s| s.as_str().parse::<f64>().ok())
        .map(|s| (s * 1000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `text` line by line, a second apart; every state reached.
    fn run(spec: Option<&Spec>, text: &str) -> Vec<RebuildView> {
        let mut t = Tracker::new(spec).expect("detection on");
        text.lines()
            .enumerate()
            .filter_map(|(i, l)| t.feed(l, 1_000_000 + i as u128 * 1000))
            .collect()
    }

    fn states(v: &[RebuildView]) -> Vec<(&str, Option<u64>)> {
        v.iter().map(|r| (r.state.as_str(), r.ms)).collect()
    }

    // Output as the tools print it (Angular 20, Next 15, webpack-dev-server
    // 5, Vite 6, dotnet watch 10, tsc 5).

    #[test]
    fn angular_application_builder() {
        let out = "\
\x1b[2m❯\x1b[22m Changes detected. Rebuilding...
Initial chunk files | Names | Raw size
main.js | main | 1.2 MB |
Application bundle generation complete. [11.934 seconds] - 2026-10-20T09:21:38.757Z

Page reload sent to client(s).
❯ Changes detected. Rebuilding...
✘ [ERROR] TS2322: Type 'string' is not assignable to type 'number'. [plugin angular-compiler]

    src/app/app.component.ts:12:2:
Application bundle generation failed. [0.523 seconds] - 2026-10-20T09:22:01.001Z
";
        let v = run(None, out);
        assert_eq!(
            states(&v),
            [
                ("rebuilding", None),
                ("ok", Some(11934)),
                ("rebuilding", None),
                ("failed", Some(523))
            ]
        );
        assert!(v.iter().all(|r| r.tool == "angular"));
        assert_eq!(
            v[3].errors,
            [
                "✘ [ERROR] TS2322: Type 'string' is not assignable to type 'number'. [plugin angular-compiler]"
            ]
        );
    }

    #[test]
    fn angular_on_windows() {
        // Windows: `X` for `✘`, the errors after the marker, the start
        // line twice.
        let out = "> Changes detected. Rebuilding...
> Changes detected. Rebuilding...
Application bundle generation failed. [9.016 seconds] - 2026-10-20T09:26:54.405Z

X [ERROR] TS7008: Member 'flags' implicitly has an 'any' type. [plugin angular-compiler]

    src/app/x.component.ts:12:2:
X [ERROR] TS2729: Property 'flags' is used before its initialization. [plugin angular-compiler]
";
        let v = run(None, out);
        assert_eq!(states(&v)[0], ("rebuilding", None));
        let last = v.last().unwrap();
        assert_eq!((last.state.as_str(), last.ms), ("failed", Some(9016)));
        assert_eq!(
            last.errors,
            [
                "X [ERROR] TS7008: Member 'flags' implicitly has an 'any' type. [plugin angular-compiler]",
                "X [ERROR] TS2729: Property 'flags' is used before its initialization. [plugin angular-compiler]",
            ]
        );
        assert_eq!(v.iter().filter(|r| r.state == "rebuilding").count(), 1);
    }

    #[test]
    fn angular_warnings_are_no_failure() {
        // A warning (NG8113, a budget) is no error.
        let out = "\
❯ Changes detected. Rebuilding...
▲ [WARNING] NG8113: NeoButtonComponent is not used within the template
▲ [WARNING] bundle initial exceeded maximum budget.
Application bundle generation complete. [2.100 seconds]
";
        assert_eq!(
            states(&run(None, out)),
            [("rebuilding", None), ("ok", Some(2100))]
        );
    }

    #[test]
    fn next() {
        let out = "\
 ○ Compiling /dashboard ...
 ✓ Compiled /dashboard in 1234ms
 ✓ Compiled in 1.5s
 ⨯ ./app/page.tsx:3:1
Module not found: Can't resolve './missing'
";
        let v = run(None, out);
        assert_eq!(
            states(&v),
            [
                ("rebuilding", None),
                ("ok", Some(1234)),
                ("ok", Some(1500)),
                ("failed", None)
            ]
        );
        assert_eq!(v[0].tool, "next");
    }

    #[test]
    fn webpack_dev_server() {
        let out = "\
<i> [webpack-dev-middleware] wait until bundle finished: /
webpack 5.99.0 compiled successfully in 812 ms
<i> [webpack-dev-middleware] wait until bundle finished: /
ERROR in ./src/index.js 3:0-27
Module not found: Error: Can't resolve './missing'
webpack 5.99.0 compiled with 1 error in 120 ms
";
        let v = run(None, out);
        assert_eq!(
            states(&v),
            [
                ("rebuilding", None),
                ("ok", Some(812)),
                ("rebuilding", None),
                ("failed", Some(120))
            ]
        );
        assert_eq!(v[3].errors.len(), 2, "{:?}", v[3].errors);
    }

    #[test]
    fn vite() {
        let out = "\
  VITE v6.2.0  ready in 412 ms
10:21:01 AM [vite] (client) hmr update /src/App.vue
10:21:09 AM [vite] page reload src/main.ts
10:21:20 AM [vite] Internal server error: Transform failed with 1 error:
";
        let v = run(None, out);
        assert_eq!(states(&v), [("ok", None), ("ok", None), ("failed", None)]);
        assert_eq!(v[0].tool, "vite");
    }

    #[test]
    fn dotnet_watch() {
        let out = "\
dotnet watch ⌚ File changed: .\\Program.cs.
dotnet watch 🔥 Hot reload of changes succeeded.
dotnet watch ⌚ File changed: .\\Program.cs.
Program.cs(3,1): error CS1002: ; expected
dotnet watch ❌ Build failed.
";
        let v = run(None, out);
        assert_eq!(
            states(&v),
            [
                ("rebuilding", None),
                ("ok", Some(1000)),
                ("rebuilding", None),
                ("failed", Some(2000))
            ]
        );
        assert_eq!(v[3].errors, ["Program.cs(3,1): error CS1002: ; expected"]);
    }

    #[test]
    fn tsc_watch() {
        let out = "\
[10:21:01 AM] File change detected. Starting incremental compilation...
src/a.ts(1,7): error TS2322: Type 'string' is not assignable to type 'number'.
[10:21:02 AM] Found 1 error. Watching for file changes.
[10:21:10 AM] File change detected. Starting incremental compilation...
[10:21:11 AM] Found 0 errors. Watching for file changes.
";
        let v = run(None, out);
        assert_eq!(
            states(&v),
            [
                ("rebuilding", None),
                ("failed", Some(2000)),
                ("rebuilding", None),
                ("ok", Some(1000))
            ]
        );
    }

    #[test]
    fn a_named_tool_custom_lines_and_none() {
        // Named: an Angular line is not a Vite one.
        let vite = Spec::Named("vite".into());
        assert!(run(Some(&vite), "❯ Changes detected. Rebuilding...\n").is_empty());
        let lines = Spec::Lines(devstack_config::model::RebuildLines {
            start: Some("^rebuild: start".into()),
            ok: r"^rebuild: done in (?P<ms>\d+)".into(),
            failed: "^rebuild: broken".into(),
        });
        let v = run(
            Some(&lines),
            "rebuild: start\nrebuild: done in 42\nrebuild: broken\n",
        );
        assert_eq!(
            states(&v),
            [("rebuilding", None), ("ok", Some(42)), ("failed", None)]
        );
        assert_eq!(v[0].tool, "custom");
        assert!(Tracker::new(Some(&Spec::Named("none".into()))).is_none());
        // Plain output says nothing.
        assert!(run(None, "listening on 4200\nGET / 200\n").is_empty());
    }

    #[test]
    fn a_finished_rebuild_is_shown_for_a_while() {
        let mut t = Tracker::new(None).unwrap();
        t.feed("❯ Changes detected. Rebuilding...", 0);
        t.feed(
            "Application bundle generation complete. [1.000 seconds]",
            1000,
        );
        assert!(t.view(1000).is_some());
        assert!(t.view(1000 + SHOWN_MS + 2000).is_none());
        t.feed("Application bundle generation failed.", 200_000);
        assert!(
            t.view(10_000_000).is_some(),
            "a failure stays until the next"
        );
    }
}
