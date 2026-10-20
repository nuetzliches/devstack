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
        } => client::lane_start(file, &name, on.as_deref(), profile.as_deref(), run),
        LaneAction::Stop { name } => client::lane_stop(file, &name),
        LaneAction::Reset { name } => client::lane_reset(file, &name),
        LaneAction::Prune {
            older_than,
            dry_run,
        } => client::lane_prune(file, older_than.as_deref(), dry_run),
        LaneAction::Move { name, to } => client::lane_move(file, &name, &to),
        LaneAction::Share { name, with } => client::lane_share(file, &name, &with),
    }
}
