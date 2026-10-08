//! The protocol, as the app's side of it: what the agent sends, and what
//! the app sends back. Runs nothing itself: it is handed the agent's lines
//! (`process.rs` reads them) and puts what to send on a channel. What the
//! user does to a conversation is in `user.rs`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::conversation::Conversation;
use super::{Launch, Mcp, OnSession, Pane, Pick};
use crate::pty::Activity;

/// The protocol version the app speaks.
const PROTOCOL_VERSION: u64 = 1;

/// What a request the app sent is waiting for.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Waiting {
    Initialize,
    List,
    Open(Opening),
    Prompt,
    Setting,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Opening {
    New,
    Load(String),
    Resume(String),
}

/// A permission question the agent is waiting on.
pub(super) struct Question {
    /// The id of the agent's request, which the answer goes back under.
    pub(super) rpc: Value,
    pub(super) entry: usize,
    pub(super) options: Vec<String>,
}

#[derive(Default)]
struct Caps {
    load: bool,
    resume: bool,
    list: bool,
    http: bool,
    /// How the agent says to sign in, for a session it will not open.
    sign_in: Vec<String>,
}

pub(super) struct State {
    pub(super) convo: Conversation,
    pub(super) cwd: String,
    pub(super) mcp: Option<Mcp>,
    pub(super) pick: Pick,
    caps: Caps,
    pub(super) agent: Option<String>,
    pub(super) session: Option<String>,
    pub(super) waiting: HashMap<u64, Waiting>,
    pub(super) questions: Vec<Question>,
    pub(super) busy: bool,
    pub(super) ready: bool,
    pub(super) exited: bool,
    /// Prompts waiting to be sent, with their entries. One given before the
    /// conversation opened has none yet: a conversation being picked up
    /// replays itself first, and the prompt goes after it, not into it.
    pub(super) queue: VecDeque<(String, Option<usize>)>,
}

impl State {
    /// What was waiting will not be sent: shown as not sent, so the text is
    /// still there to copy.
    pub(super) fn drop_queue(&mut self) {
        for (text, entry) in std::mem::take(&mut self.queue) {
            let entry = entry.unwrap_or_else(|| self.convo.user(&text, false));
            self.convo.dequeue(entry, true);
        }
    }
}

/// What handling something decided, done once the lock is let go.
#[derive(Default)]
pub(super) struct Effects {
    pub(super) send: Vec<Value>,
    pub(super) report: Option<Activity>,
    pub(super) topic: Option<String>,
    pub(super) session: Option<String>,
    pub(super) failed: Option<String>,
    pub(super) text: String,
    pub(super) changed: bool,
}

/// One ACP agent's side of the protocol, as the app sees it.
pub struct Conn {
    pane_id: String,
    out: std::sync::mpsc::Sender<String>,
    next: AtomicU64,
    pub(super) state: Mutex<State>,
    pane: OnceLock<Box<dyn Pane>>,
    pub(super) stderr: Mutex<String>,
    on_session: Option<OnSession>,
}

impl Conn {
    /// A connection that writes its lines to `out`. Nothing is sent until
    /// `begin`.
    pub(super) fn new(launch: Launch, cwd: &str, pane_id: &str, out: std::sync::mpsc::Sender<String>) -> Conn {
        let convo = Conversation::default();
        let mut queue = VecDeque::new();
        if let Some(prompt) = launch.prompt.filter(|p| !p.trim().is_empty()) {
            queue.push_back((prompt, None));
        }
        Conn {
            pane_id: pane_id.to_string(),
            out,
            next: AtomicU64::new(1),
            state: Mutex::new(State {
                convo,
                cwd: cwd.to_string(),
                mcp: launch.mcp,
                pick: launch.pick,
                caps: Caps::default(),
                agent: None,
                session: None,
                waiting: HashMap::new(),
                questions: Vec::new(),
                busy: false,
                ready: false,
                exited: false,
                queue,
            }),
            pane: OnceLock::new(),
            stderr: Mutex::new(String::new()),
            on_session: launch.on_session,
        }
    }

