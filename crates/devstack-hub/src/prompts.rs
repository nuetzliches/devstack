//! The prompts the dashboard hands to a coding agent (ADR-0046, amendment
//! 2026-10-22): the page names one and its target, the hub writes it, from
//! these templates and what it reads itself. Copied by the user, or given to
//! an agent it starts (`POST /hub/agent`).

use crate::open::display;
use std::path::Path;

const REPO_URL: &str = "https://github.com/nuetzliches/devstack";
/// What is quoted from a load error or `up`'s output, at most: every prompt
/// stays below the 5,000 characters Claude Code takes in a link's `q`.
const QUOTE: usize = 2000;

pub enum Prompt<'a> {
    /// Set a repository up: a stack file, from a draft (`devstack init`).
    Setup { repo: &'a Path, drafted: bool },
    /// A stack file that does not load.
    FixFile {
        repo: &'a Path,
        file: &'a Path,
        error: &'a str,
    },
    /// A stack file last brought up to date with an older devstack
    /// (`[stack] reviewed`, or the agent skills' version without it).
    Upgrade {
        repo: &'a Path,
        file: &'a Path,
        from: Option<&'a str>,
        /// `from` is `[stack] reviewed`, not the skills' guess.
        reviewed: bool,
        current: &'a str,
    },
    /// A lane's worktree without a stack file of its own, which runs with
    /// the main working tree's (ADR-0055 point 5).
    LaneFile {
        tree: &'a Path,
        branch: Option<&'a str>,
        main_file: &'a Path,
        file: &'a Path,
    },
    /// The last `devstack up` the hub ran failed.
    StartFailed {
        name: &'a str,
        repo: &'a Path,
        file: &'a Path,
        output: &'a str,
        log: &'a Path,
    },
}

