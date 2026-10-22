//! The foreground on Windows, for a hub that runs in the background: a
//! window it opens (the folder dialog) or a program it starts (ADR-0046)
//! would appear behind the browser that asked for it, as Windows lets a
//! background process neither take the foreground nor give it.
//!
//! [`Front`] is an invisible, topmost window brought to the foreground. The
//! usual way around the lock is a synthetic Alt key event, after which
//! `SetForegroundWindow` is allowed. While the hub has the foreground, it
//! lets any process take it next (`AllowSetForegroundWindow(ASFW_ANY)`,
//! until the user's next input): the program it starts, or the running one
//! a link is handed to (VS Code, Orca).

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
    ASFW_ANY, AllowSetForegroundWindow, CreateWindowExW, DestroyWindow, SW_SHOW,
    SetForegroundWindow, ShowWindow, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

/// The hub's window in the foreground, until dropped. Created and dropped on
/// the same thread (a window belongs to the thread that created it).
pub struct Front(HWND);

impl Front {
    /// `None`: no window could be created; what follows may open behind.
    pub fn take() -> Option<Self> {
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
            AllowSetForegroundWindow(ASFW_ANY);
            Some(Front(hwnd))
        }
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        // SAFETY: our own window.
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

impl HasWindowHandle for Front {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let h = NonZeroIsize::new(self.0 as isize).ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(h));
        // SAFETY: the window lives as long as `self`.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl HasDisplayHandle for Front {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::windows())
    }
}
