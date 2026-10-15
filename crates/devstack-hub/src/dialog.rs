//! The native folder dialog for "Add a stack" (`POST /hub/pick-folder`).
//!
//! Windows: the Explorer folder picker (`IFileOpenDialog`), Linux: the
//! desktop's dialog through the XDG portal (no GTK linked in). The hub has no
//! window of its own and runs in the background, so on Windows the dialog gets
//! a small owner window that is brought to the front first; without one it
//! would open behind the browser that asked for it.

/// The chosen folder, `None` when cancelled, `Err` when no dialog can be shown.
pub fn pick_folder() -> Result<Option<String>, String> {
    imp::pick()
}

#[cfg(windows)]
mod imp {
    pub fn pick() -> Result<Option<String>, String> {
        let owner = super::owner::Owner::new();
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

#[cfg(windows)]
mod owner {
    //! An invisible, topmost owner window in the foreground. A background
    //! process may not take the foreground; the usual way around it is a
    //! synthetic Alt key event, after which `SetForegroundWindow` is allowed.
    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle,
        Win32WindowHandle, WindowHandle,
    };
    use std::num::NonZeroIsize;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_MENU,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, SW_SHOW, SetForegroundWindow, ShowWindow, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_POPUP,
    };

    pub struct Owner(HWND);

    impl Owner {
        pub fn new() -> Option<Self> {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let title: Vec<u16> = "devstack\0".encode_utf16().collect();
            // SAFETY: plain Win32 calls; the window is destroyed in Drop.
            unsafe {
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    return None;
                }
                let alt = |flags| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VK_MENU,
                            wScan: 0,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                let inputs = [alt(0), alt(KEYEVENTF_KEYUP)];
                SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
                ShowWindow(hwnd, SW_SHOW);
                SetForegroundWindow(hwnd);
                Some(Owner(hwnd))
            }
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            // SAFETY: our own window.
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }

    impl HasWindowHandle for Owner {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let h = NonZeroIsize::new(self.0 as isize).ok_or(HandleError::Unavailable)?;
            let raw = RawWindowHandle::Win32(Win32WindowHandle::new(h));
            // SAFETY: the window lives as long as `self`.
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }

    impl HasDisplayHandle for Owner {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            Ok(DisplayHandle::windows())
        }
    }
}
