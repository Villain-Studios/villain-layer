//! The pane side of an agent over ACP (§20): starting one into the pane map,
//! and applying what its connection reports — its state, the text copy, the
//! conversation's name — as a terminal's reader thread applies what it reads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter, Runtime};

use super::{Activity, ExitEvent, Io, NoticeEvent, Pane, PaneInfo, PtyManager, SpawnOptions};
use crate::acp;
use crate::error::{Error, Result};

/// How long a conversation's changes are gathered before the window is told.
/// A reply streams in as dozens of chunks a second, and each telling is a
/// fetch of what changed.
const UPDATE_GATHER: Duration = Duration::from_millis(40);

/// What a connection reports, applied to its pane. Holds the pane weakly:
/// the pane holds the connection.
struct Sink<R: Runtime> {
    pane: Weak<Pane>,
    app: AppHandle<R>,
    id: String,
    /// A telling is on its way to the window.
    telling: Arc<AtomicBool>,
}

impl<R: Runtime> acp::Pane for Sink<R> {
    fn report(&self, activity: Activity) {
        let Some(pane) = self.pane.upgrade() else { return };
        let watched = pane.watched.load(Ordering::Acquire);
        let changed = {
            let mut meta = pane.meta.lock();
            let before = (meta.activity(watched, Utc::now()), meta.info.notice.clone());
            // Working means it was given something to do, which is what lets
            // the end of it be done rather than idle.
            if activity == Activity::Working {
                meta.prompted = true;
            }
            meta.take_report(activity);
            (meta.activity(watched, Utc::now()), meta.info.notice.clone()) != before
        };
        if changed {
            let _ = self.app.emit("pty:activity", &self.id);
        }
    }

    fn print(&self, text: &str) {
        let Some(pane) = self.pane.upgrade() else { return };
        pane.output.lock().push(text.as_bytes());
        {
            let mut meta = pane.meta.lock();
            let now = Utc::now();
            meta.info.last_output_at = now;
            meta.last_work = now;
        }
        pane.printed.send_modify(|_| {});
    }

    /// Only while the pane is on screen, as a terminal's output is fed: one
    /// coming on screen asks for what changed since it last looked.
    fn changed(&self) {
        let Some(pane) = self.pane.upgrade() else { return };
        if !pane.watched.load(Ordering::Acquire) || self.telling.swap(true, Ordering::AcqRel) {
            return;
        }
        let (app, id, telling) = (self.app.clone(), self.id.clone(), self.telling.clone());
        std::thread::spawn(move || {
            std::thread::sleep(UPDATE_GATHER);
            // Down before the event, so a change after it is told again.
            telling.store(false, Ordering::Release);
            let _ = app.emit("acp:update", &id);
        });
    }

    fn topic(&self, topic: String) {
        let Some(pane) = self.pane.upgrade() else { return };
        let mut meta = pane.meta.lock();
        if meta.info.topic.as_deref() != Some(topic.as_str()) {
            meta.info.topic = Some(topic);
            drop(meta);
            // The pane list is what carries it.
            let _ = self.app.emit("pty:activity", &self.id);
        }
    }

    fn session(&self, id: &str) {
        if let Some(pane) = self.pane.upgrade() {
            pane.meta.lock().session = Some(id.to_string());
        }
    }

    /// A failed turn's message, read for a usage limit as a terminal's tail
    /// is (STATE-6). It clears when the agent next works.
    fn failed(&self, message: &str) {
        if super::screen::notice_in(message.as_bytes(), &mut Vec::new()) != Some("usage_limit") {
            return;
        }
        let Some(pane) = self.pane.upgrade() else { return };
        {
            let mut meta = pane.meta.lock();
            meta.info.notice = Some("usage_limit".into());
            meta.notice_at = Utc::now();
            meta.settle();
        }
        let _ = self.app.emit("pty:notice", NoticeEvent { pane_id: &self.id, notice: Some("usage_limit") });
    }