    /// Say hello: `initialize`, and from its answer the rest follows.
    pub(super) fn begin(&self, pane: Box<dyn Pane>) {
        let _ = self.pane.set(pane);
        self.with(|st, fx| {
            let params = json!({
                "protocolVersion": PROTOCOL_VERSION,
                // Agents read, write and run things themselves, as they do
                // in a terminal: the app takes on none of it.
                "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
                "clientInfo": { "name": "villain-layer", "title": "Villain Layer", "version": env!("CARGO_PKG_VERSION") },
            });
            self.request(st, fx, "initialize", params, Waiting::Initialize);
            fx.report = Some(Activity::Idle);
        });
    }

    /// Run `f` under the lock, then carry out what it decided outside it.
    pub(super) fn with<T>(&self, f: impl FnOnce(&mut State, &mut Effects) -> T) -> T {
        let mut fx = Effects::default();
        let out = {
            let mut st = self.state.lock();
            let rev = st.convo.rev();
            let out = f(&mut st, &mut fx);
            fx.text = st.convo.take_text();
            fx.changed |= st.convo.rev() != rev;
            out
        };
        for msg in fx.send {
            let _ = self.out.send(msg.to_string());
        }
        if let Some(pane) = self.pane.get() {
            if !fx.text.is_empty() {
                pane.print(&fx.text);
            }
            // The pane first, then whoever remembers it: one of the two is
            // sure to see the other (`commands::remember_pane`).
            if let Some(id) = &fx.session {
                pane.session(id);
            }
            if let Some(topic) = fx.topic {
                pane.topic(topic);
            }
            if let Some(message) = &fx.failed {
                pane.failed(message);
            }
            if let Some(activity) = fx.report {
                pane.report(activity);
            }
            if fx.changed {
                pane.changed();
            }
        }
        if let (Some(id), Some(remember)) = (&fx.session, &self.on_session) {
            remember(&self.pane_id, id);
        }
        out
    }

