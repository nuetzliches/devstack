//! `devstack lane`: the repository's other working trees (plan §3.5).

use crate::{LaneAction, client};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, action: LaneAction) -> Result<()> {
    match action {
        LaneAction::List => client::lane_list(file),
        LaneAction::Start {
            name,
            on,
            profile,
            run,
        } => {
            let name = named(file, name, "start", true)?;
            client::lane_start(file, &name, on.as_deref(), profile.as_deref(), run)
        }
        LaneAction::Stop { name } => client::lane_stop(file, &named(file, name, "stop", false)?),
        LaneAction::Reset { name } => client::lane_reset(file, &named(file, name, "reset", false)?),
        LaneAction::Prune {
            older_than,
            dry_run,
        } => client::lane_prune(file, older_than.as_deref(), dry_run),
        LaneAction::Move { name, to } => client::lane_move(file, &name, &to),
        LaneAction::Share { name, with } => client::lane_share(file, &name, &with),
    }
}

/// The lane named, or without a name the lane of the worktree the command
/// runs in, as `devstack up` there finds it (#476). `start` brings the
/// daemon up to ask it; the others need it running anyway.
fn named(file: &Path, name: Option<String>, action: &str, start: bool) -> Result<String> {
    match name {
        Some(n) => Ok(n),
        None => client::lane_here(file, start).map_err(|e| {
            anyhow::anyhow!("{e}; name it: `devstack lane {action} <NAME>` (`devstack lane list`)")
        }),
    }
}
