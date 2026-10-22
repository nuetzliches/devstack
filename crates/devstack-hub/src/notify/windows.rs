//! Windows: a WinRT toast (ADR-0045 point 4). Windows shows a toast of a
//! program without a package when its AppUserModelID is registered under
//! `HKCU\Software\Classes\AppUserModelId\<id>`; no Start-menu shortcut, no
//! admin rights. A click launches the toast's link (point 6): the
//! `devstack://open` link when it is registered, else the hub's address.

use super::{Shown, Sink};
use devstack_daemon::diag;
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{
    NotificationSetting, ToastNotification, ToastNotificationManager,
};
use windows::core::{HRESULT, HSTRING};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
};

const AUMID: &str = "devstack.hub";
const AUMID_KEY: &str = r"Software\Classes\AppUserModelId\devstack.hub";
/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`.
const NOT_FOUND: HRESULT = HRESULT(0x8007_0490_u32 as i32);
/// Where `devstack hub install` registers the `devstack://` link.
const LINK_KEY: &str = r"Software\Classes\devstack\shell\open\command";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Register the AppUserModelID under which the hub's toasts show; `Err`
/// with what follows when it could not be (logged).
pub fn register() -> Result<(), String> {
    let key = wide(AUMID_KEY);
    let name = wide("DisplayName");
    let value = wide("devstack");
    // SAFETY: NUL-terminated UTF-16 buffers that outlive the call; the data
    // length is in bytes and includes the terminator, as REG_SZ wants.
    let rc = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            value.as_ptr().cast(),
            (value.len() * 2) as u32,
        )
    };
    if rc == ERROR_SUCCESS {
        return Ok(());
    }
    diag::warn(
        "notify_register_failed",
        serde_json::json!({ "error": format!("registering {AUMID}: error {rc}") }),
    );
    Err(format!(
        "devstack's name could not be registered with Windows (HKCU\\{AUMID_KEY}: error {rc}), \
         so notifications may show without it, or not at all."
    ))
}

pub fn unregister() {
    let key = wide(AUMID_KEY);
    // SAFETY: a NUL-terminated UTF-16 buffer that outlives the call.
    unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) };
}

/// The `devstack://` link is registered (`devstack hub install`).
fn link_registered() -> bool {
    let key = wide(LINK_KEY);
    // SAFETY: asks only whether the default value exists (no buffer).
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            std::ptr::null(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    rc == ERROR_SUCCESS
}

pub struct Toast {
    port: u16,
    /// How the registration at hub start went (`register`).
    registered: Result<(), String>,
}

impl Toast {
    pub fn new(port: u16, registered: Result<(), String>) -> Self {
        Self { port, registered }
    }
}

impl Sink for Toast {
    fn status(&self) -> Result<(), String> {
        let setting =
            match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))
                .and_then(|n| n.Setting())
            {
                Ok(s) => s,
                // Windows knows a program's setting once it has shown a toast;
                // before the first one there is none to ask (measured 2026-10-21).
                Err(e) if e.code() == NOT_FOUND => return Ok(()),
                Err(e) => {
                    return Err(format!(
                        "Windows does not take notifications from devstack: {e}"
                    ));
                }
            };
        match setting {
            NotificationSetting::Enabled => Ok(()),
            NotificationSetting::DisabledForApplication => {
                Err("switched off for devstack in Windows' notification settings".into())
            }
            NotificationSetting::DisabledForUser => {
                Err("notifications are switched off in Windows' settings".into())
            }
            NotificationSetting::DisabledByGroupPolicy => {
                Err("notifications are switched off by group policy".into())
            }
            _ => Err("Windows does not show notifications from devstack".into()),
        }
    }

    fn trouble(&self) -> Option<String> {
        self.registered.clone().err()
    }

    fn show(&self, n: Shown) {
        let launch = if link_registered() {
            n.target.link()
        } else {
            format!("http://127.0.0.1:{}{}", self.port, n.target.path())
        };
        let xml = toast_xml(&n, &launch);
        tokio::task::spawn_blocking(move || {
            if let Err(e) = show(&xml, &n.tag) {
                diag::warn(
                    "notify_failed",
                    serde_json::json!({ "error": e.to_string() }),
                );
                crate::notice::tell(
                    "warn",
                    format!("A desktop notification could not be shown: {e}"),
                );
            }
        });
    }
}

fn show(xml: &str, tag: &str) -> windows::core::Result<()> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    toast.SetTag(&HSTRING::from(tag))?;
    toast.SetGroup(&HSTRING::from("devstack"))?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
}

/// A toast with a title, whose it is and why; a click launches `launch`.
fn toast_xml(n: &Shown, launch: &str) -> String {
    let mut texts = format!("<text>{}</text>", esc(&n.title));
    for t in [&n.context, &n.body] {
        if !t.is_empty() {
            texts.push_str(&format!("<text>{}</text>", esc(t)));
        }
    }
    format!(
        r#"<toast activationType="protocol" launch="{}"><visual><binding template="ToastGeneric">{texts}</binding></visual></toast>"#,
        esc(launch)
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Target;

    #[test]
    fn the_toast_escapes_what_it_shows() {
        let n = Shown {
            title: "api: build failed".into(),
            context: "shop · lane feat-x".into(),
            body: "error: expected `<T>` & \"more\"".into(),
            target: Target::default(),
            tag: "t".into(),
        };
        let xml = toast_xml(&n, "devstack://open?stack=ab&lane=x");
        assert!(xml.contains(r#"launch="devstack://open?stack=ab&amp;lane=x""#));
        assert!(xml.contains("<text>error: expected `&lt;T&gt;` &amp; &quot;more&quot;</text>"));
        let doc = XmlDocument::new().unwrap();
        doc.LoadXml(&HSTRING::from(xml)).unwrap();
    }
}
