//! `devstack skills`: the agent skills generated from the stack file.

use crate::{SkillsAction, load, skills};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, action: SkillsAction) -> Result<()> {
    match action {
        SkillsAction::Install { dir, force } => {
            let stack = load(file);
            for o in skills::install(&stack, file, dir.as_deref(), force)? {
                let fresh = matches!(o, skills::Outcome::Written(_));
                match o {
                    skills::Outcome::Written(p) | skills::Outcome::Unchanged(p) => println!(
                        "{} {}",
                        if fresh { "written   " } else { "up to date" },
                        p.display()
                    ),
                    skills::Outcome::Kept(p) => println!(
                        "kept       {} (written by the project, not by devstack; --force replaces it)",
                        p.display()
                    ),
                }
            }
            Ok(())
        }
        // The hub keeps the generated skills current (ADR-0079); what it
        // leaves is a copy git still tracks (ADR-0087). Such a copy is a
        // failure, so a script or CI can check it.
        SkillsAction::Check => {
            let tracked: Vec<_> = skills::installed(file)
                .folders
                .into_iter()
                .filter(|f| git_tracks(f))
                .collect();
            if tracked.is_empty() {
                println!("skills: none tracked by git");
                return Ok(());
            }
            untrack_note(&tracked);
            std::process::exit(1);
        }
    }
}

/// Local to the working tree since ADR-0079: a copy an earlier devstack had
/// committed is the user's to take out of git (`skills check`, ADR-0087).
fn untrack_note(tracked: &[std::path::PathBuf]) {
    if tracked.is_empty() {
        return;
    }
    println!(
        "git still tracks these generated skills; they are local to the working tree now (each ignores itself) and the hub does not refresh them. Take them out of git, the files stay:"
    );
    for f in tracked {
        println!("  git rm -r --cached \"{}\"", f.display());
    }
}

/// Whether git tracks a file in `folder`.
fn git_tracks(folder: &Path) -> bool {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["ls-files", "--", "."])
        .current_dir(folder)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}
