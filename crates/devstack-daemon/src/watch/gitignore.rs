//! The repository's ignore rules, as git applies them: every `.gitignore` of
//! the working tree (nested ones too, and those above the stack's directory
//! when the stack sits in a subdirectory of the repository), the
//! repository's `info/exclude` and the user's global excludes
//! (`core.excludesFile`).
//!
//! A build runs in a snapshot of the tracked and untracked, non-ignored files
//! (`git ls-files -co --exclude-standard`, ADR-0002), so a file git ignores is
//! no input of any build: a change to one must not make a candidate. These
//! are the same rules, read once and kept in memory: judging a path is string
//! matching, no file system call and no `git` run per event. The watcher reads
//! them again when a `.gitignore`, `info/exclude` or `.gitmodules` changes.
//!
//! What this does not cover: a submodule's own rules. A path in a submodule is
//! judged by the hard-coded list only (neither the submodule's rules nor the
//! outer repository's, which git does not apply there either).

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::Path;
use std::process::Command;

/// Git compares paths without regard to case where the file system does
/// (`core.ignorecase`, the default on Windows and macOS).
const CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

#[derive(Default)]
pub struct Ignores {
    /// Where the repository (the stack's directory) is in its git working
    /// tree: `""` at the top, `"stack/"` in a subdirectory.
    prefix: String,
    /// Each set of rules with the directory it applies to, relative to the
    /// top of the working tree with a trailing `/` (`""` for the top); the
    /// deepest first, as a deeper `.gitignore` overrides a higher one, and
    /// `info/exclude` and the global excludes last.
    rules: Vec<(String, Gitignore)>,
    /// Submodules, relative to the top with a trailing `/`.
    submodules: Vec<String>,
}

impl Ignores {
    /// No rules: nothing is ignored (outside a git repository, as the
    /// snapshot then takes every file too).
    pub fn none() -> Self {
        Self::default()
    }

