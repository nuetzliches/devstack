//! Windows: an icon in the notification area (`Shell_NotifyIconW`) owned by
//! a hidden window of this thread, with a popup menu on either click
//! (ADR-0060). A timer on that window takes the owner's menus; Explorer's
//! `TaskbarCreated` (it restarted) adds the icon again. The menu follows the
//! system's dark or light mode for apps ([`dark_menus`]).

use super::{Entry, Menu, QUIT, Tone};
use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics,
    HICON, HMENU, KillTimer, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow,
    SetTimer, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
    WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW,
    WS_POPUP,
};

/// The icon's messages to its window.
const CALLBACK: u32 = WM_APP + 1;
const TIMER: usize = 1;

pub fn unavailable() -> Option<String> {
    None
}

/// What the window procedure works with; one tray per thread.
struct State {
    hwnd: HWND,
    menu: Menu,
    icon: HICON,
    updates: Receiver<Menu>,
    actions: Sender<String>,
    taskbar_created: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Popup menus in the system's dark mode for apps, light otherwise. Windows
/// documents no API for it: `uxtheme.dll` exports by ordinal only
/// `SetPreferredAppMode` (135; `AllowDarkModeForApp` with the same argument
/// on 1809), `AllowDarkModeForWindow` (133) and `FlushMenuThemes` (136), as
/// Explorer and the other programs with dark menus use them. Where one is
/// missing (before Windows 10 1809) the menu stays light. Called again on
/// `WM_SETTINGCHANGE` "ImmersiveColorSet": the menus follow a change of mode.
fn dark_menus(hwnd: HWND) {
    type AppMode = unsafe extern "system" fn(i32) -> i32;
    type ForWindow = unsafe extern "system" fn(HWND, i32) -> i32;
    type Flush = unsafe extern "system" fn();
    // `AllowDark`: dark when the system's apps are dark.
    const ALLOW_DARK: i32 = 1;
    // SAFETY: uxtheme.dll stays loaded for the process (it is not freed);
    // each ordinal is called with the signature it has had since 1809, and
    // only when it exists.
    unsafe {
        let lib = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if lib.is_null() {
            return;
        }
        let by_ordinal = |n: usize| GetProcAddress(lib, n as *const u8);
        let (Some(app), Some(window), Some(flush)) =
            (by_ordinal(135), by_ordinal(133), by_ordinal(136))
        else {
            return;
        };
        let app: AppMode = std::mem::transmute(app);
        let window: ForWindow = std::mem::transmute(window);
        let flush: Flush = std::mem::transmute(flush);
        app(ALLOW_DARK);
        window(hwnd, 1);
        flush();
    }
}

/// Whether `WM_SETTINGCHANGE` says the colours of apps changed.
fn colours_changed(lp: LPARAM) -> bool {
    if lp == 0 {
        return false;
    }
    let want = wide("ImmersiveColorSet");
    // SAFETY: for WM_SETTINGCHANGE a non-zero lParam is a NUL-terminated
    // wide string; it is read only up to the length compared.
    unsafe {
        let p = lp as *const u16;
        (0..want.len()).all(|i| *p.add(i) == want[i])
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The icon at the small-icon size, from the shared drawing.
fn make_icon(tone: Tone) -> HICON {
    // SAFETY: plain Win32 calls with buffers that outlive them.
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).clamp(16, 64) as u32;
        let rgba = super::rgba(size, tone);
        // Windows wants BGRA; straight alpha.
        let bgra: Vec<u8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0], p[3]])
            .collect();
        // The mask is unused where the colour has alpha; rows of whole words.
        let mask = vec![0u8; (size.div_ceil(16) * 2 * size) as usize];
        CreateIcon(
            GetModuleHandleW(std::ptr::null()),
            size as i32,
            size as i32,
            1,
            32,
            mask.as_ptr(),
            bgra.as_ptr(),
        )
    }
}

fn data(s: &State) -> NOTIFYICONDATAW {
    // SAFETY: an all-zero NOTIFYICONDATAW is valid (no handles, empty strings).
    let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = s.hwnd;
    d.uID = 1;
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    d.uCallbackMessage = CALLBACK;
    d.hIcon = s.icon;
    let tip: Vec<u16> = super::one_line(&s.menu.tooltip, 127)
        .encode_utf16()
        .collect();
    let n = tip.len().min(d.szTip.len() - 1);
    d.szTip[..n].copy_from_slice(&tip[..n]);
    d
}

fn show(s: &State, how: u32) {
    let d = data(s);
    // SAFETY: `d` is a valid NOTIFYICONDATAW for our own window.
    unsafe {
        Shell_NotifyIconW(how, &d);
    }
}

/// `&` marks an access key in a menu's text.
fn menu_text(label: &str) -> Vec<u16> {
    wide(&super::one_line(label, 120).replace('&', "&&"))
}