    fn exited(&self, code: Option<i32>) {
        let Some(pane) = self.pane.upgrade() else { return };
        super::log_run(&self.app, &pane, code);
        pane.meta.lock().exited(code);
        pane.printed.send_modify(|_| {});
        let _ = self.app.emit("pty:exit", ExitEvent { pane_id: &self.id, code });
    }
}

impl PtyManager {
    /// Start an agent over ACP in a pane of its own (§20): `opts.program` and
    /// `args` are its ACP command. The opening prompt is `launch.prompt`,
    /// sent whole once the conversation is open, not `initial_input`.
    pub fn spawn_acp<R: Runtime>(&self, app: &AppHandle<R>, opts: SpawnOptions, launch: acp::Launch) -> Result<PaneInfo> {
        let slot = self.reserve(&opts.cwd)?;
        let id = uuid::Uuid::new_v4().to_string();
        let started = acp::start(&opts.program, &opts.args, &opts.cwd, &opts.env, launch, &id)?;
        let (info, pane) = Self::new_pane(&opts, &id, Some(started.pid), Io::Acp(started.conn.clone()));
        self.panes.lock().insert(id.clone(), pane.clone());
        drop(slot);
        started.run(Box::new(Sink { pane: Arc::downgrade(&pane), app: app.clone(), id, telling: Default::default() }));
        Ok(info)
    }

    /// The conversation behind a pane.
    fn conn(&self, id: &str) -> Result<Arc<acp::Conn>> {
        match &self.get(id)?.io {
            Io::Acp(conn) => Ok(conn.clone()),
            Io::Pty { .. } => Err(Error::Other("this pane is a terminal, not a conversation".into())),
        }
    }

    pub fn is_acp(&self, id: &str) -> bool {
        self.conn(id).is_ok()
    }

    /// What the window draws a conversation from: what changed after `since`.
    /// The pane is on screen from now, as `attach` makes a terminal; `seen`
    /// says whether that changed what it reads as.
    pub fn acp_view(&self, id: &str, since: Option<u64>) -> Result<(acp::View, bool)> {
        let pane = self.get(id)?;
        let Io::Acp(conn) = &pane.io else {
            return Err(Error::Other("this pane is a terminal, not a conversation".into()));
        };
        let was_watched = pane.watched.swap(true, Ordering::AcqRel);
        let seen = {
            let mut meta = pane.meta.lock();
            let before = meta.activity(was_watched, Utc::now());
            meta.seen_at = Utc::now();
            meta.activity(true, Utc::now()) != before
        };
        Ok((conn.view(since), seen))
    }

    pub fn acp_prompt(&self, id: &str, text: &str) -> Result<()> {
        self.conn(id)?.prompt(text)
    }

    pub fn acp_cancel(&self, id: &str) -> Result<bool> {
        Ok(self.conn(id)?.cancel())
    }

    pub fn acp_answer(&self, id: &str, entry: usize, option: &str) -> Result<()> {
        self.conn(id)?.answer(entry, option)
    }

    pub fn acp_set(&self, id: &str, setting: &str, value: &str) -> Result<()> {
        self.conn(id)?.set(setting, value)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::pty::PaneKind;

    /// An agent in four lines of shell: it answers `initialize`, opens
    /// session `s-1`, replies "pong" to the first prompt and waits. The ids
    /// are the ones the app sends them under, in that order.
    const FAKE_AGENT: &str = r#"
read -r _; printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}'
read -r _; printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-1"}}'
read -r _; printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"pong"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
cat > /dev/null
"#;