    pub(super) fn request(&self, st: &mut State, fx: &mut Effects, method: &str, params: Value, waiting: Waiting) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        st.waiting.insert(id, waiting);
        fx.send.push(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    /// The app's MCP server, as an agent that takes one over HTTP is given it.
    fn servers(&self, st: &mut State) -> Value {
        match &st.mcp {
            Some(mcp) if st.caps.http => json!([{
                "type": "http",
                "name": mcp.name,
                "url": mcp.url,
                "headers": [
                    { "name": "Authorization", "value": format!("Bearer {}", mcp.token) },
                    { "name": "X-Villain-Pane", "value": self.pane_id },
                ],
            }]),
            Some(_) => {
                st.mcp = None;
                st.convo.note("This agent takes no MCP server over HTTP, so it cannot use the app's tools.", false);
                json!([])
            }
            None => json!([]),
        }
    }

    /// Open the conversation the launch asked for.
    fn open(&self, st: &mut State, fx: &mut Effects) {
        let pick = std::mem::replace(&mut st.pick, Pick::New);
        let opening = match pick {
            Pick::Id(id) if st.caps.load => Opening::Load(id),
            Pick::Id(id) if st.caps.resume => Opening::Resume(id),
            Pick::Newest if st.caps.list => {
                let cwd = st.cwd.clone();
                self.request(st, fx, "session/list", json!({ "cwd": cwd }), Waiting::List);
                return;
            }
            Pick::Id(_) | Pick::Newest => {
                st.convo.note("This agent cannot pick a conversation back up, so this is a new one.", false);
                Opening::New
            }
            Pick::New => Opening::New,
        };
        self.open_as(st, fx, opening);
    }

    fn open_as(&self, st: &mut State, fx: &mut Effects, opening: Opening) {
        let servers = self.servers(st);
        let cwd = st.cwd.clone();
        let (method, params) = match &opening {
            Opening::New => ("session/new", json!({ "cwd": cwd, "mcpServers": servers })),
            Opening::Load(id) => ("session/load", json!({ "sessionId": id, "cwd": cwd, "mcpServers": servers })),
            Opening::Resume(id) => ("session/resume", json!({ "sessionId": id, "cwd": cwd, "mcpServers": servers })),
        };
        self.request(st, fx, method, params, Waiting::Open(opening));
    }

    pub(super) fn send_prompt(&self, st: &mut State, fx: &mut Effects, text: &str) {
        let Some(session) = st.session.clone() else { return };
        let params = json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] });
        self.request(st, fx, "session/prompt", params, Waiting::Prompt);
        st.busy = true;
        fx.report = Some(Activity::Working);
    }

    /// The next waiting prompt, if a turn can start.
    pub(super) fn next_prompt(&self, st: &mut State, fx: &mut Effects) {
        if st.busy || !st.ready {
            return;
        }
        if let Some((text, entry)) = st.queue.pop_front() {
            match entry {
                Some(entry) => st.convo.dequeue(entry, false),
                None => {
                    st.convo.user(&text, false);
                }
            }
            self.send_prompt(st, fx, &text);
        }
    }

    // ---------------------------------------------------------- the agent

    /// One line from the agent.
    pub(crate) fn handle_line(&self, line: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else { return };
        let method = msg.get("method").and_then(Value::as_str);
        match (method, msg.get("id")) {
            (Some(method), Some(id)) => self.on_request(method, id.clone(), msg.get("params")),
            (Some(method), None) => self.on_notification(method, msg.get("params")),
            (None, Some(id)) => {
                if let Some(id) = id.as_u64() {
                    self.on_response(id, msg.get("result"), msg.get("error"));
                }
            }
            _ => {}
        }
    }

    fn on_notification(&self, method: &str, params: Option<&Value>) {
        if method != "session/update" {
            return;
        }
        let Some(update) = params.and_then(|p| p.get("update")) else { return };
        self.with(|st, fx| {
            let applied = st.convo.apply(update);
            fx.topic = applied.title;
            fx.changed = true;
        });
    }

    fn on_request(&self, method: &str, id: Value, params: Option<&Value>) {
        if method != "session/request_permission" {
            // Not offered in `initialize`, so not expected; answered all the
            // same, so the agent is not left waiting on it.
            let reply = json!({ "jsonrpc": "2.0", "id": id, "error": {
                "code": -32601, "message": format!("Villain Layer does not offer {method}") } });
            let _ = self.out.send(reply.to_string());
            return;
        }
        let empty = Value::Null;
        let params = params.unwrap_or(&empty);
        let tool = params.get("toolCall").unwrap_or(&empty);
        let options = params.get("options").unwrap_or(&empty);
        self.with(|st, fx| {
            let entry = st.convo.question(tool, options);
            let options = options
                .as_array()
                .map(|all| all.iter().filter_map(|o| o.get("optionId")?.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            st.questions.push(Question { rpc: id, entry, options });
            fx.report = Some(Activity::Asking);
        });
    }

    fn on_response(&self, id: u64, result: Option<&Value>, error: Option<&Value>) {
        self.with(|st, fx| {
            let Some(waiting) = st.waiting.remove(&id) else { return };
            let failed = error.map(|e| {
                e.get("message").and_then(Value::as_str).unwrap_or("the agent gave an error").to_string()
            });
            let empty = Value::Null;
            let result = result.unwrap_or(&empty);
            match (waiting, failed) {
                (Waiting::Initialize, None) => {
                    let caps = result.get("agentCapabilities").unwrap_or(&empty);
                    let session = caps.get("sessionCapabilities").unwrap_or(&empty);
                    st.caps = Caps {
                        load: caps.get("loadSession").and_then(Value::as_bool).unwrap_or(false),
                        resume: session.get("resume").is_some_and(Value::is_object),
                        list: session.get("list").is_some_and(Value::is_object),
                        http: caps.pointer("/mcpCapabilities/http").and_then(Value::as_bool).unwrap_or(false),
                        sign_in: sign_in(result.get("authMethods")),
                    };
                    st.agent = result.get("agentInfo").map(|a| {
                        let name = a.get("title").or_else(|| a.get("name")).and_then(Value::as_str).unwrap_or("agent");
                        match a.get("version").and_then(Value::as_str) {
                            Some(v) => format!("{name} {v}"),
                            None => name.to_string(),
                        }
                    });
                    self.open(st, fx);
                }
                (Waiting::Initialize, Some(e)) => {
                    st.convo.note(&format!("The agent would not start: {e}"), true);
                    self.give_up(st, fx);
                }
                (Waiting::List, failed) => {
                    let cwd = st.cwd.clone();
                    let newest = failed.is_none().then(|| newest_in(result, &cwd)).flatten();
                    match newest {
                        Some(id) => {
                            let opening = if st.caps.load { Opening::Load(id) } else if st.caps.resume { Opening::Resume(id) } else { Opening::New };
                            self.open_as(st, fx, opening);
                        }
                        None => self.open_as(st, fx, Opening::New),
                    }
                }
                (Waiting::Open(opening), None) => {
                    let id = match opening {
                        Opening::New => result.get("sessionId").and_then(Value::as_str).map(str::to_string),
                        Opening::Load(id) | Opening::Resume(id) => Some(id),
                    };
                    let Some(id) = id else {
                        st.convo.note("The agent opened a conversation without naming it.", true);
                        self.give_up(st, fx);
                        return;
                    };
                    if let Some(options) = result.get("configOptions") {
                        st.convo.set_settings(options);
                    }
                    st.session = Some(id.clone());
                    fx.session = Some(id);
                    st.ready = true;
                    fx.report = Some(Activity::Idle);
                    self.next_prompt(st, fx);
                }
                (Waiting::Open(Opening::Load(_) | Opening::Resume(_)), Some(e)) => {
                    st.convo.note(&format!("Could not pick the conversation back up ({e}), so this is a new one."), false);
                    self.open_as(st, fx, Opening::New);
                }
                (Waiting::Open(Opening::New), Some(e)) => {
                    st.convo.note(&format!("The agent would not open a conversation: {e}"), true);
                    if !st.caps.sign_in.is_empty() {
                        let how = st.caps.sign_in.join("\n");
                        st.convo.note(&format!("If it needs you to sign in, this is how it says to:\n{how}"), false);
                    }
                    self.give_up(st, fx);
                }
                (Waiting::Prompt, failed) => {
                    st.busy = false;
                    match failed {
                        Some(e) => {
                            st.convo.note(&e, true);
                            fx.failed = Some(e);
                            fx.report = Some(Activity::Done);
                        }
                        None => {
                            let reason = result.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn");
                            st.convo.turn_ended(reason);
                            fx.report = Some(if reason == "cancelled" { Activity::Idle } else { Activity::Done });
                        }
                    }
                    // A question left open by a turn that is over can no
                    // longer be answered.
                    for q in std::mem::take(&mut st.questions) {
                        st.convo.answered(q.entry, "cancelled");
                    }
                    self.next_prompt(st, fx);
                }
                (Waiting::Setting, None) => {
                    if let Some(options) = result.get("configOptions") {
                        st.convo.set_settings(options);
                    }
                }
                (Waiting::Setting, Some(e)) => {
                    st.convo.note(&format!("The agent would not change that: {e}"), true);
                }
            }
        });
    }

    /// The conversation will not open: what was waiting to be sent never
    /// will be, and the pane is finished with something to read.
    fn give_up(&self, st: &mut State, fx: &mut Effects) {
        st.drop_queue();
        st.ready = false;
        fx.report = Some(Activity::Done);
    }

    /// The process is gone.
    pub(super) fn exited(&self, code: Option<i32>) {
        let stderr = self.stderr.lock().trim().to_string();
        self.with(|st, _| {
            st.exited = true;
            st.busy = false;
            for q in std::mem::take(&mut st.questions) {
                st.convo.answered(q.entry, "cancelled");
            }
            st.drop_queue();
            // Before the conversation opened, the only word on why is what it
            // printed on its way out.
            if st.session.is_none() && !stderr.is_empty() {
                st.convo.note(&format!("The agent exited before it was ready. It said:\n{stderr}"), true);
            }
        });
        if let Some(pane) = self.pane.get() {
            pane.exited(code);
        }
    }
}

/// How an agent's `authMethods` say to sign in, one line each.
fn sign_in(methods: Option<&Value>) -> Vec<String> {
    let Some(all) = methods.and_then(Value::as_array) else { return Vec::new() };
    all.iter()
        .filter_map(|m| {
            let name = m.get("name").and_then(Value::as_str)?;
            let terminal = m.pointer("/_meta/terminal-auth").and_then(|t| {
                let cmd = t.get("command")?.as_str()?;
                let args: Vec<&str> = t.get("args")?.as_array()?.iter().filter_map(Value::as_str).collect();
                Some(format!("{cmd} {}", args.join(" ")).trim().to_string())
            });
            let said = m.get("description").and_then(Value::as_str);
            Some(match (terminal, said) {
                (Some(cmd), _) => format!("- {name}: run `{cmd}` in a shell"),
                (None, Some(said)) => format!("- {name}: {said}"),
                (None, None) => format!("- {name}"),
            })
        })
        .collect()
}

/// The newest conversation `session/list` names for `cwd`.
fn newest_in(result: &Value, cwd: &str) -> Option<String> {
    result
        .get("sessions")?
        .as_array()?
        .iter()
        .filter(|s| s.get("cwd").and_then(Value::as_str).is_none_or(|c| c == cwd))
        .max_by_key(|s| s.get("updatedAt").and_then(Value::as_str).unwrap_or_default().to_string())
        .and_then(|s| s.get("sessionId")?.as_str().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;
    use std::sync::Arc;

    use crate::acp::{Mcp, Pick};

    #[derive(Default)]
    struct Seen {
        reports: Vec<Activity>,
        text: String,
        topic: Option<String>,
        session: Option<String>,
        failed: Option<String>,
    }

    struct FakePane(Arc<Mutex<Seen>>);

    impl Pane for FakePane {
        fn report(&self, a: Activity) { self.0.lock().reports.push(a); }
        fn print(&self, t: &str) { self.0.lock().text.push_str(t); }
        fn changed(&self) {}
        fn topic(&self, t: String) { self.0.lock().topic = Some(t); }
        fn session(&self, id: &str) { self.0.lock().session = Some(id.into()); }
        fn failed(&self, m: &str) { self.0.lock().failed = Some(m.into()); }
        fn exited(&self, _: Option<i32>) {}
    }

    fn launch(pick: Pick, prompt: Option<&str>) -> Launch {
        Launch {
            mcp: Some(Mcp { name: "villain-layer".into(), url: "http://127.0.0.1:1/mcp".into(), token: "tok".into() }),
            pick,
            prompt: prompt.map(str::to_string),
            on_session: None,
        }
    }

    /// A connection with no process: what it sends comes out of the receiver.
    fn conn(pick: Pick, prompt: Option<&str>) -> (Conn, Receiver<String>, Arc<Mutex<Seen>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let c = Conn::new(launch(pick, prompt), "/work", "pane-1", tx);
        let seen = Arc::new(Mutex::new(Seen::default()));
        c.begin(Box::new(FakePane(seen.clone())));
        (c, rx, seen)
    }

    fn sent(rx: &Receiver<String>) -> Vec<Value> {
        rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect()
    }

    fn reply(c: &Conn, id: &Value, result: Value) {
        c.handle_line(&json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string());
    }

    const CAPS: &str = r#"{ "protocolVersion": 1, "agentInfo": { "title": "Fake", "version": "1.0" },
        "agentCapabilities": { "loadSession": true, "mcpCapabilities": { "http": true },
        "sessionCapabilities": { "list": {}, "resume": {} } } }"#;

    /// Through `initialize` and `session/new`, as OpenCode, Copilot and the
    /// Claude adapter answered them in the spike.
    fn opened(prompt: Option<&str>) -> (Conn, Receiver<String>, Arc<Mutex<Seen>>) {
        let (c, rx, seen) = conn(Pick::New, prompt);
        let init = sent(&rx).remove(0);
        reply(&c, &init["id"], serde_json::from_str(CAPS).unwrap());
        let new = sent(&rx).remove(0);
        assert_eq!(new["method"], "session/new");
        reply(&c, &new["id"], json!({ "sessionId": "s-1" }));
        (c, rx, seen)
    }

    #[test]
    fn the_apps_tools_go_to_the_agent_over_its_stdin_with_the_token_as_a_header() {
        let (c, rx, _) = conn(Pick::New, None);
        let init = sent(&rx).remove(0);
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);
        reply(&c, &init["id"], serde_json::from_str(CAPS).unwrap());
        let new = sent(&rx).remove(0);
        let server = &new["params"]["mcpServers"][0];
        assert_eq!(server["type"], "http");
        assert_eq!(server["headers"][0]["value"], "Bearer tok");
        assert_eq!(server["headers"][1], json!({ "name": "X-Villain-Pane", "value": "pane-1" }));
        assert_eq!(new["params"]["cwd"], "/work");
    }

    #[test]
    fn an_agent_with_no_http_mcp_is_started_without_the_tools_and_says_so() {
        let (c, rx, seen) = conn(Pick::New, None);
        let init = sent(&rx).remove(0);
        reply(&c, &init["id"], json!({ "agentCapabilities": {} }));
        let new = sent(&rx).remove(0);
        assert_eq!(new["params"]["mcpServers"], json!([]));
        assert!(seen.lock().text.contains("cannot use the app's tools"));
    }

    #[test]
    fn the_opening_prompt_waits_for_the_conversation_and_then_it_works_until_done() {
        let (c, rx, seen) = opened(Some("Fix the login bug"));
        let prompt = sent(&rx).remove(0);
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["sessionId"], "s-1");
        assert_eq!(prompt["params"]["prompt"][0]["text"], "Fix the login bug");
        assert_eq!(seen.lock().session.as_deref(), Some("s-1"));
        assert_eq!(seen.lock().reports.last(), Some(&Activity::Working));
        reply(&c, &prompt["id"], json!({ "stopReason": "end_turn" }));
        assert_eq!(seen.lock().reports.last(), Some(&Activity::Done));
        assert!(!c.view(None).busy);
    }

    #[test]
    fn a_prompt_longer_than_a_terminal_takes_is_sent_whole() {
        let (c, rx, _) = opened(None);
        let long = "x".repeat(10_000);
        c.prompt(&long).unwrap();
        assert_eq!(sent(&rx)[0]["params"]["prompt"][0]["text"].as_str().map(str::len), Some(10_000));
    }

    #[test]
    fn a_prompt_sent_during_a_turn_waits_for_it_to_end() {
        let (c, rx, _) = opened(None);
        c.prompt("one").unwrap();
        let first = sent(&rx).remove(0);
        c.prompt("two").unwrap();
        assert!(sent(&rx).is_empty());
        assert_eq!(c.view(None).queued, 1);
        reply(&c, &first["id"], json!({ "stopReason": "end_turn" }));
        let second = sent(&rx).remove(0);
        assert_eq!(second["params"]["prompt"][0]["text"], "two");
    }

    #[test]
    fn a_question_is_asking_until_answered_and_the_answer_goes_back_under_its_id() {
        let (c, rx, seen) = opened(Some("go"));
        let _prompt = sent(&rx);
        c.handle_line(r#"{"jsonrpc":"2.0","id":"q7","method":"session/request_permission","params":{"sessionId":"s-1",
            "toolCall":{"toolCallId":"t1","title":"Write hello.txt","kind":"edit"},
            "options":[{"optionId":"allow","name":"Allow","kind":"allow_once"},{"optionId":"reject","name":"Reject","kind":"reject_once"}]}}"#);
        assert_eq!(seen.lock().reports.last(), Some(&Activity::Asking));
        // A phone's "2" picks the second option.
        assert!(c.key("2"));
        let answer = sent(&rx).remove(0);
        assert_eq!(answer, json!({ "jsonrpc": "2.0", "id": "q7", "result": { "outcome": { "outcome": "selected", "optionId": "reject" } } }));
        assert_eq!(seen.lock().reports.last(), Some(&Activity::Working));
        assert!(!c.key("1"), "nothing is open any more");
    }

    #[test]
    fn stopping_cancels_the_turn_and_answers_open_questions_cancelled() {
        let (c, rx, seen) = opened(Some("go"));
        let prompt = sent(&rx).remove(0);
        c.handle_line(r#"{"jsonrpc":"2.0","id":3,"method":"session/request_permission","params":{"sessionId":"s-1",
            "toolCall":{"toolCallId":"t1"},"options":[{"optionId":"a","name":"Allow","kind":"allow_once"}]}}"#);
        c.prompt("then this").unwrap();
        assert!(c.key("\u{1b}"));
        let out = sent(&rx);
        assert_eq!(out[0]["method"], "session/cancel");
        assert_eq!(out[1]["result"]["outcome"]["outcome"], "cancelled");
        assert_eq!(c.view(None).queued, 0, "what waited is not sent after a stop");
        reply(&c, &prompt["id"], json!({ "stopReason": "cancelled" }));
        assert_eq!(seen.lock().reports.last(), Some(&Activity::Idle));
        assert!(sent(&rx).is_empty());
    }

    #[test]
    fn a_conversation_is_picked_up_by_its_id_and_its_replay_is_kept() {
        let (c, rx, seen) = conn(Pick::Id("s-9".into()), None);
        let init = sent(&rx).remove(0);
        reply(&c, &init["id"], serde_json::from_str(CAPS).unwrap());
        let load = sent(&rx).remove(0);
        assert_eq!(load["method"], "session/load");
        assert_eq!(load["params"]["sessionId"], "s-9");
        c.handle_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-9","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"earlier"}}}}"#);
        reply(&c, &load["id"], json!({}));
        assert_eq!(seen.lock().session.as_deref(), Some("s-9"));
        assert_eq!(c.view(None).changes.entries.len(), 1);
        // Picked up, not given work: idle, not done.
        assert_eq!(seen.lock().reports.last(), Some(&Activity::Idle));
    }

    /// OpenCode, picked up with an opening prompt: the replayed reply and the
    /// new one ran together as "pongpong", because the prompt went into the
    /// conversation before the replay did, and the replay into it.
    #[test]
    fn a_prompt_given_while_a_conversation_is_picked_up_goes_after_its_replay() {
        let (c, rx, _) = conn(Pick::Id("s-9".into()), Some("and now?"));
        let init = sent(&rx).remove(0);
        reply(&c, &init["id"], serde_json::from_str(CAPS).unwrap());
        let load = sent(&rx).remove(0);
        let update = |kind: &str, text: &str| {
            c.handle_line(&json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s-9",
                "update": { "sessionUpdate": kind, "content": { "type": "text", "text": text } } } }).to_string());
        };
        update("user_message_chunk", "say pong");
        update("agent_message_chunk", "pong");
        reply(&c, &load["id"], json!({}));
        let prompt = sent(&rx).remove(0);
        assert_eq!(prompt["params"]["prompt"][0]["text"], "and now?");
        update("agent_message_chunk", "pong again");
        let texts: Vec<String> = c.view(None).changes.entries.iter().map(|e| format!("{:?}", e.body)).collect();
        assert_eq!(texts.len(), 4, "{texts:?}");
        assert!(texts[0].contains("say pong") && texts[2].contains("and now?") && texts[3].contains("pong again"), "{texts:?}");
    }

    #[test]
    fn the_newest_conversation_is_the_newest_the_agent_lists_for_the_folder() {
        let (c, rx, _) = conn(Pick::Newest, None);
        let init = sent(&rx).remove(0);
        reply(&c, &init["id"], serde_json::from_str(CAPS).unwrap());
        let list = sent(&rx).remove(0);
        assert_eq!(list["method"], "session/list");
        reply(&c, &list["id"], json!({ "sessions": [
            { "sessionId": "old", "cwd": "/work", "updatedAt": "2026-10-01T10:00:00Z" },
            { "sessionId": "elsewhere", "cwd": "/other", "updatedAt": "2026-10-08T10:00:00Z" },
            { "sessionId": "new", "cwd": "/work", "updatedAt": "2026-10-07T10:00:00Z" }
        ] }));
        let load = sent(&rx).remove(0);
        assert_eq!(load["params"]["sessionId"], "new");
    }

    #[test]
    fn a_conversation_that_will_not_load_is_started_new_and_says_why() {
        let (c, rx, seen) = conn(Pick::Id("gone".into()), None);
        let init = sent(&rx).remove(0);
        reply(&c, &init["id"], serde_json::from_str(CAPS).unwrap());
        let load = sent(&rx).remove(0);
        c.handle_line(&json!({ "jsonrpc": "2.0", "id": load["id"], "error": { "code": -32002, "message": "Session not found" } }).to_string());
        assert_eq!(sent(&rx).remove(0)["method"], "session/new");
        assert!(seen.lock().text.contains("Session not found"));
    }

    #[test]
    fn an_agent_that_will_not_open_says_how_it_wants_signing_in() {
        let (c, rx, seen) = conn(Pick::New, Some("go"));
        let init = sent(&rx).remove(0);
        reply(&c, &init["id"], json!({ "agentCapabilities": {}, "authMethods": [{ "id": "copilot-login", "name": "Log in with Copilot CLI",
            "_meta": { "terminal-auth": { "command": "copilot", "args": ["login"] } } }] }));
        let new = sent(&rx).remove(0);
        c.handle_line(&json!({ "jsonrpc": "2.0", "id": new["id"], "error": { "code": -32000, "message": "Authentication required" } }).to_string());
        let text = seen.lock().text.clone();
        assert!(text.contains("Authentication required"), "{text}");
        assert!(text.contains("run `copilot login` in a shell"), "{text}");
        assert_eq!(c.view(None).queued, 0);
        assert!(sent(&rx).is_empty());
    }

    #[test]
    fn a_failed_turn_is_done_and_its_message_may_be_a_usage_limit() {
        let (c, rx, seen) = opened(Some("go"));
        let prompt = sent(&rx).remove(0);
        c.handle_line(&json!({ "jsonrpc": "2.0", "id": prompt["id"], "error": { "code": -32000, "message": "Claude usage limit reached" } }).to_string());
        assert_eq!(seen.lock().failed.as_deref(), Some("Claude usage limit reached"));
        assert_eq!(seen.lock().reports.last(), Some(&Activity::Done));
    }

    #[test]
    fn a_request_the_app_does_not_offer_is_refused_not_left_waiting() {
        let (c, rx, _) = opened(None);
        c.handle_line(r#"{"jsonrpc":"2.0","id":11,"method":"fs/read_text_file","params":{"path":"/etc/hosts"}}"#);
        let refused = sent(&rx).remove(0);
        assert_eq!(refused["id"], 11);
        assert_eq!(refused["error"]["code"], -32601);
    }

    #[test]
    fn the_title_the_agent_gives_names_the_pane() {
        let (c, _rx, seen) = opened(None);
        c.handle_line(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-1","update":{"sessionUpdate":"session_info_update","title":"Fixing the login bug"}}}"#);
        assert_eq!(seen.lock().topic.as_deref(), Some("Fixing the login bug"));
    }
}
