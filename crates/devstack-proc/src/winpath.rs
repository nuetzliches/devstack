//! PATH for a process the daemon starts on Windows: the daemon's own, plus
//! what an installer added to the registry since the daemon started.
//!
//! A process inherits its environment once, at its start. The daemon (and the
//! hub that may start it) runs for days, so a tool installed after its start
//! (`winget install GoLang.Go`) was missing from every build and service until
//! `devstack down` and `up` from a new terminal. Windows keeps the PATH a new
//! terminal gets in the registry (the machine's and the user's `Path`), so each
//! start reads it again and appends what is new. The daemon's own entries stay
//! first and in their order: a terminal's additions (a virtualenv, a tool's
//! version manager) keep winning, nothing is removed or reordered.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr;
use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
};

const MACHINE: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const USER: &str = "Environment";

/// The PATH for a new process: `current` (the daemon's), with the registry's
/// entries it lacks appended. `None` when nothing is new, so the caller leaves
/// the inherited PATH alone.
pub(crate) fn refreshed(current: Option<&OsStr>) -> Option<OsString> {
    let registry: Vec<OsString> = [
        read(HKEY_LOCAL_MACHINE, MACHINE),
        read(HKEY_CURRENT_USER, USER),
    ]
    .into_iter()
    .flatten()
    .collect();
    merge(current.unwrap_or_default(), &registry)
}

/// `current` with the entries of `extra` (each a `;`-separated list) it does
/// not have yet appended, in order; `None` when there are none. Entries
/// compare as Windows does: ignoring case and a trailing backslash.
pub(crate) fn merge(current: &OsStr, extra: &[OsString]) -> Option<OsString> {
    let mut seen: Vec<String> = split(current).map(|e| key(&e)).collect();
    let mut added: Vec<OsString> = Vec::new();
    for list in extra {
        for e in split(list) {
            let k = key(&e);
            if !seen.contains(&k) {
                seen.push(k);
                added.push(e);
            }
        }
    }
    if added.is_empty() {
        return None;
    }
    let mut out = current.to_os_string();
    for e in added {
        if !out.is_empty() && !out.to_string_lossy().ends_with(';') {
            out.push(";");
        }
        out.push(e);
    }
    Some(out)
}

/// The non-empty entries of a `;`-separated list, trimmed (a registry PATH
/// often carries a stray space or an empty entry).
fn split(list: &OsStr) -> impl Iterator<Item = OsString> + '_ {
    let wide: Vec<u16> = list.encode_wide().collect();
    let parts: Vec<OsString> = wide
        .split(|&c| c == u16::from(b';'))
        .map(|p| OsString::from_wide(p).to_string_lossy().trim().into())
        .filter(|p: &OsString| !p.is_empty())
        .collect();
    parts.into_iter()
}

fn key(entry: &OsStr) -> String {
    entry
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
}

/// A `Path` value under `root\subkey`, its `%VARIABLES%` expanded (by
/// `RegGetValueW`, against this process's environment). `None` if absent.
fn read(root: HKEY, subkey: &str) -> Option<OsString> {
    let subkey = wide(subkey);
    let name = wide("Path");
    let mut len: u32 = 0;
    // SAFETY: null buffer, the call only reports the size it needs.
    let rc = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut len,
        )
    };
    if rc != ERROR_SUCCESS && rc != ERROR_MORE_DATA {
        return None;
    }
    // The expanded value can be longer than the size reported for the raw
    // one: retry with more room a few times.
    for _ in 0..4 {
        let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `bytes` bytes.
        let rc = unsafe {
            RegGetValueW(
                root,
                subkey.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if rc == ERROR_MORE_DATA {
            len = bytes.max(len.saturating_mul(2));
            continue;
        }
        if rc != ERROR_SUCCESS {
            return None;
        }
        let n = (bytes as usize / 2).min(buf.len());
        let end = buf[..n].iter().position(|&c| c == 0).unwrap_or(n);
        return Some(OsString::from_wide(&buf[..end]));
    }
    None
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(current: &str, extra: &[&str]) -> Option<String> {
        let extra: Vec<OsString> = extra.iter().map(OsString::from).collect();
        merge(OsStr::new(current), &extra).map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn a_tool_installed_later_is_appended() {
        assert_eq!(
            m(
                r"C:\venv\Scripts;C:\Windows",
                &[r"C:\Windows;C:\Program Files\Go\bin"]
            )
            .as_deref(),
            Some(r"C:\venv\Scripts;C:\Windows;C:\Program Files\Go\bin")
        );
    }

    #[test]
    fn nothing_new_leaves_the_path_alone() {
        assert_eq!(
            m(r"C:\Windows;C:\Go\bin", &[r"c:\windows\;C:\GO\BIN"]),
            None
        );
        assert_eq!(m(r"C:\Windows", &[]), None);
    }

    #[test]
    fn the_daemons_order_wins_and_nothing_is_removed() {
        assert_eq!(
            m(r"C:\b;C:\a", &[r"C:\a;C:\c", r"C:\c;C:\d"]).as_deref(),
            Some(r"C:\b;C:\a;C:\c;C:\d")
        );
    }

    #[test]
    fn empty_entries_and_spaces_are_skipped() {
        assert_eq!(m("", &[r" C:\x ;;"]).as_deref(), Some(r"C:\x"));
        assert_eq!(m(r"C:\a;", &[r"C:\b"]).as_deref(), Some(r"C:\a;C:\b"));
    }

    #[test]
    fn the_registry_path_is_readable() {
        // Every Windows machine has the system's PATH, stored as
        // `%SystemRoot%\system32`: read expanded.
        let p = read(HKEY_LOCAL_MACHINE, MACHINE).expect("the machine's Path");
        let p = p.to_string_lossy().to_lowercase();
        assert!(p.contains(r"\system32"), "{p}");
        assert!(!p.contains("%systemroot%"), "{p}");
    }
}