    #[test]
    fn an_acp_agent_is_a_pane_that_works_until_done_and_keeps_a_text_copy() {
        let app = tauri::test::mock_app();
        let ptys = PtyManager::default();
        let opts = SpawnOptions {
            task_id: "t".into(),
            checkout_id: None,
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
            kind: PaneKind::Agent,
            title: "Fake".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), FAKE_AGENT.into()],
            agent_id: Some("fake".into()),
            rows: None,
            cols: None,
            initial_input: None,
            prompted: true,
            env: Vec::new(),
            title_activity: None,
            title_topic: None,
        };
        let launch = acp::Launch { mcp: None, pick: acp::Pick::New, prompt: Some("ping".into()), on_session: None };
        let pane = ptys.spawn_acp(app.handle(), opts, launch).unwrap();
        assert!(pane.acp);

        let deadline = Instant::now() + Duration::from_secs(10);
        while ptys.info(&pane.id).unwrap().activity != Activity::Done && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(ptys.info(&pane.id).unwrap().activity, Activity::Done);
        assert_eq!(ptys.session(&pane.id).unwrap().as_deref(), Some("s-1"));
        let text = ptys.transcript(&pane.id, 50).unwrap();
        assert!(text.contains("› ping") && text.contains("pong"), "{text}");
        // Keys are not typed into a conversation, and a terminal's limit on
        // typing does not apply to it.
        assert!(!ptys.write(&pane.id, "x").unwrap());
        assert!(ptys.resize(&pane.id, 10, 10).is_ok());
        ptys.close(&pane.id).unwrap();
        assert!(ptys.list(None).is_empty());
    }

    /// The app's whole path against a real agent, which this machine must
    /// have and be signed in to: a prompt answered, the conversation's id
    /// kept, and the conversation picked up by it in a new pane (ACP-9).
    /// Not run by default:
    /// `VILLAIN_ACP_AGENT="opencode acp" cargo test --lib real_agent -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn a_real_agent_answers_and_is_picked_up_again_by_its_id() {
        let Ok(command) = std::env::var("VILLAIN_ACP_AGENT") else {
            panic!("set VILLAIN_ACP_AGENT to the agent's ACP command, e.g. \"opencode acp\"");
        };
        let mut words = command.split_whitespace();
        let program = crate::shellenv::which(words.next().unwrap()).expect("the agent is not on the PATH");
        let args: Vec<String> = words.map(str::to_string).collect();
        let dir = std::env::temp_dir().join(format!("vl-acp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let app = tauri::test::mock_app();
        let ptys = PtyManager::default();
        let start = |pick: acp::Pick, prompt: &str| {
            let opts = SpawnOptions {
                task_id: "t".into(),
                checkout_id: None,
                cwd: dir.to_string_lossy().to_string(),
                kind: PaneKind::Agent,
                title: "Real".into(),
                program: program.clone(),
                args: args.clone(),
                agent_id: None,
                rows: None,
                cols: None,
                initial_input: None,
                prompted: true,
                env: Vec::new(),
                title_activity: None,
                title_topic: None,
            };
            let launch = acp::Launch { mcp: None, pick, prompt: Some(prompt.into()), on_session: None };
            ptys.spawn_acp(app.handle(), opts, launch).unwrap()
        };
        let done = |id: &str| {
            let deadline = Instant::now() + Duration::from_secs(180);
            while ptys.info(id).unwrap().activity != Activity::Done && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            let text = ptys.transcript(id, 200).unwrap();
            println!("---- {id}\n{text}");
            assert_eq!(ptys.info(id).unwrap().activity, Activity::Done, "{text}");
            text
        };

        let first = start(acp::Pick::New, "Reply with exactly the one word: pong");
        assert!(done(&first.id).to_lowercase().contains("pong"));
        let session = ptys.session(&first.id).unwrap().expect("the agent named its conversation");
        ptys.close(&first.id).unwrap();

        let again = start(acp::Pick::Id(session), "What one word did you reply with before? Reply with only that word.");
        let text = done(&again.id);
        let answer = text.rsplit("Reply with only that word.").next().unwrap_or_default().to_lowercase();
        assert!(answer.contains("pong"), "the conversation was not picked up: {text}");
        ptys.close(&again.id).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
