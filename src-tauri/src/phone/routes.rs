//! The phone's HTTP surface: its page, pairing, the overview, a pane's
//! output, and typing. Every route is behind the way-in check; all but the
//! page and pairing are behind a paired phone's token too.
//!
//! Streams are newline-delimited JSON over a plain response, read with
//! `fetch`. `EventSource` cannot send a header, and the token must not ride
//! in a URL, where it lands in logs and history.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, FromRequestParts, Path, Query, Request, State};
use axum::http::{header, request::Parts, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::{input, phone, view, Attempt, Open};
use crate::commands::{blocking, off_runtime, AppState};
use crate::config::PhoneDevice;

/// A line, at least, this often on every stream: a phone that hears nothing
/// for longer takes the connection for dead and opens another, and a stream
/// whose phone has gone is found out on the next write.
const HEARTBEAT: Duration = Duration::from_secs(20);
/// Output is gathered this long before it goes: a phone does not need an
/// agent's thirty repaints a second, and every one is radio time.
const FRAME: Duration = Duration::from_millis(100);

/// Generic over the runtime only so the tests can drive it with a mock app.
pub fn router<R: Runtime>(app: AppHandle<R>) -> Router {
    Router::new()
        .route("/", get(page::<R>))
        .route("/phone.html", get(page::<R>))
        .route("/assets/{file}", get(asset::<R>))
        .route("/api/pair", post(pair::<R>))
        .route("/api/overview", get(overview::<R>))
        .route("/api/changes", get(changes::<R>))
        .route("/api/panes/{id}/output", get(output::<R>))
        .route("/api/panes/{id}/input", post(type_into::<R>))
        .layer(middleware::from_fn(way_in))
        .with_state(app)
}

/// Only callers on a way that is switched on (PHONE-2).
async fn way_in(ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request, next: Next) -> Response {
    if !phone().reach().admits(peer.ip()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, header::HeaderValue::from_static("no-referrer"));
    res
}

fn fail(status: StatusCode, why: impl Into<String>) -> Response {
    (status, Json(json!({ "error": why.into() }))).into_response()
}

/// A paired phone, known by the token it sends (PHONE-3).
struct Device {
    id: String,
    name: String,
}

impl<S: Send + Sync> FromRequestParts<S> for Device {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default();
        match phone().holder(token) {
            Some((id, name)) => Ok(Device { id, name }),
            None => Err(fail(StatusCode::UNAUTHORIZED, "This phone is not paired with the app, or was forgotten.")),
        }
    }
}

// ---------------------------------------------------------------- the page

async fn page<R: Runtime>(State(app): State<AppHandle<R>>) -> Response {
    built(app, "phone.html".into()).await
}

async fn asset<R: Runtime>(State(app): State<AppHandle<R>>, Path(file): Path<String>) -> Response {
    // Only the build's own file names: nothing climbs out of `assets/`.
    let plain = !file.is_empty()
        && !file.starts_with('.')
        && file.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !plain {
        return StatusCode::NOT_FOUND.into_response();
    }
    built(app, format!("assets/{file}")).await
}

/// A file of the frontend's build. The dev build reads `dist/`, so the page
/// is there only after `bun run build`.
async fn built<R: Runtime>(app: AppHandle<R>, path: String) -> Response {
    let html = path.ends_with(".html");
    let found = off_runtime({
        let path = path.clone();
        move || app.asset_resolver().get(path)
    })
    .await
    .ok()
    .flatten()
    // A missing file comes back as the app's index.html.
    .filter(|a| a.mime_type.starts_with("text/html") == html);
    match found {
        Some(a) => {
            // Asset names carry a hash of their content; the page does not.
            let cache = if html { "no-cache" } else { "public, max-age=31536000, immutable" };
            ([(header::CONTENT_TYPE, a.mime_type), (header::CACHE_CONTROL, cache.into())], a.bytes).into_response()
        }
        None => fail(StatusCode::NOT_FOUND, format!("{path} is not in the app's build. In a dev build, run `bun run build` first.")),
    }
}

// ---------------------------------------------------------------- pairing

#[derive(Deserialize)]
struct PairBody {
    code: String,
    name: String,
}

