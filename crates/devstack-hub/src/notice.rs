//! What happened once in the hub that no request reports (ADR-0050 point
//! 3): a shared lane's tunnel refused after its page opened, a desktop
//! notification not shown. The dashboard hears it on `/hub/events`
//! (`notice`) and shows it as a toast. The caller logs it, as before.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// The same text again within this time is not told again: a page opens
/// several connections, each refused alike.
const QUIET: Duration = Duration::from_secs(30);

fn sender() -> &'static broadcast::Sender<Value> {
    static TX: OnceLock<broadcast::Sender<Value>> = OnceLock::new();
    TX.get_or_init(|| broadcast::channel(16).0)
}

/// Tell the dashboards: `level` is `warn` or `error`.
pub fn tell(level: &str, text: String) {
    static LAST: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap();
        let last = last.get_or_insert_with(HashMap::new);
        last.retain(|_, at| at.elapsed() < QUIET);
        if last.contains_key(&text) {
            return;
        }
        last.insert(text.clone(), Instant::now());
    }
    // Nobody listening (no dashboard open) is not an error.
    let _ = sender().send(json!({ "type": "notice", "level": level, "text": text }));
}

pub fn subscribe() -> broadcast::Receiver<Value> {
    sender().subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_is_told_once_in_a_while() {
        let mut rx = subscribe();
        tell("error", "port 3000 of lane x: refused".into());
        tell("error", "port 3000 of lane x: refused".into());
        tell("warn", "another".into());
        let told: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(told.len(), 2, "{told:?}");
        assert_eq!(told[0]["type"], "notice");
        assert_eq!(told[1]["text"], "another");
    }
}
