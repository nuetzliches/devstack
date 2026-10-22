//! Linux: `org.freedesktop.Notifications` on the session bus (ADR-0045
//! point 5), in pure Rust, so the binary stays static. Without a session
//! bus, or without a notification service on it (a server, a container),
//! notifications are off and say why. A click (the `default` action) is
//! heard here and opens the dashboard (point 6).

use crate::notify::{Failure, Hooks, Note, Sink};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio_stream::StreamExt;

/// How many notifications a click is remembered for.
const REMEMBERED: usize = 64;

pub struct Bus {
    inner: Arc<Inner>,
}

struct Inner {
    proxy: tokio::sync::Mutex<Option<zbus::Proxy<'static>>>,
    status: Mutex<Result<(), String>>,
    /// Notification id -> the route a click opens, oldest first.
    clicks: Mutex<VecDeque<(u32, String)>>,
    /// Tag -> the id of the notification shown for it, to replace it.
    tags: Mutex<HashMap<String, u32>>,
    hooks: Hooks,
    /// Why the last click could not open the dashboard; a click that does
    /// clears it.
    open_error: Mutex<Option<String>>,
}

impl Bus {
    pub fn new(hooks: Hooks) -> Self {
        let inner = Arc::new(Inner {
            proxy: tokio::sync::Mutex::new(None),
            status: Mutex::new(Err("not connected to the session bus yet".into())),
            clicks: Mutex::new(VecDeque::new()),
            tags: Mutex::new(HashMap::new()),
            hooks,
            open_error: Mutex::new(None),
        });
        let first = inner.clone();
        tokio::spawn(async move {
            first.proxy().await;
        });
        Self { inner }
    }
}

impl Inner {
    /// The notification service, connected on first use (and again after
    /// a failure: a desktop session may come later than the hub).
    async fn proxy(self: &Arc<Self>) -> Option<zbus::Proxy<'static>> {
        let mut p = self.proxy.lock().await;
        if p.is_none() {
            match connect().await {
                Ok(proxy) => {
                    listen(self.clone(), proxy.clone());
                    *p = Some(proxy);
                    *self.status.lock().unwrap() = Ok(());
                }
                Err(e) => *self.status.lock().unwrap() = Err(e),
            }
        }
        p.clone()
    }

    /// A click on the notification for `route`: open the dashboard there,
    /// and keep whether it could.
    fn clicked(&self, route: &str) {
        *self.open_error.lock().unwrap() = (self.hooks.open)(route).err();
    }
}

async fn connect() -> Result<zbus::Proxy<'static>, String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| format!("no desktop session (no D-Bus session bus: {e})"))?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    .map_err(|e| e.to_string())?;
    let _: (String, String, String, String) = proxy
        .call("GetServerInformation", &())
        .await
        .map_err(|e| format!("no notification service on the session bus ({e})"))?;
    Ok(proxy)
}

/// Clicks on our notifications, for as long as the connection lives.
fn listen(inner: Arc<Inner>, proxy: zbus::Proxy<'static>) {
    tokio::spawn(async move {
        let Ok(mut invoked) = proxy.receive_signal("ActionInvoked").await else {
            return;
        };
        while let Some(msg) = invoked.next().await {
            let Ok((id, action)) = msg.body().deserialize::<(u32, String)>() else {
                continue;
            };
            if action != "default" {
                continue;
            }
            let target = inner
                .clicks
                .lock()
                .unwrap()
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, t)| t.clone());
            if let Some(t) = target {
                inner.clicked(&t);
            }
        }
    });
}

impl Sink for Bus {
    fn status(&self) -> Result<(), String> {
        self.inner.status.lock().unwrap().clone()
    }

    fn trouble(&self) -> Option<String> {
        self.inner.open_error.lock().unwrap().clone()
    }

    fn show(&self, n: Note) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let Some(proxy) = inner.proxy().await else {
                return;
            };
            let body = [n.context.as_str(), n.body.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let replaces = inner.tags.lock().unwrap().get(&n.tag).copied().unwrap_or(0);
            let hints: HashMap<&str, zbus::Variant<'_>> = HashMap::new();
            let shown: zbus::Result<u32> = proxy
                .call(
                    "Notify",
                    &(
                        "devstack",
                        replaces,
                        "",
                        n.title.as_str(),
                        esc(&body).as_str(),
                        vec!["default", "Open the dashboard"],
                        hints,
                        -1i32,
                    ),
                )
                .await;
            match shown {
                Ok(id) => {
                    inner.tags.lock().unwrap().insert(n.tag, id);
                    let mut clicks = inner.clicks.lock().unwrap();
                    clicks.retain(|(i, _)| *i != id);
                    clicks.push_back((id, n.route));
                    while clicks.len() > REMEMBERED {
                        clicks.pop_front();
                    }
                }
                Err(e) => {
                    (inner.hooks.failed)(Failure::NotShown(e.to_string()));
                    // Connect again next time: the service may have gone.
                    *inner.proxy.lock().await = None;
                }
            }
        });
    }
}

/// The body may be read as markup (the specification's `body-markup`).
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn a_click_that_cannot_open_the_dashboard_is_trouble_until_one_can() {
        let fail = Arc::new(AtomicBool::new(true));
        let hooks = Hooks {
            open: {
                let fail = fail.clone();
                Arc::new(move |_: &str| -> Result<(), String> {
                    if fail.load(Ordering::SeqCst) {
                        Err("xdg-open did not start".into())
                    } else {
                        Ok(())
                    }
                })
            },
            failed: Arc::new(|_| {}),
        };
        let bus = Bus {
            inner: Arc::new(Inner {
                proxy: tokio::sync::Mutex::new(None),
                status: Mutex::new(Ok(())),
                clicks: Mutex::new(VecDeque::new()),
                tags: Mutex::new(HashMap::new()),
                hooks,
                open_error: Mutex::new(None),
            }),
        };
        assert_eq!(bus.trouble(), None);
        bus.inner.clicked("/#/");
        assert_eq!(bus.trouble().as_deref(), Some("xdg-open did not start"));
        fail.store(false, Ordering::SeqCst);
        bus.inner.clicked("/#/");
        assert_eq!(bus.trouble(), None);
    }
}