async fn pair<R: Runtime>(State(app): State<AppHandle<R>>, Json(body): Json<PairBody>) -> Response {
    match super::try_pair(&body.code) {
        Attempt::Paired => {}
        Attempt::Wrong => return fail(StatusCode::FORBIDDEN, "That is not the code showing on the Mac."),
        Attempt::NoCode => {
            return fail(
                StatusCode::GONE,
                "No code is showing on the Mac. In the app, open Settings, then Phone, and choose Pair a phone.",
            )
        }
    }
    let name: String = body.name.trim().chars().filter(|c| !c.is_control()).take(40).collect();
    let device = PhoneDevice {
        id: uuid::Uuid::new_v4().to_string(),
        name: if name.is_empty() { "Phone".into() } else { name },
        paired_at: Utc::now(),
    };
    let token = super::pair::mint();
    let reply = json!({ "token": token, "device": device.id, "name": device.name });
    let kept = blocking(app.clone(), move |state| super::keep(state, device, token)).await;
    let _ = app.emit("phone:changed", ());
    match kept {
        Ok(()) => Json(reply).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, format!("The Mac could not keep the pairing: {e}")),
    }
}

// ---------------------------------------------------------------- reading

async fn overview<R: Runtime>(State(app): State<AppHandle<R>>, device: Device) -> Response {
    let typing = phone().typing();
    let groups = blocking(app, |state| {
        let tasks = state.config.read().tasks;
        Ok(view::groups(&tasks, state.ptys.list(None)))
    })
    .await;
    match groups {
        Ok(groups) => Json(view::Overview { device: device.name, typing, groups }).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// A response that is a stream of JSON lines, written by `fill` until it
/// returns or the phone goes away.
fn lines<F, Fut>(fill: F) -> Response
where
    F: FnOnce(Lines) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(8);
    tauri::async_runtime::spawn(fill(Lines(tx)));
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|line| (Ok::<_, Infallible>(line), rx))
    });
    (
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

struct Lines(tokio::sync::mpsc::Sender<String>);

impl Lines {
    /// False once the phone has gone.
    async fn send(&self, v: Value) -> bool {
        self.0.send(format!("{v}\n")).await.is_ok()
    }

    async fn beat(&self) -> bool {
        self.0.send("\n".into()).await.is_ok()
    }
}

/// Wait for the server to stop. Forever, when there is none to stop.
async fn stopped(rx: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    match rx {
        Some(rx) => {
            let _ = rx.wait_for(|s| *s).await;
        }
        None => std::future::pending().await,
    }
}

/// A line whenever the overview may have changed; the phone asks for it again.
async fn changes<R: Runtime>(State(app): State<AppHandle<R>>, device: Device) -> Response {
    let mut changes = phone().changes.subscribe();
    let mut stop = phone().stopping();
    lines(move |out| async move {
        let _open = Open::new(&app, &device.id);
        changes.borrow_and_update();
        loop {
            tokio::select! {
                r = changes.changed() => {
                    if r.is_err() {
                        return;
                    }
                    // A burst of state changes is one look.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    changes.borrow_and_update();
                    if !phone().knows(&device.id) || !out.send(json!({ "changed": true })).await {
                        return;
                    }
                }
                _ = tokio::time::sleep(HEARTBEAT) => {
                    if !out.beat().await {
                        return;
                    }
                }
                _ = stopped(&mut stop) => return,
            }
        }
    })
}

#[derive(Deserialize)]
struct Since {
    from: Option<u64>,
}

/// A pane's output (PHONE-6): its size, then what it printed after `from`
/// (all its scrollback without one), then everything as it comes.
async fn output<R: Runtime>(State(app): State<AppHandle<R>>, device: Device, Path(id): Path<String>, Query(q): Query<Since>) -> Response {
    let state = app.state::<AppState>();
    let Ok(mut feed) = state.ptys.feed(&id) else {
        return fail(StatusCode::NOT_FOUND, "That terminal is closed.");
    };
    let seen = |app: &AppHandle<R>, id: &str| {
        if app.state::<AppState>().ptys.seen_elsewhere(id).unwrap_or(false) {
            let _ = app.emit("pty:activity", id);
        }
    };
    seen(&app, &id);
    let mut changes = phone().changes.subscribe();
    let mut stop = phone().stopping();
    lines(move |out| async move {
        let _open = Open::new(&app, &device.id);
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut size = feed.size();
        let mut at = q.from;
        if !out.send(json!({ "size": [size.0, size.1] })).await {
            return;
        }
        loop {
            let exited = feed.exited();
            if exited.is_some() {
                // The last of its output can be read after the exit is.
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            let chunk = feed.since(at);
            if !chunk.data.is_empty() || chunk.reset {
                let line = json!({ "data": b64.encode(&chunk.data), "end": chunk.end, "reset": chunk.reset });
                if !out.send(line).await {
                    break;
                }
            }
            at = Some(chunk.end);
            if feed.size() != size {
                size = feed.size();
                if !out.send(json!({ "size": [size.0, size.1] })).await {
                    break;
                }
            }
            if let Some(code) = exited {
                let _ = out.send(json!({ "exit": code })).await;
                break;
            }
            tokio::select! {
                _ = feed.printed() => tokio::time::sleep(FRAME).await,
                r = changes.changed() => {
                    if r.is_err() || !phone().knows(&device.id) {
                        break;
                    }
                }
                _ = tokio::time::sleep(HEARTBEAT) => {
                    if !out.beat().await {
                        break;
                    }
                }
                _ = stopped(&mut stop) => break,
            }
        }
        // Looked at until now: what it finished meanwhile has been seen.
        seen(&app, &id);
    })
}

// ---------------------------------------------------------------- typing

#[derive(Deserialize)]
struct Typed {
    text: Option<String>,
    key: Option<String>,
}

/// A line of text and Enter, or one key of the key bar, into an agent
/// (PHONE-7).
async fn type_into<R: Runtime>(State(app): State<AppHandle<R>>, _device: Device, Path(id): Path<String>, Json(t): Json<Typed>) -> Response {
    let state = app.state::<AppState>();
    let Ok(pane) = state.ptys.info(&id) else {
        return fail(StatusCode::NOT_FOUND, "That terminal is closed.");
    };
    if let Some(why) = input::refused(phone().typing(), &pane) {
        return fail(StatusCode::FORBIDDEN, why);
    }
    let sent = match (t.text, t.key) {
        (Some(text), None) => match input::line(&text) {
            Ok(line) => state.ptys.submit(&id, &line).map(|_| false),
            Err(why) => return fail(StatusCode::BAD_REQUEST, why),
        },
        (None, Some(name)) => match input::key(&name) {
            Some(bytes) => state.ptys.write(&id, bytes),
            None => return fail(StatusCode::BAD_REQUEST, format!("No key called {name}.")),
        },
        _ => return fail(StatusCode::BAD_REQUEST, "Send text or a key, one of them."),
    };
    match sent {
        Ok(changed) => {
            if changed {
                let _ = app.emit("pty:activity", &id);
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => fail(StatusCode::CONFLICT, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ConfigStore, Task};
    use crate::phone::net::Reach;
    use crate::pty::{PaneKind, SpawnOptions};
    use axum::extract::connect_info::MockConnectInfo;

    fn state(root: &std::path::Path, tasks: Vec<Task>) -> AppState {
        let cfg = AppConfig { tasks, ..Default::default() };
        AppState {
            config: ConfigStore::for_tests(root.join("config.json"), cfg),
            ptys: crate::pty::PtyManager::default(),
            jira_types: Default::default(),
            epic_field_missing: Default::default(),
            pending_notices: Default::default(),
            status_cache: Default::default(),
            news: Default::default(),
            messages: crate::messages::Messages::for_tests(root.join("messages.json")),
            notes: crate::notes::Notes::load(root),
            browser: Default::default(),
        }
    }

    /// Lines from a stream until one holds `text`.
    async fn read_until(res: &mut reqwest::Response, text: &str) -> String {
        let mut seen = String::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !seen.contains(text) {
                match res.chunk().await.unwrap() {
                    Some(bytes) => {
                        for line in String::from_utf8_lossy(&bytes).lines().filter(|l| !l.trim().is_empty()) {
                            let v: Value = serde_json::from_str(line).unwrap();
                            match v["data"].as_str() {
                                Some(data) => seen.push_str(&String::from_utf8_lossy(
                                    &base64::engine::general_purpose::STANDARD.decode(data).unwrap(),
                                )),
                                None => seen.push_str(line),
                            }
                        }
                    }
                    None => panic!("the stream ended before {text:?}; it had {seen:?}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never saw {text:?}; saw {seen:?}"));
        seen
    }

    /// The whole way, over HTTP: let in by way and token, the overview, a
    /// pane followed and typed into, and each refusal.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_paired_phone_on_an_open_way_sees_the_tasks_follows_an_agent_and_types_into_it() {
        let root = std::env::temp_dir().join(format!("vl-phone-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let task = Task {
            id: "t1".into(),
            name: "Fix the login race".into(),
            root: root.to_string_lossy().to_string(),
            branch: "t1".into(),
            issue_key: Some("ACME-1".into()),
            issue_url: None,
            created_at: Utc::now(),
            ticket_stage: None,
            chat: None,
            review: None,
            browser_url: None,
        };
        let app = tauri::test::mock_app();
        app.manage(state(&root, vec![task]));
        let ptys = &app.state::<AppState>().ptys;
        let spawn = |kind: PaneKind| {
            ptys.spawn(
                app.handle(),
                SpawnOptions {
                    task_id: "t1".into(),
                    checkout_id: None,
                    cwd: root.to_string_lossy().to_string(),
                    kind,
                    title: "Agent".into(),
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "printf ready; while read l; do printf 'got:%s;' \"$l\"; done".into()],
                    agent_id: Some("claude".into()),
                    rows: Some(24),
                    cols: Some(80),
                    initial_input: None,
                    prompted: false,
                    env: Vec::new(),
                    title_activity: None,
                    title_topic: None,
                },
            )
            .unwrap()
            .id
        };
        let agent = spawn(PaneKind::Agent);
        let shell = spawn(PaneKind::Shell);

        let token = super::super::pair::mint();
        {
            let mut inner = phone().inner.lock();
            inner.reach = Reach { tailscale: false, home: true };
            inner.typing = true;
            inner.tokens.insert("ph".into(), token.clone());
            inner.names.insert("ph".into(), "iPhone".into());
        }

        // Every request comes from a phone on the home network.
        let router = router(app.handle().clone()).layer(MockConnectInfo(SocketAddr::from(([192, 168, 8, 23], 50_000))));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await });
        let http = reqwest::Client::new();
        let get = |path: &str, token: Option<&str>| {
            let mut req = http.get(format!("{base}{path}"));
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            req.send()
        };
        let post = |path: &str, body: Value| http.post(format!("{base}{path}")).bearer_auth(&token).json(&body).send();

        assert_eq!(get("/api/overview", None).await.unwrap().status(), 401, "no token, no way in");
        assert_eq!(get("/api/overview", Some(&super::super::pair::mint())).await.unwrap().status(), 401);
        let overview: Value = get("/api/overview", Some(&token)).await.unwrap().json().await.unwrap();
        assert_eq!(overview["device"], "iPhone");
        assert_eq!(overview["groups"][0]["key"], "ACME-1");
        assert_eq!(overview["groups"][0]["panes"][0]["id"], agent.as_str());

        let wrong = http.post(format!("{base}/api/pair")).json(&json!({ "code": "000000", "name": "x" })).send().await.unwrap();
        assert_eq!(wrong.status(), 410, "no code showing, nothing to pair with");

        let mut out = get(&format!("/api/panes/{agent}/output"), Some(&token)).await.unwrap();
        assert_eq!(out.status(), 200);
        let first = read_until(&mut out, "ready").await;
        assert!(first.contains(r#""size":[24,80]"#), "drawn at the window's size: {first}");

        let sent = post(&format!("/api/panes/{agent}/input"), json!({ "text": "run the tests\nnow" })).await.unwrap();
        assert_eq!(sent.status(), 204);
        read_until(&mut out, "got:run the tests now;").await;
        assert_eq!(post(&format!("/api/panes/{agent}/input"), json!({ "key": "enter" })).await.unwrap().status(), 204);
        read_until(&mut out, "got:;").await;

        assert_eq!(post(&format!("/api/panes/{agent}/input"), json!({ "key": "rm" })).await.unwrap().status(), 400);
        assert_eq!(post(&format!("/api/panes/{shell}/input"), json!({ "text": "ls" })).await.unwrap().status(), 403, "never a shell");
        phone().inner.lock().typing = false;
        assert_eq!(post(&format!("/api/panes/{agent}/input"), json!({ "text": "hi" })).await.unwrap().status(), 403, "typing off");
        assert_eq!(get("/api/panes/nope/output", Some(&token)).await.unwrap().status(), 404);

        // The home network switched off: the same phone is turned away.
        phone().inner.lock().reach = Reach { tailscale: true, home: false };
        assert_eq!(get("/api/overview", Some(&token)).await.unwrap().status(), 403);

        *phone().inner.lock() = Default::default();
        ptys.shutdown(Duration::from_millis(200));
        let _ = std::fs::remove_dir_all(&root);
    }
}
