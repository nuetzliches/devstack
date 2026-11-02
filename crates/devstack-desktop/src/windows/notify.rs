//! Windows: a WinRT toast (ADR-0045 point 4). Windows shows a toast of a
//! program without a package when its AppUserModelID is registered under
//! `HKCU\Software\Classes\AppUserModelId\<id>`; no Start-menu shortcut, no
//! admin rights. A click launches the toast's link (point 6): the
//! `devstack://open` link when it is registered, else the hub's address.

use crate::notify::{Failure, Hooks, Note, Sink};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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
/// with what follows when it could not be (told to `hooks`).
pub fn register(hooks: &Hooks) -> Result<(), String> {
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
    (hooks.failed)(Failure::NotRegistered(format!(
        "registering {AUMID}: error {rc}"
    )));
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

/// How long a call into Windows' notification platform may take before the
/// sink says it does not answer.
const STUCK: Duration = Duration::from_secs(30);
/// How often the setting is asked again while nothing shows.
const REFRESH: Duration = Duration::from_secs(60);
/// Toasts waiting to be shown; more are dropped while Windows does not answer.
const QUEUE: usize = 16;

/// What the sink asks of Windows: on its own thread, never on one of the
/// hub's runtime workers. Each is a call into the notification platform,
/// another process; one that does not return must not keep the hub from
/// answering (#398).
trait Calls: Send + 'static {
    fn status(&self) -> Result<(), String>;
    fn show(&self, xml: &str, tag: &str) -> Result<(), String>;
}

struct WinRt;

impl Calls for WinRt {
    fn status(&self) -> Result<(), String> {
        setting()
    }

    fn show(&self, xml: &str, tag: &str) -> Result<(), String> {
        show(xml, tag).map_err(|e| e.to_string())
    }
}

/// What the toast thread and the hub share.
struct Shared {
    /// The setting as the toast thread asked it last.
    status: Mutex<Result<(), String>>,
    /// Since when the call under way waits for Windows; `None`: no call.
    busy: Mutex<Option<Instant>>,
    /// The call under way was told as stuck.
    told: AtomicBool,
}

impl Shared {
    fn call<T>(&self, f: impl FnOnce() -> T) -> T {
        *self.busy.lock().unwrap() = Some(Instant::now());
        let out = f();
        *self.busy.lock().unwrap() = None;
        self.told.store(false, Ordering::SeqCst);
        out
    }
}

pub struct Toast {
    /// How the registration at hub start went (`register`).
    registered: Result<(), String>,
    hooks: Hooks,
    shared: Arc<Shared>,
    jobs: SyncSender<Note>,
    stuck: Duration,
}

impl Toast {
    pub fn new(port: u16, registered: Result<(), String>, hooks: Hooks) -> Self {
        Self::with(port, registered, hooks, WinRt, STUCK)
    }

