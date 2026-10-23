//! The hub and the dashboard (ADR-0021): `devstack ui`, `devstack hub`, and
//! the `devstack://` links the system hands over.

use crate::{Cmd, HubAction, NotifyAction, TrayAction, hubcmd, integration, tray};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Ui {
            lane,
            link,
            print,
            browser,
        } => hubcmd::ui(file, lane.as_deref(), link, print, browser.as_deref()),
        Cmd::Hub { action } => hub(action),
        Cmd::OpenUrl { url } => integration::open_url(&url),
        Cmd::Tray => tray::run(),
        _ => super::misrouted(),
    }
}

/// `devstack hub`: without an action the hub in the foreground.
fn hub(action: Option<HubAction>) -> Result<()> {
    match action {
        None => devstack_hub::serve().map_err(|e| anyhow::anyhow!(e)),
        Some(HubAction::Status) => {
            println!("{}", hubcmd::status_text());
            Ok(())
        }
        Some(HubAction::Stop) => hubcmd::stop(),
        Some(HubAction::Start) => {
            let port = devstack_hub::ensure_running().map_err(|e| anyhow::anyhow!(e))?;
            println!("hub running: http://127.0.0.1:{port}");
            Ok(())
        }
        Some(HubAction::Install { no_autostart }) => {
            integration::install(!no_autostart)?;
            println!("{}", integration::status_text());
            Ok(())
        }
        Some(HubAction::Uninstall) => {
            integration::uninstall()?;
            println!("{}", integration::status_text());
            Ok(())
        }
        Some(HubAction::Notify { action }) => match action {
            None => hubcmd::notify_show(),
            Some(NotifyAction::On { kinds }) => hubcmd::notify_set(&kinds, true),
            Some(NotifyAction::Off { kinds }) => hubcmd::notify_set(&kinds, false),
            Some(NotifyAction::Test) => hubcmd::notify_test(),
        },
        Some(HubAction::Tray { action }) => match action {
            None => {
                println!("{}", hubcmd::tray_text());
                Ok(())
            }
            Some(TrayAction::On) => hubcmd::tray_on(),
            Some(TrayAction::Off) => hubcmd::tray_off(),
        },
    }
}
