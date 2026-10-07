//! The Chrome DevTools Protocol over Chrome's pipe: JSON messages, each
//! ended by a NUL byte.
//!
//! A thread writes and a thread reads, so nothing that sends ever waits on
//! Chrome, and an answer is handed to whoever asked through a oneshot the
//! async side awaits. Events go to one handler, on the reading thread.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::error::{Error, Result};

/// Longer than any page load an agent should wait on, short enough that a
/// Chrome that stopped answering is noticed.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// One message Chrome sent without being asked.
#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub params: Value,
    /// The tab it is about, for a tab's events.
    pub session: Option<String>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

pub struct Cdp {
    out: mpsc::Sender<Vec<u8>>,
    pending: Pending,
    next: AtomicU64,
    closed: Arc<AtomicBool>,
}

impl Cdp {
    /// Start the writing and reading threads. `on_event` runs on the reading
    /// thread, so it must not wait; `on_close` runs once, when Chrome's end
    /// closes.
    pub fn start(
        reader: impl Read + Send + 'static,
        mut writer: impl Write + Send + 'static,
        on_event: impl Fn(Event) + Send + 'static,
        on_close: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Self>> {
        let (out, queue) = mpsc::channel::<Vec<u8>>();
        let pending: Pending = Arc::default();
        let closed = Arc::new(AtomicBool::new(false));

        std::thread::Builder::new()
            .name("browser-write".into())
            .spawn(move || {
                for message in queue {
                    if writer.write_all(&message).and_then(|_| writer.flush()).is_err() {
                        break;
                    }
                }
            })?;

        let (waiting, gone) = (pending.clone(), closed.clone());
        std::thread::Builder::new()
            .name("browser-read".into())
            .spawn(move || {
                let mut reader = BufReader::with_capacity(256 * 1024, reader);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(0, &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if buf.last() == Some(&0) {
                        buf.pop();
                    }
                    let Ok(message) = serde_json::from_slice::<Value>(&buf) else {
                        continue;
                    };
                    match message.get("id").and_then(Value::as_u64) {
                        Some(id) => {
                            if let Some(tx) = waiting.lock().remove(&id) {
                                let _ = tx.send(answer(message));
                            }
                        }
                        None => {
                            let Some(method) = message.get("method").and_then(Value::as_str) else {
                                continue;
                            };
                            on_event(Event {
                                method: method.to_string(),
                                params: message.get("params").cloned().unwrap_or(Value::Null),
                                session: message.get("sessionId").and_then(Value::as_str).map(str::to_string),
                            });
                        }
                    }
                }
                gone.store(true, Ordering::Release);
                // Whoever is still waiting hears now, not after the timeout.
                for (_, tx) in waiting.lock().drain() {
                    let _ = tx.send(Err(Error::Other("the browser closed".into())));
                }
                on_close();
            })?;

        Ok(Arc::new(Self { out, pending, next: AtomicU64::new(1), closed }))
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn message(&self, method: &str, params: Value, session: Option<&str>) -> (u64, Vec<u8>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut m = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            m["sessionId"] = json!(s);
        }
        let mut bytes = serde_json::to_vec(&m).unwrap_or_default();
        bytes.push(0);
        (id, bytes)
    }

    /// Send without waiting for the answer: input events, acknowledgements,
    /// anything sent from the reading thread itself, which cannot wait on an
    /// answer it would have to read.
    pub fn send(&self, method: &str, params: Value, session: Option<&str>) {
        let (_, bytes) = self.message(method, params, session);
        let _ = self.out.send(bytes);
    }

    /// Send and wait for the answer.
    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        if self.is_closed() {
            return Err(Error::Other("the browser closed".into()));
        }
        let (id, bytes) = self.message(method, params, session);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        if self.out.send(bytes).is_err() {
            self.pending.lock().remove(&id);
            return Err(Error::Other("the browser closed".into()));
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(Error::Other("the browser closed".into())),
            Err(_) => {
                self.pending.lock().remove(&id);
                Err(Error::Other(format!("the browser did not answer {method} in {}s", CALL_TIMEOUT.as_secs())))
            }
        }
    }
}

/// A reply's result, or its error as one.
fn answer(message: Value) -> Result<Value> {
    if let Some(e) = message.get("error") {
        let text = e.get("message").and_then(Value::as_str).unwrap_or("error");
        return Err(Error::Other(text.to_string()));
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Chrome stand-in: answers every command with its own method name,
    /// and sends one event first.
    fn echo() -> (Arc<Cdp>, mpsc::Receiver<Event>, mpsc::Receiver<()>) {
        let (to_chrome_r, to_chrome_w) = std::io::pipe().unwrap();
        let (from_chrome_r, mut from_chrome_w) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            from_chrome_w.write_all(b"{\"method\":\"Page.loadEventFired\",\"params\":{},\"sessionId\":\"S\"}\0").unwrap();
            let mut r = BufReader::new(to_chrome_r);
            let mut buf = Vec::new();
            while r.read_until(0, &mut buf).unwrap_or(0) > 0 {
                buf.pop();
                let m: Value = serde_json::from_slice(&buf).unwrap();
                let reply = if m["method"] == "Fail.please" {
                    json!({ "id": m["id"], "error": { "message": "no such thing" } })
                } else {
                    json!({ "id": m["id"], "result": { "method": m["method"] } })
                };
                let mut out = serde_json::to_vec(&reply).unwrap();
                out.push(0);
                from_chrome_w.write_all(&out).unwrap();
                buf.clear();
                if m["method"] == "Browser.close" {
                    break;
                }
            }
        });
        let (events_tx, events) = mpsc::channel();
        let (closed_tx, closed) = mpsc::channel();
        let cdp = Cdp::start(
            from_chrome_r,
            to_chrome_w,
            move |e| { let _ = events_tx.send(e); },
            move || { let _ = closed_tx.send(()); },
        )
        .unwrap();
        (cdp, events, closed)
    }

    #[tokio::test]
    async fn each_answer_reaches_the_call_that_asked_and_events_go_to_the_handler() {
        let (cdp, events, _) = echo();
        let (a, b) = tokio::join!(
            cdp.call("Page.enable", json!({}), Some("S")),
            cdp.call("Runtime.enable", json!({}), None),
        );
        assert_eq!(a.unwrap()["method"], "Page.enable");
        assert_eq!(b.unwrap()["method"], "Runtime.enable");
        let e = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((e.method.as_str(), e.session.as_deref()), ("Page.loadEventFired", Some("S")));
    }

    #[tokio::test]
    async fn an_error_from_the_browser_is_the_call_s_error() {
        let (cdp, _, _) = echo();
        let e = cdp.call("Fail.please", json!({}), None).await.unwrap_err();
        assert_eq!(e.to_string(), "no such thing");
    }

    #[tokio::test]
    async fn a_browser_that_goes_away_fails_calls_at_once_and_says_it_closed() {
        let (cdp, _, closed) = echo();
        cdp.call("Browser.close", json!({}), None).await.unwrap();
        closed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(cdp.is_closed());
        let e = cdp.call("Page.enable", json!({}), None).await.unwrap_err();
        assert_eq!(e.to_string(), "the browser closed");
    }
}
