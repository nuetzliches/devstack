//! The hidden commands devstack runs itself: the daemon in the foreground,
//! the static file server of a `kind = "static"` service, the Linux shim.

use crate::Cmd;
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Daemon => devstack_daemon::serve(file).map_err(|e| anyhow::anyhow!(e)),
        Cmd::ServeStatic { dir, port, no_spa } => {
            devstack_daemon::staticsrv::serve(dir, port, !no_spa).map_err(|e| anyhow::anyhow!(e))
        }
        Cmd::Shim { args } => {
            #[cfg(unix)]
            devstack_proc::shim_main(&args);
            #[cfg(not(unix))]
            {
                let _ = args;
                anyhow::bail!("the shim is only used on Linux")
            }
        }
        _ => super::misrouted(),
    }
}
