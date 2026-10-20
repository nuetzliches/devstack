//! devstack itself on this machine: `devstack update`, `devstack self`.

use crate::{Cmd, SelfAction, selfcmd, update};
use anyhow::Result;

pub(crate) fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Update { check, version } => update::run(check, version.as_deref()),
        Cmd::Myself {
            action:
                SelfAction::Uninstall {
                    yes,
                    with_coordinator,
                },
        } => selfcmd::uninstall(yes, with_coordinator),
        _ => super::misrouted(),
    }
}
