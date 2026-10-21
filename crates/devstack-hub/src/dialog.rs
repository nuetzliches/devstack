//! The native folder dialog for "Add a stack" (`POST /hub/pick-folder`).
//!
//! Windows: the Explorer folder picker (`IFileOpenDialog`), Linux: the
//! desktop's dialog through the XDG portal (no GTK linked in). The hub has no
//! window of its own and runs in the background, so on Windows the dialog gets
//! an owner window that is brought to the front first (`foreground::Front`);
//! without one it would open behind the browser that asked for it.

/// The chosen folder, `None` when cancelled, `Err` when no dialog can be shown.
pub fn pick_folder() -> Result<Option<String>, String> {
    imp::pick()
}

#[cfg(windows)]
mod imp {
    pub fn pick() -> Result<Option<String>, String> {
        let owner = crate::foreground::Front::take();
        let mut d = rfd::FileDialog::new().set_title("Choose the repository folder (devstack)");
        if let Some(o) = &owner {
            d = d.set_parent(o);
        }
        Ok(d.pick_folder().map(|p| p.to_string_lossy().into_owned()))
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn pick() -> Result<Option<String>, String> {
        // The portal answers on the session bus; without a desktop session
        // there is none, and rfd returns None just like a cancel.
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            return Err("no desktop session for a folder dialog".into());
        }
        Ok(rfd::FileDialog::new()
            .set_title("Choose the repository folder (devstack)")
            .pick_folder()
            .map(|p| p.to_string_lossy().into_owned()))
    }
}
