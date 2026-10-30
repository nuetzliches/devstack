//! `devstack skills`: the agent skills generated from the stack file.

use crate::{SkillsAction, load, skills};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, action: SkillsAction) -> Result<()> {
    match action {
        SkillsAction::Install { dir, force } => {
            let stack = load(file);
            let mut tracked = Vec::new();
            for o in skills::install(&stack, file, dir.as_deref(), force)? {
                let fresh = matches!(o, skills::Outcome::Written(_));
                match o {
                    skills::Outcome::Written(p) | skills::Outcome::Unchanged(p) => {
                        println!(
                            "{} {}",
                            if fresh { "written   " } else { "up to date" },
                            p.display()
                        );
                        if let Some(folder) = p.parent()
                            && git_tracks(folder)
                        {
                            tracked.push(folder.to_path_buf());
                        }
                    }
                    skills::Outcome::Kept(p) => println!(
                        "kept       {} (written by the project, not by devstack; --force replaces it)",
                        p.display()
                    ),
                }
            }
            // Local to the working tree since ADR-0079: a copy an earlier
            // devstack had committed is the user's to take out of git (a
            // migration aid, #389).
            if !tracked.is_empty() {
                println!(
                    "git still tracks these generated skills; they are local to the working tree now (each ignores itself). Take them out of git, the files stay:"
                );
                for f in tracked {
                    println!("  git rm -r --cached \"{}\"", f.display());
                }
            }
            Ok(())
        }
        SkillsAction::Check => {
            match skills::outdated_note(file) {
                // Outdated is a failure, so a script or CI can check it.
                Some(n) => {
                    println!("{n}");
                    std::process::exit(1);
                }
                None => println!("skills: up to date (or none installed)"),
            }
            Ok(())
        }
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
