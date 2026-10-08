//! Keep the Mac from sleeping while an agent is working (STATE-7).
//!
//! Held by `caffeinate`, not an IOKit assertion of our own: no new
//! dependency, and `-w` ties it to this app's pid, so the assertion ends with
//! the app even when a panic aborts it and nothing here gets to clean up.
//! `-i` prevents idle sleep only. The display still sleeps, and closing the
//! lid still sleeps the Mac: that is the one way left to say "sleep now".

use std::process::{Child, Command, Stdio};

use crate::pty::{Activity, PaneInfo, PaneKind};

/// Whether anything is working that sleep would interrupt.
///
/// Only agents: a shell's state is not known, and one left open would keep
/// the Mac up for good. A chat at work counts as much as a task.
///
/// With a phone's way in open, any agent still running counts (PHONE-8): one
/// asking for permission is exactly the one you would answer from the phone,
/// and a Mac asleep is not there to answer.
pub(crate) fn wanted(on: bool, phone: bool, panes: &[(PaneInfo, bool)]) -> bool {
    on && panes.iter().any(|(info, _)| {
        info.kind == PaneKind::Agent && info.running && (phone || info.activity == Activity::Working)
    })
}

#[derive(Default)]
pub(crate) struct Awake {
    child: Option<Child>,
}

impl Awake {
    /// Hold the Mac awake, or let it go. Called on every attention pass.
    pub(crate) fn hold(&mut self, want: bool) {
        // Someone ran `killall caffeinate`: hold it again rather than believe
        // it still held.
        if let Some(child) = self.child.as_mut() {
            if !matches!(child.try_wait(), Ok(None)) {
                self.child = None;
            }
        }
        match (want, self.child.take()) {
            (true, None) => self.child = caffeinate(),
            (false, Some(mut child)) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            (_, kept) => self.child = kept,
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.hold(false);
    }
}

fn caffeinate() -> Option<Child> {
    Command::new("/usr/bin/caffeinate")
        .arg("-i")
        .arg("-w")
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CHAT_TASK_ID;

    fn pane(kind: PaneKind, task_id: &str, activity: Activity) -> (PaneInfo, bool) {
        let now = chrono::Utc::now();
        let info = PaneInfo {
            id: "p".into(),
            task_id: task_id.into(),
            checkout_id: None,
            kind,
            title: "Claude Code".into(),
            agent_id: Some("claude".into()),
            cwd: "/tmp".into(),
            running: true,
            exit_code: None,
            started_at: now,
            last_output_at: now,
            notice: None,
            activity,
            activity_since: now,
            topic: None,
            acp: false,
        };
        (info, false)
    }

    #[test]
    fn only_a_working_agent_keeps_the_mac_awake() {
        let working = pane(PaneKind::Agent, "t1", Activity::Working);
        assert!(wanted(true, false, std::slice::from_ref(&working)));
        assert!(!wanted(false, false, std::slice::from_ref(&working)), "not unless switched on");
        assert!(!wanted(true, false, &[]));
        for quiet in [Activity::Asking, Activity::Done, Activity::Idle] {
            assert!(!wanted(true, false, &[pane(PaneKind::Agent, "t1", quiet)]), "{quiet:?}");
        }
        assert!(!wanted(true, false, &[pane(PaneKind::Shell, "t1", Activity::Working)]));
        let mut exited = working.clone();
        exited.0.running = false;
        assert!(!wanted(true, false, &[exited]));
        assert!(wanted(true, false, &[pane(PaneKind::Agent, CHAT_TASK_ID, Activity::Working)]));
    }

    #[test]
    fn with_a_phone_way_in_open_any_running_agent_keeps_the_mac_awake() {
        for quiet in [Activity::Asking, Activity::Done, Activity::Idle] {
            assert!(wanted(true, true, &[pane(PaneKind::Agent, "t1", quiet)]), "{quiet:?}");
        }
        assert!(!wanted(false, true, &[pane(PaneKind::Agent, "t1", Activity::Asking)]), "still only with the cup on");
        assert!(!wanted(true, true, &[pane(PaneKind::Shell, "t1", Activity::Idle)]), "a shell never does");
        let mut exited = pane(PaneKind::Agent, "t1", Activity::Idle);
        exited.0.running = false;
        assert!(!wanted(true, true, &[exited]));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn holding_starts_caffeinate_and_letting_go_ends_it() {
        let mut awake = Awake::default();
        awake.hold(true);
        assert!(awake.child.is_some());
        let pid = awake.child.as_ref().map(|c| c.id());
        awake.hold(true);
        assert_eq!(awake.child.as_ref().map(|c| c.id()), pid, "one caffeinate, not one per pass");

        // One that died by other hands is replaced on the next pass.
        if let Some(child) = awake.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        awake.hold(true);
        assert!(awake.child.is_some());
        assert_ne!(awake.child.as_ref().map(|c| c.id()), pid);

        awake.hold(false);
        assert!(!awake.child.is_some());
    }
}
