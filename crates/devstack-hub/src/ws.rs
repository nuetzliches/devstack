//! The dashboard's live streams over WebSocket (ADR-0021, amended).
//!
//! A browser opens at most six HTTP/1.1 connections per host, across every
//! tab and installed-app window of a profile. Each dashboard window holds
//! server-sent event streams open (the hub's, the stack's, a log), so two or
//! three windows used up the six and every further request, a restart click
//! included, waited in the browser's queue without end. WebSockets do not
//! count against that limit.
//!
//! `GET /hub/ws?path=<path>` upgrades to a WebSocket that carries the server-sent
//! event stream at `path` (`/hub/events` or `/hub/stacks/<id>/api/...`): every
//! event's `data` is one text message. The streams themselves stay as they
//! are, so the CLI and older dashboards keep using them. The upstream is
//! opened before the upgrade: a stack that is not running answers the
//! handshake with an error, as the event stream did.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::{Buf, Bytes, BytesMut};
use http_body_util::BodyExt;
use serde::Deserialize;
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::{Stream, StreamExt};

use crate::{AppState, err};

type Messages = Pin<Box<dyn Stream<Item = String> + Send>>;

#[derive(Deserialize)]
pub(crate) struct WsQuery {
    path: String,
}

pub(crate) async fn ws(
    State(s): State<AppState>,
    Query(q): Query<WsQuery>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let messages: Messages = if q.path == "/hub/events" {
        Box::pin(crate::hub_events(&s).map(|v| v.to_string()))
    } else if let Some((id, rest)) = q
        .path
        .strip_prefix("/hub/stacks/")
        .and_then(|r| r.split_once("/api/"))
    {
        let resp = match crate::daemon_get(&s, id, &format!("/{rest}")).await {
            Ok(r) => r,
            Err(e) => return e.into_response(),
        };
        if resp.status() != StatusCode::OK {
            let status = resp.status();
            let body = resp
                .into_body()
                .collect()
                .await
                .map(|b| String::from_utf8_lossy(&b.to_bytes()).into_owned())
                .unwrap_or_default();
            return (status, body).into_response();
        }
        Box::pin(sse_data(resp.into_body().into_data_stream()))
    } else if let Some((lease, rest)) = q
        .path
        .strip_prefix("/hub/pool/shared/")
        .and_then(|r| r.split_once("/api/"))
    {
        // A lane shared with this machine, viewed (ADR-0040) through its node.
        let (rest, query) = rest
            .split_once('?')
            .map_or((rest, None), |(r, q)| (r, Some(q)));
        let pool = query.and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("pool="))
                .map(str::to_string)
        });
        let pq = crate::shared::PoolQuery { pool };
        let path = format!("/{rest}{}", crate::shared::without_pool(query));
        let resp = match crate::shared::view_get(&pq, lease, &path).await {
            Ok(r) => r,
            Err(r) => return r,
        };
        if resp.status() != StatusCode::OK {
            let status = resp.status();
            let body = resp
                .into_body()
                .collect()
                .await
                .map(|b| String::from_utf8_lossy(&b.to_bytes()).into_owned())
                .unwrap_or_default();
            return (status, body).into_response();
        }
        Box::pin(sse_data(resp.into_body().into_data_stream()))
    } else {
        return err(StatusCode::BAD_REQUEST, "no stream at that path");
    };
    upgrade.on_upgrade(move |socket| pump(socket, messages))
}

