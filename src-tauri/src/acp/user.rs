//! What the user does to a conversation: send a prompt, stop a turn, answer
//! a question, change a setting, look at it.

use serde_json::json;

use super::conn::{Conn, Waiting};
use super::View;
use crate::error::{Error, Result};
use crate::pty::Activity;

impl Conn {
    /// Send a prompt, whole however long it is (ACP-4). One sent while a
    /// turn runs waits for it to end.
    pub fn prompt(&self, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        self.with(|st, fx| {
            if st.exited {
                return Err(Error::Other("the agent has exited".into()));
            }
            // Into the conversation now only if it is open: one being picked
            // up is still replaying (`State::queue`).
            let entry = st.ready.then(|| st.convo.user(text, st.busy));
            st.queue.push_back((text.to_string(), entry));
            self.next_prompt(st, fx);
            Ok(())
        })
    }

    /// Stop the running turn (ACP-6). Every open question is answered
    /// "cancelled", as the protocol requires, and prompts that were waiting
    /// are not sent. True when there was anything to stop.
    pub fn cancel(&self) -> bool {
        self.with(|st, fx| {
            if !st.busy && st.questions.is_empty() && st.queue.is_empty() {
                return false;
            }
            if let (Some(session), true) = (st.session.clone(), st.busy) {
                fx.send.push(json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session } }));
            }
            for q in std::mem::take(&mut st.questions) {
                fx.send.push(json!({ "jsonrpc": "2.0", "id": q.rpc, "result": { "outcome": { "outcome": "cancelled" } } }));
                st.convo.answered(q.entry, "cancelled");
            }
            st.drop_queue();
            true
        })
    }

    /// Answer the question at `entry` with one of its options (ACP-5).
    pub fn answer(&self, entry: usize, option: &str) -> Result<()> {
        self.with(|st, fx| {
            let at = st
                .questions
                .iter()
                .position(|q| q.entry == entry && q.options.iter().any(|o| o == option))
                .ok_or_else(|| Error::Other("that question is no longer open".into()))?;
            let q = st.questions.remove(at);
            fx.send.push(json!({ "jsonrpc": "2.0", "id": q.rpc, "result": { "outcome": { "outcome": "selected", "optionId": option } } }));
            st.convo.answered(q.entry, option);
            if st.questions.is_empty() && st.busy {
                fx.report = Some(Activity::Working);
            }
            Ok(())
        })
    }

    /// Keys, from a phone or a terminal's habits (ACP-10): Esc and Ctrl-C
    /// stop the turn; 1 to 9 pick the oldest open question's options in
    /// order. True when they did anything.
    pub fn key(&self, data: &str) -> bool {
        match data {
            "\u{1b}" | "\u{3}" => self.cancel(),
            d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && d != "0" => {
                let n = (d.as_bytes()[0] - b'1') as usize;
                let pick = {
                    let st = self.state.lock();
                    st.questions.first().and_then(|q| Some((q.entry, q.options.get(n)?.clone())))
                };
                pick.is_some_and(|(entry, option)| self.answer(entry, &option).is_ok())
            }
            _ => false,
        }
    }

    /// Change one of the session's settings (ACP-12).
    pub fn set(&self, setting: &str, value: &str) -> Result<()> {
        self.with(|st, fx| {
            let session = st.session.clone().ok_or_else(|| Error::Other("the conversation is not open yet".into()))?;
            let params = json!({ "sessionId": session, "configId": setting, "value": value });
            self.request(st, fx, "session/set_config_option", params, Waiting::Setting);
            Ok(())
        })
    }

    /// What its finished turns used, for the run log (RUN-4).
    pub fn tokens(&self) -> Option<crate::runs::Tokens> {
        self.state.lock().tokens
    }

    /// Its tool calls that finished, and failed (RUN-7).
    pub fn tools(&self) -> crate::runs::ToolCalls {
        self.state.lock().convo.calls
    }

    pub fn view(&self, since: Option<u64>) -> View {
        let st = self.state.lock();
        View {
            changes: st.convo.changes(since),
            agent: st.agent.clone(),
            ready: st.ready,
            busy: st.busy,
            queued: st.queue.len(),
            settings: st.convo.settings.clone(),
            commands: st.convo.commands.clone(),
            usage: st.convo.usage.clone(),
            exited: st.exited,
        }
    }
}
