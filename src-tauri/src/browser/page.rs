//! A task's tab, as the tools act on it: one call to Chrome per step, each
//! awaited, so an agent's actions happen in the order it made them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::cdp::Cdp;
use super::{keys, snapshot, Browser, Viewport};
use crate::error::{Error, Result};

/// How long an action waits for the page it started to load.
const LOAD_WAIT: Duration = Duration::from_secs(10);

pub struct Page {
    cdp: Arc<Cdp>,
    session: String,
    pub task: String,
}

impl Page {
    pub(super) fn new(cdp: Arc<Cdp>, session: String, task: String) -> Self {
        Self { cdp, session, task }
    }

    pub fn send(&self, method: &str, params: Value) {
        self.cdp.send(method, params, Some(&self.session));
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.cdp.call(method, params, Some(&self.session)).await.map_err(stale)
    }

    /// Run `expression` in the page and return its value.
    pub async fn eval(&self, expression: &str) -> Result<Value> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true, "userGesture": true }),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let text = ex
                .pointer("/exception/description")
                .and_then(Value::as_str)
                .or_else(|| ex.get("text").and_then(Value::as_str))
                .unwrap_or("the script threw");
            return Err(Error::Other(text.to_string()));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// The address and title of the page as it is now, not as last heard.
    pub async fn location(&self) -> Result<(String, String)> {
        let v = self.eval("({ u: location.href, t: document.title })").await?;
        let get = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        Ok((get("u"), get("t")))
    }

    /// Open `url`, failing with Chrome's reason when it cannot be reached.
    pub async fn navigate(&self, url: &str) -> Result<()> {
        let r = self.call("Page.navigate", json!({ "url": url })).await?;
        match r.get("errorText").and_then(Value::as_str).filter(|e| !e.is_empty()) {
            Some(e) => Err(Error::Other(format!("could not open {url}: {e}"))),
            None => Ok(()),
        }
    }

    /// Back one page in the tab's history. False when there is none.
    pub async fn back(&self) -> Result<bool> {
        self.history(-1).await
    }

    pub async fn forward(&self) -> Result<bool> {
        self.history(1).await
    }

    async fn history(&self, by: i64) -> Result<bool> {
        let h = self.call("Page.getNavigationHistory", json!({})).await?;
        let at = h.get("currentIndex").and_then(Value::as_i64).unwrap_or(0) + by;
        let entry = usize::try_from(at)
            .ok()
            .and_then(|i| h.get("entries")?.as_array()?.get(i)?.get("id")?.as_i64());
        let Some(id) = entry else { return Ok(false) };
        self.call("Page.navigateToHistoryEntry", json!({ "entryId": id })).await?;
        Ok(true)
    }

    /// Wait for what the last action started: a page load, if one began,
    /// up to `LOAD_WAIT`. A page that never finishes loading is reported as
    /// it is then, not as an error.
    pub async fn settle(&self, browser: &Browser) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let deadline = Instant::now() + LOAD_WAIT;
        while Instant::now() < deadline {
            // The page's scripts wait on a dialog, and so would this.
            if browser.dialog(&self.task).is_some() {
                return;
            }
            let loading = browser.state_of(&self.task).is_some_and(|s| s.2);
            if !loading {
                if let Ok(Value::String(s)) = self.eval("document.readyState").await {
                    if s != "loading" {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn outline(&self) -> Result<String> {
        let tree = self.call("Accessibility.getFullAXTree", json!({})).await?;
        let nodes = tree.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(snapshot::outline(&nodes))
    }

    /// Where to click an element: the middle of it, scrolled into view.
    pub async fn center(&self, node: i64) -> Result<(f64, f64)> {
        self.call("DOM.scrollIntoViewIfNeeded", json!({ "backendNodeId": node })).await?;
        let q = self.call("DOM.getContentQuads", json!({ "backendNodeId": node })).await?;
        let quad: Vec<f64> = q
            .pointer("/quads/0")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if quad.len() < 8 {
            return Err(Error::Other(format!("e{node} is not visible: it has no size on the page")));
        }
        let x = (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0;
        let y = (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0;
        Ok((x, y))
    }

    pub async fn click(&self, (x, y): (f64, f64), button: &str, count: u32) -> Result<()> {
        self.mouse("mouseMoved", (x, y), "none", 0).await?;
        for n in 1..=count {
            self.mouse("mousePressed", (x, y), button, n).await?;
            self.mouse("mouseReleased", (x, y), button, n).await?;
        }
        Ok(())
    }

    pub async fn hover(&self, at: (f64, f64)) -> Result<()> {
        self.mouse("mouseMoved", at, "none", 0).await
    }

    async fn mouse(&self, kind: &str, (x, y): (f64, f64), button: &str, count: u32) -> Result<()> {
        self.call(
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": x, "y": y, "button": button, "clickCount": count }),
        )
        .await
        .map(|_| ())
    }

    pub async fn wheel(&self, (x, y): (f64, f64), dy: f64) -> Result<()> {
        self.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": dy }),
        )
        .await
        .map(|_| ())
    }

    pub async fn key(&self, spec: &str) -> Result<()> {
        for event in keys::press(spec)? {
            self.call("Input.dispatchKeyEvent", event).await?;
        }
        Ok(())
    }

    /// Text typed at the caret, as an input method would: one event, so a
    /// long text is not a thousand keystrokes.
    pub async fn insert_text(&self, text: &str) -> Result<()> {
        self.call("Input.insertText", json!({ "text": text })).await.map(|_| ())
    }

    pub async fn focus(&self, node: i64) -> Result<()> {
        self.call("DOM.focus", json!({ "backendNodeId": node })).await.map(|_| ())
    }

    /// Call `function` with the element as `this`, returning its value.
    pub async fn call_on(&self, node: i64, function: &str, args: Vec<Value>) -> Result<Value> {
        let resolved = self.call("DOM.resolveNode", json!({ "backendNodeId": node })).await?;
        let object = resolved
            .pointer("/object/objectId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other(format!("e{node} is not on the page any more: take a new browser_snapshot")))?;
        let args: Vec<Value> = args.into_iter().map(|v| json!({ "value": v })).collect();
        let r = self
            .call(
                "Runtime.callFunctionOn",
                json!({ "objectId": object, "functionDeclaration": function, "arguments": args, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let text = ex.pointer("/exception/description").and_then(Value::as_str).unwrap_or("the script threw");
            return Err(Error::Other(text.to_string()));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// The visible part of the page as a JPEG, base64, at one pixel per CSS
    /// pixel however sharp the panel shows it: an image costs an agent's
    /// context by its size.
    pub async fn screenshot(&self, v: Viewport) -> Result<String> {
        // A clip is measured from the top of the document, not the window:
        // without the scroll, a scrolled page came back as its top.
        let m = self.call("Page.getLayoutMetrics", json!({})).await?;
        let at = |k: &str| m.pointer(&format!("/cssVisualViewport/{k}")).and_then(Value::as_f64).unwrap_or(0.0);
        let r = self
            .call(
                "Page.captureScreenshot",
                json!({
                    "format": "jpeg",
                    "quality": 80,
                    "clip": { "x": at("pageX"), "y": at("pageY"), "width": v.width, "height": v.height, "scale": 1.0 / v.scale.max(1.0) },
                }),
            )
            .await?;
        r.get("data")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Other("the browser returned no image".into()))
    }
}

/// Chrome's word for an element that went away, in the agent's terms.
fn stale(e: Error) -> Error {
    let text = e.to_string();
    if text.contains("No node with given id") || text.contains("Could not find node") || text.contains("No node found") {
        return Error::Other("that element is not on the page any more: take a new browser_snapshot".into());
    }
    e
}
