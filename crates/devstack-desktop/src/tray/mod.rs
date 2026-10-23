//! The tray (ADR-0060): an icon in the notification area (Windows), a
//! StatusNotifierItem (Linux) or a status item in the menu bar (macOS), with
//! a menu. What it shows is made by its owner (`devstack tray`) as a [`Menu`];
//! this module only draws it and sends the chosen action's id back.
//!
//! [`run`] takes the calling thread, which must be the process's main thread
//! (AppKit wants it; Windows and Linux do not mind), until the menu's `quit`
//! action, or until the owner drops its end of `updates`.

use std::sync::mpsc::{Receiver, Sender};

mod icon;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

pub use icon::{dot, rgba};

/// What the icon says, in the state's colour (ADR-0060 point 4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tone {
    /// Nothing runs, or the hub does not.
    #[default]
    Idle,
    /// Something runs, and nothing needs attention.
    Ok,
    /// A stack starts or stops.
    Busy,
    /// Something needs attention.
    Attention,
}

/// Everything the tray shows: the icon, its tooltip and the menu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Menu {
    pub tone: Tone,
    pub tooltip: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// Text that cannot be chosen.
    Label(String),
    Separator,
    /// Sends `id` when chosen.
    Action {
        label: String,
        id: String,
        enabled: bool,
    },
    Submenu {
        label: String,
        /// A state's mark before the label: a coloured dot where the
        /// platform shows images (Windows), else [`mark_text`].
        mark: Option<Tone>,
        entries: Vec<Entry>,
    },
}

/// A state's mark as text, where the menu shows no image: the same shape
/// for every tone would say nothing without colour.
pub fn mark_text(tone: Tone) -> &'static str {
    match tone {
        Tone::Ok => "●",
        Tone::Attention => "▲",
        Tone::Busy => "◌",
        Tone::Idle => "○",
    }
}

/// `label` with its mark as text in front, for the platforms without images.
pub fn marked(label: &str, mark: Option<Tone>) -> String {
    match mark {
        Some(tone) => format!("{} {label}", mark_text(tone)),
        None => label.to_string(),
    }
}

/// The action id that ends [`run`] (after it was sent to the owner).
pub const QUIT: &str = "quit";

/// Show `initial`, then each menu from `updates`, and send the id of every
/// chosen action to `actions`. Returns when [`QUIT`] is chosen or `updates`
/// is disconnected; `Err` when no tray can be shown here.
pub fn run(initial: Menu, updates: Receiver<Menu>, actions: Sender<String>) -> Result<(), String> {
    os::run(initial, updates, actions)
}

/// Why no tray can be shown in this session, or `None`: Linux without a
/// session bus (a server, a container, an SSH login).
pub fn unavailable() -> Option<String> {
    os::unavailable()
}

#[cfg(target_os = "linux")]
use self::linux as os;
#[cfg(target_os = "macos")]
use self::macos as os;
#[cfg(windows)]
use self::windows as os;

/// What a menu's label shows on one line: no control characters, at most
/// `max` characters.
pub fn one_line(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.trim();
    if clean.chars().count() > max {
        format!(
            "{}…",
            clean
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        clean.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_one_short_line() {
        assert_eq!(one_line("  a\nb\tc ", 10), "a b c");
        assert_eq!(one_line("abcdef", 4), "abc…");
        assert_eq!(one_line("äöü", 3), "äöü");
    }
}
