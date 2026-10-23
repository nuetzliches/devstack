//! The optional native macOS menu client (ADR-0060).
//!
//! Its owner collects status and performs commands on a worker. This module
//! only presents snapshots and sends action IDs back to that worker.

use std::sync::mpsc::{Receiver, Sender};

/// A complete snapshot, independent of the desktop toolkit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub tooltip: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Label(String),
    Separator,
    Action {
        label: String,
        id: String,
        enabled: bool,
    },
    Submenu {
        label: String,
        entries: Vec<Entry>,
    },
}

#[cfg(target_os = "macos")]
mod macos;

/// Run on the process's main thread until the user exits the menu client or
/// its worker disconnects. The reserved action ID `quit` exits only this
/// client, after sending the action to its owner; it stops no other process.
pub fn run(initial: Menu, updates: Receiver<Menu>, actions: Sender<String>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::run(initial, updates, actions)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (initial, updates, actions);
        Err("The devstack menu bar is available on macOS only.".into())
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn unavailable_without_a_macos_desktop() {
        let (_, updates) = std::sync::mpsc::channel();
        let (actions, _) = std::sync::mpsc::channel();
        let result = run(
            Menu {
                tooltip: "devstack".into(),
                entries: vec![],
            },
            updates,
            actions,
        );
        assert!(result.unwrap_err().contains("macOS only"));
    }
}