pub fn text(p: &Prompt) -> String {
    let v = devstack_config::VERSION;
    let at = format!("{REPO_URL}/blob/v{v}");
    let about = "devstack (https://github.com/nuetzliches/devstack) runs a local development stack from a devstack.toml in the repository root: containers from the Compose files, native services built in the background, a change goes live once it is healthy.";
    let careful = "Ask me before running anything that deletes data, and before committing.";
    match *p {
        Prompt::Setup { repo, drafted } => format!(
            "Set up devstack for the repository at {repo}.

{about}

1. Read the stack file reference: {at}/docs/stack-file.md
2. {draft} Work through every TODO in it: which containers run in development (infrastructure only; what runs natively stays out), build/output/run/watch of each service, ports (the process listens on {{port}}), a readiness health endpoint, the environment (from a Compose service with `compose = {{...}}`), background workers (`workers`), and commands such as seed or reset (with `confirm` when they delete data).
3. Validate with `devstack check` (and `devstack check --compose`), then `devstack doctor`.
4. Run `devstack skills install`, so later sessions have the devstack skills.
5. Start with `devstack up` and fix what fails. {careful}",
            repo = display(repo),
            draft = if drafted {
                "A draft devstack.toml exists (from `devstack init`)."
            } else {
                "Run `devstack init` in the repository for a draft (it detects Compose files, .NET, Node, Go, Python and Spring Boot apps)."
            },
        ),
        Prompt::FixFile { repo, file, error } => format!(
            "The devstack stack file {file} (repository {repo}) does not load. devstack {v} says:

{error}

{about}

1. Read the stack file reference: {at}/docs/stack-file.md
2. Fix the file so that `devstack check` passes (and `devstack check --compose`). Change what the error names; keep what the file means. A key this devstack does not know may come from a newer one (`[stack] requires`): then say so rather than remove it.
3. Run `devstack doctor`, and `devstack skills install` so the agent skills match the file.
4. {careful}",
            file = display(file),
            repo = display(repo),
            error = quote(error),
        ),
        Prompt::Upgrade {
            repo,
            file,
            from,
            reviewed,
            current,
        } => {
            let since = match from {
                Some(f) if reviewed => format!(
                    "Its stack file was last brought up to date with devstack {f} (`reviewed` in `[stack]`)."
                ),
                Some(f) => format!(
                    "It was set up with devstack {f} (the version its generated agent skills name; the stack file does not say)."
                ),
                None => "It was set up with an earlier devstack.".to_string(),
            };
            let changes = match from {
                Some(f) => format!("the sections after {f}, up to {current}, in {at}/CHANGELOG.md"),
                None => format!("the newest sections of {at}/CHANGELOG.md"),
            };
            // A file without the key gets it now: an older devstack fails on it.
            let requires = if reviewed {
                String::new()
            } else {
                format!(
                    " A devstack before {v} does not know the key: raise `requires` to at least \"{v}\" too.",
                    v = devstack_config::REVIEWED_SINCE
                )
            };
            format!(
                "Bring the devstack setup of the repository at {repo} up to devstack {current}. {since}

{about}

1. Read what changed since then: {changes}. Note what concerns this repository: keys that changed or went away, new defaults, new features.
2. Hold {file} against the stack file reference of {current}: {at}/docs/stack-file.md
3. Run `devstack check` (and `devstack check --compose`) and `devstack doctor`, and fix every error and warning.
4. Tell me which new features would help this stack, and add only those I agree to. Raise `[stack] requires` only when the file uses something an older devstack does not know: it shuts out everyone still on one.
5. Set `reviewed = \"{current}\"` in `[stack]`: the stack file is up to date with devstack {current} then, and the dashboard stops offering this.{requires}
6. Run `devstack skills install` to refresh the generated skills. Check the project's own skills, CLAUDE.md and AGENTS.md for anything that contradicts devstack {current}.
7. Restart the stack (`devstack down`, then `devstack up`) and check that every service becomes healthy. {careful}",
                repo = display(repo),
                file = display(file),
            )
        }
        Prompt::LaneFile {
            tree,
            branch,
            main_file,
            file,
        } => format!(
            "The git worktree {tree}{on} has no devstack stack file of its own. Its devstack lane runs with the main working tree's, {main_file}, which may not fit this branch.

{about} Each other worktree of the repository is a lane: the same stack on its own ports.

1. Read the stack file reference: {at}/docs/stack-file.md
2. Copy {main_file} to {file}, unless it is there by now. Hold it against this branch: services, projects, paths, ports, Compose files and commands that differ here or do not exist yet. Change it to fit the branch. Where it does not fit at all, write a draft with `devstack init` in the worktree instead, and work through its TODOs.
3. Validate with `devstack check` (and `devstack check --compose`) in the worktree.
4. Start the lane with `devstack up` in the worktree and fix what fails. Change nothing in the main working tree. {careful}",
            tree = display(tree),
            on = branch.map(|b| format!(" (branch {b})")).unwrap_or_default(),
            main_file = display(main_file),
            file = display(file),
        ),
        Prompt::StartFailed {
            name,
            repo,
            file,
            output,
            log,
        } => format!(
            "The devstack stack {name} (repository {repo}, stack file {file}) does not start. The end of what `devstack up` said:

{output}

The whole output: {log}

{about}

1. Find out why. `devstack doctor` checks the tools, Docker and the ports; `devstack check` the stack file (reference: {at}/docs/stack-file.md). A port taken, Docker not running, a stack file for a newer devstack and a failing build are the usual causes.
2. Fix the cause where it lies in the repository. What lies outside it (a program to install, another process on a port, Docker) you tell me, with what to do, rather than change it.
3. Start again with `devstack up` and check that every service becomes healthy. {careful}",
            repo = display(repo),
            file = display(file),
            output = quote(output),
            log = display(log),
        ),
    }
}