    /// The rules that apply to the repository at `repo`; none when it is not
    /// in a git working tree or git cannot be run.
    pub fn load(repo: &Path) -> Self {
        let Some(out) = git(
            repo,
            &["rev-parse", "--show-prefix", "--git-path", "info/exclude"],
        ) else {
            return Self::none();
        };
        let mut lines = out.split('\n');
        let prefix = lines
            .next()
            .unwrap_or("")
            .trim_end_matches('\r')
            .to_string();
        let exclude = lines.next().unwrap_or("").trim_end_matches('\r');
        // The top of the working tree: as many levels up as `prefix` is deep.
        let depth = prefix.matches('/').count();
        let Some(top) = repo.ancestors().nth(depth) else {
            return Self::none();
        };

        let mut rules = Vec::new();
        // `.gitignore` files above the stack's directory, up to the top.
        let parts: Vec<&str> = prefix.split_terminator('/').collect();
        for i in 0..depth {
            let dir: String = parts[..i].iter().map(|p| format!("{p}/")).collect();
            let file = repo
                .ancestors()
                .nth(depth - i)
                .map(|d| d.join(".gitignore"));
            if let Some(file) = file.filter(|f| f.is_file())
                && let Some(gi) = read(&file)
            {
                rules.push((dir, gi));
            }
        }
        // Those in it, at any depth, that git itself reads (not those in
        // ignored directories).
        let listed = git(
            repo,
            &[
                "ls-files",
                "-z",
                "-co",
                "--exclude-standard",
                "--",
                ":(glob)**/.gitignore",
            ],
        )
        .unwrap_or_default();
        for rel in listed.split('\0').filter(|p| !p.is_empty()) {
            let dir = rel.strip_suffix(".gitignore").unwrap_or("");
            if let Some(gi) = read(&repo.join(rel)) {
                rules.push((format!("{prefix}{dir}"), gi));
            }
        }
        // Deepest first (stable: the same directory never appears twice).
        rules.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.matches('/').count()));
        if !exclude.is_empty()
            && let Some(gi) = read(&repo.join(exclude))
        {
            rules.push((String::new(), gi));
        }
        let (global, _) = builder().build_global();
        if !global.is_empty() {
            rules.push((String::new(), global));
        }

        let mut submodules = Vec::new();
        let modules = top.join(".gitmodules");
        if modules.is_file() {
            let path = modules.to_string_lossy().into_owned();
            let out = git(
                repo,
                &[
                    "config",
                    "-z",
                    "--file",
                    &path,
                    "--get-regexp",
                    r"^submodule\..*\.path$",
                ],
            )
            .unwrap_or_default();
            for entry in out.split('\0') {
                if let Some((_, p)) = entry.split_once('\n') {
                    submodules.push(format!("{}/", p.trim_end_matches('/')));
                }
            }
        }
        Ignores {
            prefix,
            rules,
            submodules,
        }
    }

    /// Does git ignore `rel` (repository-relative, `/`-separated): the path
    /// itself, or a directory it is in? The path itself is judged as a file:
    /// the event does not say, and asking the file system is what the
    /// watcher must not do (module doc of `watch`). A pattern for
    /// directories only (`dist/`) then still covers everything in `dist`,
    /// though not an event about `dist` itself.
    pub fn ignored(&self, rel: &str) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        let full = format!("{}{rel}", self.prefix);
        if self.submodules.iter().any(|s| starts_with(&full, s)) {
            return false;
        }
        let start = self.prefix.len();
        // Git does not look into an ignored directory, so nothing below one
        // can be taken back by a `!pattern`.
        full[start..]
            .match_indices('/')
            .any(|(i, _)| self.decide(&full[..start + i], true))
            || self.decide(&full, false)
    }

    /// The deepest rule that matches `path` (relative to the top) decides.
    fn decide(&self, path: &str, is_dir: bool) -> bool {
        for (dir, gi) in &self.rules {
            if path.len() <= dir.len() || !starts_with(path, dir) {
                continue;
            }
            match gi.matched(&path[dir.len()..], is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }
}

/// Is `rel` (repository-relative) a file whose change changes the rules?
pub fn is_rule_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let eq = |a: &str, b: &str| {
        if CASE_INSENSITIVE {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    eq(name, ".gitignore") || eq(name, ".gitmodules") || eq(rel, ".git/info/exclude")
}

fn starts_with(path: &str, prefix: &str) -> bool {
    path.len() >= prefix.len()
        && path.is_char_boundary(prefix.len())
        && if CASE_INSENSITIVE {
            path[..prefix.len()].eq_ignore_ascii_case(prefix)
        } else {
            path.starts_with(prefix)
        }
}

/// Matchers are given paths relative to their own directory: rooted at
/// `.`, the crate strips nothing from them.
fn builder() -> GitignoreBuilder {
    let mut b = GitignoreBuilder::new(".");
    b.case_insensitive(CASE_INSENSITIVE)
        .expect("case insensitivity is supported");
    b
}

/// The rules of one file; a line that does not parse is skipped, as git
/// skips it.
fn read(file: &Path) -> Option<Gitignore> {
    let mut b = builder();
    let _ = b.add(file);
    b.build().ok().filter(|gi| !gi.is_empty())
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn outside_git_nothing_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        write(&d.path().join(".gitignore"), "*.log\n");
        // A temporary directory may be below a repository of the machine's;
        // only the rules' absence matters when git says there is none.
        let ig = Ignores::load(d.path());
        if git(d.path(), &["rev-parse", "--show-prefix"]).is_none() {
            assert!(!ig.ignored("a.log"));
        }
        assert!(!Ignores::none().ignored("a.log"));
    }

    #[test]
    fn the_rules_of_the_repository_apply() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        run(r, &["init", "-q"]);
        write(
            &r.join(".gitignore"),
            "dist/\n*.log\n!keep.log\nappsettings.Development.json\n",
        );
        write(&r.join("web/.gitignore"), "/.angular/\n!*.important.log\n");
        write(&r.join("web/deep/.gitignore"), "generated.ts\n");
        write(&r.join(".git/info/exclude"), "scratch/\n");
        // Inside an ignored directory: not read, as git does not read it.
        write(&r.join("dist/.gitignore"), "!*\n");
        let ig = Ignores::load(r);
        for p in [
            "dist/main.js",
            "web/dist/main.js",
            "a.log",
            "web/src/b.log",
            "web/.angular/cache/20/app/vite/deps/package.json",
            "web/.angular/cache/20/app/vite/deps_temp_1a2b/package.json",
            "web/deep/generated.ts",
            "scratch/notes.md",
            "src/Api/appsettings.Development.json",
        ] {
            assert!(ig.ignored(p), "{p}");
        }
        for p in [
            "src/Api/Program.cs",
            "keep.log",
            "web/x.important.log",
            "web/src/app.ts",
            "web/.gitignore",
            "generated.ts", // only below `web/deep`
            ".angular/x",   // `/.angular/` is anchored to `web`
            "dist",         // the directory itself, judged as a file
        ] {
            assert!(!ig.ignored(p), "{p}");
        }
        if CASE_INSENSITIVE {
            assert!(ig.ignored("DIST/Main.js"));
            assert!(ig.ignored("Web/.Angular/x"));
        }
    }

    #[test]
    fn a_stack_in_a_subdirectory_sees_the_rules_above_it() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        run(r, &["init", "-q"]);
        write(&r.join(".gitignore"), "*.tmp\nstack/out/\n");
        write(&r.join("stack/.gitignore"), "cache/\n");
        write(&r.join("other/.gitignore"), "*.cs\n");
        let ig = Ignores::load(&r.join("stack"));
        assert_eq!(ig.prefix, "stack/");
        for p in ["a.tmp", "src/b.tmp", "out/x.js", "cache/y"] {
            assert!(ig.ignored(p), "{p}");
        }
        for p in ["src/a.cs", "src/out/x.js"] {
            assert!(!ig.ignored(p), "{p}");
        }
    }

    #[test]
    fn a_submodule_is_judged_by_neither_repository_rules() {
        let mut b = builder();
        b.add_line(None, "*.json").unwrap();
        let ig = Ignores {
            prefix: String::new(),
            rules: vec![(String::new(), b.build().unwrap())],
            submodules: vec!["libs/shared/".into()],
        };
        assert!(ig.ignored("src/a.json"));
        assert!(!ig.ignored("libs/shared/a.json"));
    }

    #[test]
    fn rule_files() {
        for p in [
            ".gitignore",
            "web/.gitignore",
            ".git/info/exclude",
            ".gitmodules",
        ] {
            assert!(is_rule_file(p), "{p}");
        }
        for p in ["src/gitignore", "web/.gitignore.bak", "info/exclude"] {
            assert!(!is_rule_file(p), "{p}");
        }
    }
}
