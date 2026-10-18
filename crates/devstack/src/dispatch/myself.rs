//! devstack itself on this machine: `devstack update`, `devstack self`.

use crate::{Cmd, SelfAction, selfcmd, update};
use anyhow::Result;

pub(crate) fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Update { check, version } => update::run(check, version.as_deref()),
        Cmd::Myself {
            action: SelfAction::Uninstall { yes },
        } => selfcmd::uninstall(yes),
        _ => super::misrouted(),
    }
}
