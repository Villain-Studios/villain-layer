//! Phone access (PHONE-*): the tasks and their agents, from a phone, while
//! everything still runs here.
//!
//! A web server of its own, apart from the MCP server. That one is the
//! agents' way to Jira, GitHub and Slack and stays on loopback; this one is
//! on the network, so it offers only what a phone needs: the overview, a
//! pane's output, and typing into agents. It runs only while one of the two
//! ways in is switched on, and admits only callers from that way (`net.rs`)
//! holding a paired phone's token (`pair.rs`).

mod input;
mod net;
mod pair;
mod routes;
mod view;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Listener, Manager, Runtime};

use crate::commands::AppState;
use crate::config::PhoneDevice;
use crate::error::{Error, Result};
use crate::secrets;

pub use net::Address;
use net::Reach;
use pair::{Attempt, Pairing};

/// A fixed port, so the address saved on the phone keeps working. The dev
/// build has its own, so it can run beside the installed app.
fn port() -> u16 {
    if tauri::is_dev() {
        7421
    } else {
        7420
    }
}

/// A phone's token, as the keychain keeps it.
fn token_key(device: &str) -> String {
    format!("phone:{device}")
}

#[derive(Default)]
struct Inner {
    reach: Reach,
    typing: bool,
    /// Paired phones' tokens, by phone, read from the keychain once.
    tokens: HashMap<String, String>,
    names: HashMap<String, String>,
    loaded: bool,
    pairing: Option<Pairing>,
    server: Option<Server>,
    /// Why the server is not listening although a way in is on.
    error: Option<String>,
    /// Streams open, by phone: what the top bar's phone counts.
    open: HashMap<String, usize>,
}

struct Server {
    stop: tokio::sync::watch::Sender<bool>,
    task: tauri::async_runtime::JoinHandle<()>,
}

struct Phone {
    inner: Mutex<Inner>,
    /// Rung when anything the overview shows may have changed, and when a
    /// phone was forgotten, so its streams look again and end.
    changes: tokio::sync::watch::Sender<()>,
    /// Applying the settings, one at a time: two at once each bound a server.
    applying: tokio::sync::Mutex<()>,
}

fn phone() -> &'static Phone {
    static PHONE: OnceLock<Phone> = OnceLock::new();
    PHONE.get_or_init(|| Phone {
        inner: Mutex::new(Inner::default()),
        changes: tokio::sync::watch::Sender::new(()),
        applying: tokio::sync::Mutex::new(()),
    })
}

impl Phone {
    fn reach(&self) -> Reach {
        self.inner.lock().reach
    }

    fn typing(&self) -> bool {
        self.inner.lock().typing
    }

    /// The paired phone a token belongs to, and its name.
    fn holder(&self, token: &str) -> Option<(String, String)> {
        let inner = self.inner.lock();
        let id = pair::holder(&inner.tokens, token)?.to_string();
        let name = inner.names.get(&id).cloned().unwrap_or_default();
        Some((id, name))
    }

    fn knows(&self, device: &str) -> bool {
        self.inner.lock().tokens.contains_key(device)
    }

    fn changed(&self) {
        self.changes.send_modify(|_| {});
    }

    /// The running server's stop signal, for a stream to end with it.
    fn stopping(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.inner.lock().server.as_ref().map(|s| s.stop.subscribe())
    }
}

/// A stream a phone holds open, counted while it lasts.
struct Open<R: Runtime> {
    app: AppHandle<R>,
    device: String,
}

impl<R: Runtime> Open<R> {
    fn new(app: &AppHandle<R>, device: &str) -> Self {
        *phone().inner.lock().open.entry(device.to_string()).or_default() += 1;
        let _ = app.emit("phone:changed", ());
        Self { app: app.clone(), device: device.to_string() }
    }
}

impl<R: Runtime> Drop for Open<R> {
    fn drop(&mut self) {
        {
            let mut inner = phone().inner.lock();
            if let Some(n) = inner.open.get_mut(&self.device) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    inner.open.remove(&self.device);
                }
            }
        }
        let _ = self.app.emit("phone:changed", ());
    }
}

/// Start, stop or adjust the server to match the settings. At launch, and
/// after every change to them.
pub async fn apply(app: AppHandle) {
    let p = phone();
    let _one = p.applying.lock().await;
    let cfg = app.state::<AppState>().config.read().phone;
    let reach = Reach { tailscale: cfg.tailscale, home: cfg.home };

    // The keychain only once a way is open: it can ask for a password, and
    // someone who never uses this should never see that.
    let load = reach.any() && !p.inner.lock().loaded;
    if load {
        let ids: Vec<String> = cfg.devices.iter().map(|d| d.id.clone()).collect();
        let read = crate::commands::off_runtime(move || {
            ids.into_iter()
                .filter_map(|id| match secrets::get(&token_key(&id)) {
                    Ok(Some(t)) => Some(Ok((id, t))),
                    Ok(None) => None,
                    Err(e) => Some(Err(e)),
                })
                .collect::<Result<HashMap<String, String>>>()
        })
        .await;
        match read {
            Ok(Ok(tokens)) => {
                let mut inner = p.inner.lock();
                inner.tokens = tokens;
                inner.loaded = true;
            }
            // Tried again at the next change: until then no phone gets in.
            Ok(Err(e)) | Err(e) => crate::commands::notify(
                &app,
                "error",
                format!("Paired phones cannot get in: the keychain would not give up their tokens ({e})."),
            ),
        }
    }

    let stop = {
        let mut inner = p.inner.lock();
        inner.reach = reach;
        inner.typing = cfg.typing;
        inner.names = cfg.devices.iter().map(|d| (d.id.clone(), d.name.clone())).collect();
        if !reach.any() {
            inner.pairing = None;
            inner.error = None;
            inner.server.take()
        } else {
            None
        }
    };
    if let Some(server) = stop {
        server.stop.send_replace(true);
        // Its streams end on the signal, so this is quick; the port is free
        // again for the next time it is switched on.
        let _ = tokio::time::timeout(Duration::from_secs(3), server.task).await;
    }

    let start = reach.any() && p.inner.lock().server.is_none();
    if start {
        listen_for_changes(&app);
        let bound = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, port())).await;
        match bound {
            Ok(listener) => {
                let (stop, mut stopped) = tokio::sync::watch::channel(false);
                let router = routes::router(app.clone());
                let task = tauri::async_runtime::spawn(async move {
                    let served = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
                        .with_graceful_shutdown(async move {
                            let _ = stopped.wait_for(|s| *s).await;
                        })
                        .await;
                    if let Err(e) = served {
                        eprintln!("villain-layer phone server stopped: {e}");
                    }
                });
                let mut inner = p.inner.lock();
                inner.server = Some(Server { stop, task });
                inner.error = None;
            }
            Err(e) => {
                let why = format!("Port {} is in use by something else, so phones cannot reach the app ({e}).", port());
                p.inner.lock().error = Some(why.clone());
                crate::commands::notify(&app, "error", why);
            }
        }
    }
    p.changed();
    let _ = app.emit("phone:changed", ());
}