/// `s` as a fenced block; its end when it is long (the end says why).
fn quote(s: &str) -> String {
    let s = s.trim();
    let n = s.chars().count();
    let cut = if n > QUOTE {
        let tail: String = s.chars().skip(n - QUOTE).collect();
        // From a whole line on.
        let tail = tail
            .split_once('\n')
            .map_or(tail.as_str(), |(_, rest)| rest);
        format!("…\n{tail}")
    } else {
        s.to_string()
    };
    let mut fence = String::from("```");
    while cut.contains(fence.as_str()) {
        fence.push('`');
    }
    format!("{fence}\n{cut}\n{fence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Claude Code takes in a link's `q`.
    const MAX: usize = 5000;

    fn all(error: &str) -> Vec<String> {
        let repo = Path::new("/src/shop");
        let file = Path::new("/src/shop/devstack.toml");
        let log = Path::new("/state/up-x.log");
        vec![
            text(&Prompt::Setup {
                repo,
                drafted: false,
            }),
            text(&Prompt::Setup {
                repo,
                drafted: true,
            }),
            text(&Prompt::FixFile { repo, file, error }),
            text(&Prompt::Upgrade {
                repo,
                file,
                from: Some("0.1.0"),
                reviewed: true,
                current: "0.2.0",
            }),
            text(&Prompt::Upgrade {
                repo,
                file,
                from: None,
                reviewed: false,
                current: "0.2.0",
            }),
            text(&Prompt::Upgrade {
                repo,
                file,
                from: Some("0.1.0"),
                reviewed: false,
                current: "0.2.0",
            }),
            text(&Prompt::StartFailed {
                name: "shop",
                repo,
                file,
                output: error,
                log,
            }),
            text(&Prompt::LaneFile {
                tree: Path::new("/src/shop.worktrees/feat-x"),
                branch: Some("feat/x"),
                main_file: file,
                file: Path::new("/src/shop.worktrees/feat-x/devstack.toml"),
            }),
        ]
    }

    #[test]
    fn every_prompt_fits_a_link_even_with_a_long_error() {
        let long = (0..2000)
            .map(|i| format!("line {i}: something went wrong"))
            .collect::<Vec<_>>()
            .join("\n");
        for p in all(&long) {
            assert!(p.chars().count() < MAX, "{} chars", p.chars().count());
        }
    }

    #[test]
    fn a_long_error_keeps_its_end() {
        let long = (0..500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let q = quote(&long);
        assert!(q.contains("line 499") && !q.contains("line 0\n"), "{q}");
        assert!(q.starts_with("```\n…\nline "), "from a whole line on: {q}");
    }

    #[test]
    fn a_quote_cannot_close_its_fence() {
        let q = quote("before\n```\nafter");
        assert!(q.starts_with("````\n") && q.ends_with("\n````"), "{q}");
    }

    #[test]
    fn the_prompts_name_their_versions_and_references() {
        let p = all("boom");
        assert!(p[0].contains("Run `devstack init`"));
        assert!(p[1].contains("A draft devstack.toml exists"));
        assert!(p[2].contains("```\nboom\n```"));
        assert!(p[3].contains("last brought up to date with devstack 0.1.0"));
        assert!(p[3].contains("the sections after 0.1.0, up to 0.2.0"));
        assert!(p[3].contains("Set `reviewed = \"0.2.0\"`"));
        assert!(!p[3].contains("raise `requires`"), "the file has the key");
        assert!(p[4].contains("an earlier devstack"));
        assert!(p[5].contains("its generated agent skills name"));
        let since = format!(
            "raise `requires` to at least \"{}\"",
            devstack_config::REVIEWED_SINCE
        );
        assert!(p[4].contains(&since) && p[5].contains(&since));
        assert!(p[6].contains("up-x.log"));
        assert!(p[7].contains("feat-x (branch feat/x) has no devstack stack file"));
        assert!(p[7].contains("devstack.toml, which may not fit this branch"));
        let reference = format!(
            "{REPO_URL}/blob/v{}/docs/stack-file.md",
            devstack_config::VERSION
        );
        assert!(
            p.iter().all(|t| t.contains(&reference)),
            "every one links the reference"
        );
    }
}
