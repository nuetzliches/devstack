//! `devstack skills`: the agent skills generated from the stack file.

use crate::{SkillsAction, load, skills};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, action: SkillsAction) -> Result<()> {
    match action {
        SkillsAction::Install { dir, force } => {
            let stack = load(file);
            for o in skills::install(&stack, file, dir.as_deref(), force)? {
                match o {
                    skills::Outcome::Written(p) => println!("written    {}", p.display()),
                    skills::Outcome::Unchanged(p) => println!("up to date {}", p.display()),
                    skills::Outcome::Kept(p) => println!(
                        "kept       {} (written by the project, not by devstack; --force replaces it)",
                        p.display()
                    ),
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
                None => println!("skills: up to date (or not installed in the default directory)"),
            }
            Ok(())
        }
    }
}