    fn with(
        port: u16,
        registered: Result<(), String>,
        hooks: Hooks,
        calls: impl Calls,
        stuck: Duration,
    ) -> Self {
        let shared = Arc::new(Shared {
            status: Mutex::new(Ok(())),
            busy: Mutex::new(None),
            told: AtomicBool::new(false),
        });
        let (jobs, queue) = sync_channel::<Note>(QUEUE);
        let (s, failed) = (shared.clone(), hooks.failed.clone());
        let _ = std::thread::Builder::new()
            .name("toasts".into())
            .spawn(move || {
                loop {
                    let got = s.call(|| calls.status());
                    *s.status.lock().unwrap() = got;
                    match queue.recv_timeout(REFRESH) {
                        Ok(n) => {
                            let launch = if link_registered() {
                                n.link.clone()
                            } else {
                                format!("http://127.0.0.1:{port}{}", n.route)
                            };
                            let xml = toast_xml(&n, &launch);
                            if let Err(e) = s.call(|| calls.show(&xml, &n.tag)) {
                                failed(Failure::NotShown(e));
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            });
        Self {
            registered,
            hooks,
            shared,
            jobs,
            stuck,
        }
    }
}

/// Whether Windows takes notifications from devstack: a call into the
/// notification platform, made on the toast thread only.
fn setting() -> Result<(), String> {
    let setting = match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))
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

impl Sink for Toast {
    /// The setting as asked last, without asking Windows now; `Err` while
    /// a call has waited longer than it may (told once to the hooks).
    fn status(&self) -> Result<(), String> {
        let since = *self.shared.busy.lock().unwrap();
        if let Some(t) = since.filter(|t| t.elapsed() >= self.stuck) {
            let e = format!(
                "Windows' notification platform has not answered for {} s",
                t.elapsed().as_secs()
            );
            if !self.shared.told.swap(true, Ordering::SeqCst) {
                (self.hooks.failed)(Failure::NotShown(e.clone()));
            }
            return Err(e);
        }
        self.shared.status.lock().unwrap().clone()
    }

    fn trouble(&self) -> Option<String> {
        self.registered.clone().err()
    }

    fn show(&self, n: Note) {
        // Full: Windows has not answered for a while, and what waits
        // already is enough.
        let _ = self.jobs.try_send(n);
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
fn toast_xml(n: &Note, launch: &str) -> String {
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

    #[test]
    fn the_toast_escapes_what_it_shows() {
        let n = Note {
            title: "api: build failed".into(),
            context: "shop · lane feat-x".into(),
            body: "error: expected `<T>` & \"more\"".into(),
            route: "/#/".into(),
            link: "devstack://open".into(),
            tag: "t".into(),
        };
        let xml = toast_xml(&n, "devstack://open?stack=ab&lane=x");
        assert!(xml.contains(r#"launch="devstack://open?stack=ab&amp;lane=x""#));
        assert!(xml.contains("<text>error: expected `&lt;T&gt;` &amp; &quot;more&quot;</text>"));
        let doc = XmlDocument::new().unwrap();
        doc.LoadXml(&HSTRING::from(xml)).unwrap();
    }

    /// Windows as a test has it: a setting, and a `show` that waits until
    /// the test lets it go.
    struct Hanging {
        setting: Result<(), String>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        shown: std::sync::mpsc::Sender<String>,
    }

    impl Calls for Hanging {
        fn status(&self) -> Result<(), String> {
            self.setting.clone()
        }

        fn show(&self, _: &str, tag: &str) -> Result<(), String> {
            let _ = self.release.lock().unwrap().recv();
            let _ = self.shown.send(tag.to_string());
            Ok(())
        }
    }

    fn note(tag: &str) -> Note {
        Note {
            title: "api: build failed".into(),
            context: "shop".into(),
            body: String::new(),
            route: "/#/".into(),
            link: "devstack://open".into(),
            tag: tag.into(),
        }
    }

    fn until(what: impl Fn() -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            if what() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_toast_windows_does_not_answer_keeps_no_caller_waiting() {
        let (release, wait) = std::sync::mpsc::channel();
        let (shown, seen) = std::sync::mpsc::channel();
        let failures = Arc::new(Mutex::new(Vec::new()));
        let hooks = Hooks {
            open: Arc::new(|_: &str| Ok(())),
            failed: {
                let f = failures.clone();
                Arc::new(move |e| f.lock().unwrap().push(e))
            },
        };
        let calls = Hanging {
            setting: Ok(()),
            release: Mutex::new(wait),
            shown,
        };
        let toast = Toast::with(4250, Ok(()), hooks, calls, Duration::from_millis(500));
        assert!(until(|| toast.status().is_ok()));
        // Windows hangs on the first: `show` and `status` still return.
        toast.show(note("a"));
        toast.show(note("b"));
        assert!(toast.status().is_ok(), "not stuck yet");
        assert!(until(|| toast.status().is_err()), "said once stuck");
        let e = toast.status().unwrap_err();
        assert!(e.contains("has not answered"), "{e}");
        assert_eq!(failures.lock().unwrap().len(), 1, "told once");
        // It answers again: what waited shows, and the setting counts.
        release.send(()).unwrap();
        release.send(()).unwrap();
        let tags: Vec<String> = (0..2)
            .map(|_| seen.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(tags, ["a", "b"]);
        assert!(until(|| toast.status().is_ok()));
    }

    #[test]
    fn the_setting_is_the_one_asked_last() {
        let (_release, wait) = std::sync::mpsc::channel();
        let hooks = Hooks {
            open: Arc::new(|_: &str| Ok(())),
            failed: Arc::new(|_| {}),
        };
        let calls = Hanging {
            setting: Err("switched off".into()),
            release: Mutex::new(wait),
            shown: std::sync::mpsc::channel().0,
        };
        let toast = Toast::with(4250, Ok(()), hooks, calls, STUCK);
        assert!(until(|| toast.status() == Err("switched off".into())));
    }
}
