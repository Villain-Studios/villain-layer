//! Running one repository's check command (LOOP-1, LOOP-5): a process of its
//! own, in the worktree, until it exits, runs out of time or is stopped.
//! Blocking, so only ever on the blocking pool.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// The most output held while a command runs. Only its end is ever shown,
/// and a whole build's log, held entire, is tens of megabytes.
const KEEP: usize = 256 * 1024;

/// How long a command stopped on purpose gets to end before it is killed,
/// as an agent gets (PANE-5).
const GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Exited,
    TimedOut,
    Stopped,
}

#[derive(Debug)]
pub struct Outcome {
    pub ended: Ended,
    /// Its exit code; None when it was stopped, or a signal ended it.
    pub code: Option<i32>,
    /// The end of what it printed, standard error and output together in
    /// the order written.
    pub output: String,
    pub secs: f64,
}

/// Run `command` in `dir` through `/bin/sh -c`, in the login shell's
/// environment, for at most `timeout`, or until `stop` is raised.
pub fn run(dir: &Path, command: &str, timeout: Duration, stop: &AtomicBool) -> Result<Outcome> {
    let (mut reader, writer) = std::io::pipe()?;
    let mut child = {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(dir)
            .env_clear()
            .envs(crate::shellenv::user_env())
            // Colour codes are noise to an agent reading a log, and some
            // tools draw progress bars only for a terminal they think is one.
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .stdin(Stdio::null())
            .stdout(writer.try_clone()?)
            .stderr(writer)
            // A group of its own, so stopping it reaches what it started: a
            // test runner's workers, a build's compilers.
            .process_group(0);
        // `cmd` holds the pipe's writing end until it is dropped, and the
        // reader would wait on it for ever.
        cmd.spawn().map_err(|e| Error::Other(format!("could not start `{command}`: {e}")))?
    };
    // From here, not from the call: the login shell's environment is read
    // once, on first use, and that took the whole of a short time limit.
    let started = Instant::now();
    let group = child.id() as i32;

    let held = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    {
        let held = held.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut held = held.lock();
                held.extend_from_slice(&buf[..n]);
                if held.len() > 2 * KEEP {
                    let cut = held.len() - KEEP;
                    held.drain(..cut);
                }
            }
            let _ = done_tx.send(());
        });
    }

    let deadline = started + timeout;
    let (ended, code) = loop {
        if let Some(status) = child.try_wait()? {
            break (Ended::Exited, status.code());
        }
        let ended = if stop.load(Ordering::Acquire) {
            Ended::Stopped
        } else if Instant::now() >= deadline {
            Ended::TimedOut
        } else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        signal(group, libc::SIGTERM);
        let grace = Instant::now() + GRACE;
        while Instant::now() < grace && child.try_wait()?.is_none() {
            std::thread::sleep(Duration::from_millis(50));
        }
        signal(group, libc::SIGKILL);
        let _ = child.wait();
        break (ended, None);
    };
    // Whatever it left running in its group goes with it: a server a test
    // started and never stopped would hold the output open, and run on.
    signal(group, libc::SIGKILL);
    // Something that left the group and kept the output open must not hold
    // the loop up: what was read by now is the answer.
    let _ = done_rx.recv_timeout(Duration::from_secs(2));

    let mut output = held.lock().clone();
    if output.len() > KEEP {
        output.drain(..output.len() - KEEP);
    }
    Ok(Outcome {
        ended,
        code,
        output: String::from_utf8_lossy(&output).into_owned(),
        secs: started.elapsed().as_secs_f64(),
    })
}

fn signal(group: i32, signal: i32) {
    // A negative pid is the whole group. The group may be gone already,
    // which is what this was for.
    unsafe { libc::kill(-group, signal) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn a_command_that_exits_0_passes_with_what_it_printed_on_both_streams_in_order() {
        let stop = AtomicBool::new(false);
        let out = run(&dir(), "echo one; echo two >&2; echo three", Duration::from_secs(10), &stop).unwrap();
        assert_eq!(out.ended, Ended::Exited);
        assert_eq!(out.code, Some(0));
        assert_eq!(out.output, "one\ntwo\nthree\n");
    }

    #[test]
    fn a_failing_command_says_its_code_and_runs_in_the_folder_it_was_given() {
        let here = std::env::temp_dir().join(format!("vl-check-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::write(here.join("marker"), "").unwrap();
        let stop = AtomicBool::new(false);
        let out = run(&here, "ls; exit 3", Duration::from_secs(10), &stop).unwrap();
        assert_eq!(out.code, Some(3));
        assert!(out.output.contains("marker"), "{}", out.output);
        let missing = run(&here, "no-such-command-anywhere", Duration::from_secs(10), &stop).unwrap();
        assert_eq!(missing.code, Some(127), "the shell's own code for a command it cannot find");
        let _ = std::fs::remove_dir_all(&here);
    }

    #[test]
    fn a_command_past_its_time_is_stopped_with_everything_it_started() {
        let stop = AtomicBool::new(false);
        let begun = Instant::now();
        // A child of its own that would print later, had it lived.
        let out = run(&dir(), "(sleep 30; echo late) & echo early; sleep 30", Duration::from_millis(1500), &stop).unwrap();
        assert_eq!(out.ended, Ended::TimedOut);
        assert_eq!(out.code, None);
        assert!(begun.elapsed() < Duration::from_secs(10), "took {:?}", begun.elapsed());
        assert_eq!(out.output, "early\n");
    }

    #[test]
    fn a_command_is_stopped_when_asked() {
        let stop = Arc::new(AtomicBool::new(false));
        let raise = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            raise.store(true, Ordering::Release);
        });
        let out = run(&dir(), "sleep 30", Duration::from_secs(60), &stop).unwrap();
        assert_eq!(out.ended, Ended::Stopped);
    }

    #[test]
    fn a_long_log_keeps_its_end() {
        let stop = AtomicBool::new(false);
        let out = run(&dir(), "i=0; while [ $i -lt 40000 ]; do echo line-$i-padding-padding; i=$((i+1)); done", Duration::from_secs(60), &stop).unwrap();
        assert!(out.output.len() <= KEEP);
        assert!(out.output.ends_with("line-39999-padding-padding\n"));
    }
}