/// Until either side ends: messages out, pings so a closed tab is noticed
/// even on a quiet stream. When the stream ends the socket closes, and the
/// dashboard reconnects as it did after an event stream ended.
///
/// A ping that goes out says nothing: on a half-open connection (a laptop
/// that slept, a network that went away) it lands in the send buffer, and
/// only the system's retransmit timeout, many minutes later, would end the
/// socket and the daemon stream behind it. So the browser has to answer:
/// after [`MISSED_PINGS`] pings without a word back, the socket is closed.
async fn pump(mut socket: WebSocket, mut messages: Messages) {
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut unanswered = 0u32;
    loop {
        tokio::select! {
            m = messages.next() => match m {
                Some(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                None => {
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                // A pong, or anything else: the other side is there.
                Some(Ok(_)) => unanswered = 0,
            },
            _ = ping.tick() => {
                if unanswered >= MISSED_PINGS {
                    return;
                }
                if socket.send(Message::Ping(Bytes::new())).await.is_err() {
                    return;
                }
                unanswered += 1;
            }
        }
    }
}

/// How often the hub pings a dashboard's socket.
const PING_EVERY: Duration = Duration::from_secs(20);

/// Pings in a row without an answer after which the socket counts as gone.
const MISSED_PINGS: u32 = 2;

/// The `data` of each server-sent event in a body; comments (keep-alives)
/// and other fields are dropped, multi-line data joined with `\n`.
pub(crate) fn sse_data<S, E>(body: S) -> impl Stream<Item = String> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Send + 'static,
{
    let mut buf = BytesMut::new();
    let mut body = Box::pin(body);
    async_stream(move |tx| async move {
        // How far `buf` is known to hold no event's end; a separator may
        // straddle two chunks, so the next search starts 3 bytes before.
        let mut scanned = 0usize;
        while let Some(Ok(chunk)) = body.next().await {
            buf.extend_from_slice(&chunk);
            loop {
                let from = scanned.saturating_sub(3);
                let Some((len, sep)) = frame_end(&buf[from..]).map(|(l, s)| (l + from, s)) else {
                    scanned = buf.len();
                    break;
                };
                let frame = String::from_utf8_lossy(&buf[..len]).into_owned();
                buf.advance(len + sep);
                scanned = 0;
                if let Some(data) = frame_data(&frame)
                    && tx.send(data).await.is_err()
                {
                    return;
                }
            }
            // A stream that never ends an event would grow the buffer without
            // end; no event of a daemon comes near this.
            if buf.len() > MAX_EVENT {
                return;
            }
        }
    })
}

/// The longest event the bridge holds; a longer one ends the stream, and the
/// dashboard reconnects (and starts with a snapshot).
const MAX_EVENT: usize = 8 * 1024 * 1024;

/// Where the first event in `buf` ends: its length and the blank line's.
fn frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    (0..buf.len()).find_map(|i| {
        if buf[i..].starts_with(b"\r\n\r\n") {
            Some((i, 4))
        } else if buf[i..].starts_with(b"\n\n") {
            Some((i, 2))
        } else {
            None
        }
    })
}

fn frame_data(frame: &str) -> Option<String> {
    let lines: Vec<&str> = frame
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|l| l.strip_prefix(' ').unwrap_or(l))
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// A stream fed by a task: `f` gets the sending half of a channel.
fn async_stream<F, Fut>(f: F) -> impl Stream<Item = String> + Send
where
    F: FnOnce(tokio::sync::mpsc::Sender<String>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(f(tx));
    tokio_stream::wrappers::ReceiverStream::new(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn events_are_split_and_keep_alives_dropped() {
        let chunks: Vec<Result<Bytes, std::convert::Infallible>> = vec![
            Ok(Bytes::from_static(b"data: {\"a\":1}\n\n:\n\nda")),
            Ok(Bytes::from_static(
                b"ta: {\"b\":2}\r\n\r\nevent: x\ndata: one\ndata: two\n\n",
            )),
        ];
        let got: Vec<String> = sse_data(tokio_stream::iter(chunks)).collect().await;
        assert_eq!(got, vec!["{\"a\":1}", "{\"b\":2}", "one\ntwo"]);
    }

    #[tokio::test]
    async fn a_separator_split_across_chunks_still_ends_the_event() {
        let chunks: Vec<Result<Bytes, std::convert::Infallible>> = vec![
            Ok(Bytes::from_static(b"data: a\r\n\r")),
            Ok(Bytes::from_static(b"\n")),
            Ok(Bytes::from_static(b"data: b\n")),
            Ok(Bytes::from_static(b"\n")),
        ];
        let got: Vec<String> = sse_data(tokio_stream::iter(chunks)).collect().await;
        assert_eq!(got, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn an_event_that_never_ends_ends_the_stream() {
        let big = vec![b'x'; MAX_EVENT + 1];
        let chunks: Vec<Result<Bytes, std::convert::Infallible>> = vec![
            Ok(Bytes::from_static(b"data: ")),
            Ok(Bytes::from(big)),
            Ok(Bytes::from_static(b"\n\ndata: later\n\n")),
        ];
        let got: Vec<String> = sse_data(tokio_stream::iter(chunks)).collect().await;
        assert!(got.is_empty());
    }
}