/// Phones look again: something their page shows changed outside the panes
/// (the theme, SET-5).
pub fn changed() {
    phone().changed();
}

/// The overview changes when a pane does. Registered once, when the server
/// first starts.
fn listen_for_changes(app: &AppHandle) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for event in ["pty:activity", "pty:exit", "pty:notice"] {
            app.listen_any(event, |_| phone().changed());
        }
    });
}

#[derive(Debug, Serialize)]
pub struct PhoneStatus {
    pub tailscale: bool,
    pub home: bool,
    pub typing: bool,
    pub port: u16,
    pub listening: bool,
    pub error: Option<String>,
    /// Where a phone can reach this Mac, on the ways that are on.
    pub addresses: Vec<Address>,
    pub devices: Vec<DeviceView>,
    pub pairing: Option<PairingView>,
}

#[derive(Debug, Serialize)]
pub struct DeviceView {
    #[serde(flatten)]
    pub device: PhoneDevice,
    /// Has the overview or a terminal open right now.
    pub connected: bool,
}

#[derive(Debug, Serialize)]
pub struct PairingView {
    pub code: String,
    pub seconds_left: u64,
}

pub fn status(state: &AppState) -> PhoneStatus {
    let cfg = state.config.read().phone;
    let reach = Reach { tailscale: cfg.tailscale, home: cfg.home };
    let mut inner = phone().inner.lock();
    let now = Instant::now();
    if inner.pairing.as_ref().is_some_and(|p| !p.live(now)) {
        inner.pairing = None;
    }
    PhoneStatus {
        tailscale: cfg.tailscale,
        home: cfg.home,
        typing: cfg.typing,
        port: port(),
        listening: inner.server.is_some(),
        error: inner.error.clone(),
        addresses: net::addresses()
            .into_iter()
            .filter(|a| match a.way {
                net::Way::Tailscale => reach.tailscale,
                net::Way::Home => reach.home,
            })
            .collect(),
        devices: cfg
            .devices
            .into_iter()
            .map(|d| DeviceView { connected: inner.open.contains_key(&d.id), device: d })
            .collect(),
        pairing: inner.pairing.as_ref().map(|p| PairingView {
            code: p.code().to_string(),
            seconds_left: p.left(now).as_secs(),
        }),
    }
}

/// Show a new code to pair a phone with, replacing any showing.
pub fn start_pairing(app: &AppHandle) -> Result<PairingView> {
    let view = {
        let mut inner = phone().inner.lock();
        if inner.server.is_none() {
            return Err(Error::Other("Turn on Tailscale or the home network first, so the phone can reach the app.".into()));
        }
        let now = Instant::now();
        let p = Pairing::new(now);
        let view = PairingView { code: p.code().to_string(), seconds_left: p.left(now).as_secs() };
        inner.pairing = Some(p);
        view
    };
    let _ = app.emit("phone:changed", ());
    Ok(view)
}

/// A phone typed a code. Its token, when it was the one showing.
fn try_pair(code: &str) -> Attempt {
    let mut inner = phone().inner.lock();
    let now = Instant::now();
    let Some(pairing) = inner.pairing.as_mut() else {
        return Attempt::NoCode;
    };
    let attempt = pairing.attempt(code, now);
    if !pairing.live(now) {
        inner.pairing = None;
    }
    attempt
}

/// Keep a newly paired phone: its token in the keychain, its name in the
/// config. Blocking: the keychain can stop and ask.
fn keep(state: &AppState, device: PhoneDevice, token: String) -> Result<()> {
    secrets::set(&token_key(&device.id), &token)?;
    let (id, name) = (device.id.clone(), device.name.clone());
    state.config.update(|c| c.phone.devices.push(device))?;
    let mut inner = phone().inner.lock();
    inner.tokens.insert(id.clone(), token);
    inner.names.insert(id, name);
    Ok(())
}

/// Forget a phone: its token stops working at once, and its open streams end.
/// Blocking, for the keychain.
pub fn forget(state: &AppState, device: &str) -> Result<()> {
    state.config.update(|c| c.phone.devices.retain(|d| d.id != device))?;
    {
        let mut inner = phone().inner.lock();
        inner.tokens.remove(device);
        inner.names.remove(device);
    }
    phone().changed();
    secrets::delete(&token_key(device))
}