/// The popup menu for `entries`; command ids from 1, their actions in `ids`.
fn build(entries: &[Entry], ids: &mut Vec<String>) -> HMENU {
    // SAFETY: plain Win32 calls; the menu is destroyed by the caller (with
    // its submenus).
    unsafe {
        let menu = CreatePopupMenu();
        for e in entries {
            match e {
                Entry::Separator => {
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                }
                Entry::Label(text) => {
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, menu_text(text).as_ptr());
                }
                Entry::Action { label, id, enabled } => {
                    ids.push(id.clone());
                    let flags = if *enabled {
                        MF_STRING
                    } else {
                        MF_STRING | MF_GRAYED
                    };
                    AppendMenuW(menu, flags, ids.len(), menu_text(label).as_ptr());
                }
                Entry::Submenu { label, entries } => {
                    let sub = build(entries, ids);
                    AppendMenuW(
                        menu,
                        MF_STRING | MF_POPUP,
                        sub as usize,
                        menu_text(label).as_ptr(),
                    );
                }
            }
        }
        menu
    }
}

/// Show the menu at the cursor and send what was chosen.
fn popup(hwnd: HWND) {
    let Some((entries, actions)) = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|s| (s.menu.entries.clone(), s.actions.clone()))
    }) else {
        return;
    };
    let mut ids = Vec::new();
    let menu = build(&entries, &mut ids);
    // SAFETY: our own window and menu. The window must be in front, or the
    // menu does not close when the user clicks elsewhere; WM_NULL afterwards
    // as Microsoft documents for notification-area menus.
    let chosen = unsafe {
        let mut at = POINT { x: 0, y: 0 };
        GetCursorPos(&mut at);
        SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        chosen
    };
    if chosen <= 0 {
        return;
    }
    let Some(id) = ids.get(chosen as usize - 1).cloned() else {
        return;
    };
    let quit = id == QUIT;
    if actions.send(id).is_err() || quit {
        // SAFETY: our own window, on its thread.
        unsafe {
            DestroyWindow(hwnd);
        }
    }
}

/// Take the owner's latest menu; the window ends when the owner is gone.
fn poll(hwnd: HWND) {
    let mut gone = false;
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(s) = s.as_mut() else {
            return;
        };
        let mut next = None;
        loop {
            match s.updates.try_recv() {
                Ok(m) => next = Some(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    gone = true;
                    break;
                }
            }
        }
        let Some(next) = next else {
            return;
        };
        if next == s.menu {
            return;
        }
        let tone = next.tone != s.menu.tone;
        let tip = next.tooltip != s.menu.tooltip;
        s.menu = next;
        if tone {
            let old = s.icon;
            s.icon = make_icon(s.menu.tone);
            show(s, NIM_MODIFY);
            // SAFETY: the icon the notification area no longer uses.
            unsafe {
                DestroyIcon(old);
            }
        } else if tip {
            show(s, NIM_MODIFY);
        }
    });
    if gone {
        // SAFETY: our own window, on its thread.
        unsafe {
            DestroyWindow(hwnd);
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        CALLBACK => {
            let event = (lp & 0xffff) as u32;
            if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
                popup(hwnd);
            }
            0
        }
        WM_TIMER if wp == TIMER => {
            poll(hwnd);
            0
        }
        WM_SETTINGCHANGE if colours_changed(lp) => {
            dark_menus(hwnd);
            0
        }
        WM_DESTROY => {
            STATE.with(|s| {
                if let Some(s) = s.borrow().as_ref() {
                    show(s, NIM_DELETE);
                }
            });
            // SAFETY: our own window and timer, on its thread.
            unsafe {
                KillTimer(hwnd, TIMER);
                PostQuitMessage(0);
            }
            0
        }
        _ => {
            let again = STATE.with(|s| {
                s.borrow()
                    .as_ref()
                    .is_some_and(|s| s.taskbar_created != 0 && msg == s.taskbar_created)
            });
            if again {
                // Explorer restarted: the icon is gone from the new taskbar.
                STATE.with(|s| {
                    if let Some(s) = s.borrow().as_ref() {
                        show(s, NIM_ADD);
                    }
                });
                return 0;
            }
            // SAFETY: the default procedure for our own window.
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
    }
}

pub fn run(initial: Menu, updates: Receiver<Menu>, actions: Sender<String>) -> Result<(), String> {
    let class = wide("devstack-tray");
    // SAFETY: plain Win32 calls; the window, icon and timer are ours and
    // released when the message loop ends.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(window_proc);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        // A second registration in the same process fails; the class is the same.
        RegisterClassW(&wc);
        // A hidden top-level window, not a message-only one: only top-level
        // windows hear Explorer's `TaskbarCreated` broadcast.
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("devstack").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return Err("Windows could not create the tray's window".into());
        }
        dark_menus(hwnd);
        let state = State {
            hwnd,
            icon: make_icon(initial.tone),
            menu: initial,
            updates,
            actions,
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
        };
        let d = data(&state);
        let added = Shell_NotifyIconW(NIM_ADD, &d) != 0;
        let icon = state.icon;
        STATE.with(|s| *s.borrow_mut() = Some(state));
        if !added {
            DestroyWindow(hwnd);
            STATE.with(|s| s.borrow_mut().take());
            DestroyIcon(icon);
            return Err("Windows did not add the icon to the notification area".into());
        }
        SetTimer(hwnd, TIMER, 200, None);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(s) = STATE.with(|s| s.borrow_mut().take()) {
            DestroyIcon(s.icon);
        }
    }
    Ok(())
}
