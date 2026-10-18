//! `devstack lane`: the repository's other working trees (plan §3.5).

use crate::{LaneAction, client};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, action: LaneAction) -> Result<()> {
    match action {
        LaneAction::List => client::lane_list(file),
        LaneAction::Stop { name } => client::lane_stop(file, &name),
        LaneAction::Move { name, to } => client::lane_move(file, &name, &to),
        LaneAction::Share { name, with } => client::lane_share(file, &name, &with),
    }
}
