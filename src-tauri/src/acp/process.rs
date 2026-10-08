//! The process behind a connection: started in its own group, with a
//! thread each to write to it, read it, keep the end of its stderr and wait
//! for it to exit, as a terminal pane has. Nothing here runs on the async
//! runtime or the main thread.

use std::io::{BufRead, Read, Write};
use std::sync::Arc;

use super::conn::Conn;
use super::{Launch, Pane};
use crate::error::{Error, Result};
use crate::shellenv;

/// How much of the end of an agent's stderr is kept, to say why it would
/// not start.
const STDERR_TAIL: usize = 4096;

/// A started agent, not yet handed to its pane.
pub struct Started {
    pub conn: Arc<Conn>,
    pub pid: u32,
    child: std::process::Child,
    lines: std::sync::mpsc::Receiver<String>,
}

/// Start `program` as an ACP agent in `cwd`, in its own process group, with
/// the login shell's environment and `env` over it (PANE-4), as pane
/// `pane_id`.
pub fn start(program: &str, args: &[String], cwd: &str, env: &[(String, String)], launch: Launch, pane_id: &str) -> Result<Started> {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(shellenv::user_env())
        .env("VILLAIN_PANE", pane_id)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Its own group, so stopping it reaches whatever it started (PANE-5).
        .process_group(0);
    let child = cmd.spawn().map_err(|e| Error::Pty(format!("spawn {program}: {e}")))?;
    let pid = child.id();
    let (out, lines) = std::sync::mpsc::channel();
    let conn = Arc::new(Conn::new(launch, cwd, pane_id, out));
    Ok(Started { conn, pid, child, lines })
}

impl Started {
    /// Hand the agent to its pane and start talking to it.
    pub fn run(self, pane: Box<dyn Pane>) {
        let Started { conn, mut child, lines, .. } = self;
        let (Some(mut stdin), Some(stdout), Some(mut stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return;
        };
        // Ends when the connection is dropped with its pane, which closes
        // the agent's stdin.
        std::thread::spawn(move || {
            for line in lines {
                if stdin.write_all(line.as_bytes()).and_then(|_| stdin.write_all(b"\n")).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        {
            let conn = conn.clone();
            std::thread::spawn(move || {
                let mut reader = std::io::BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => conn.handle_line(line.trim_end()),
                    }
                }
            });
        }
        {
            let conn = conn.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut tail = conn.stderr.lock();
                    tail.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if tail.len() > STDERR_TAIL {
                        let mut cut = tail.len() - STDERR_TAIL;
                        while !tail.is_char_boundary(cut) {
                            cut += 1;
                        }
                        tail.drain(..cut);
                    }
                }
            });
        }
        conn.begin(pane);
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            conn.exited(code);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::Pick;
    use crate::pty::Activity;

    /// A real process behind it, which reads the first line the app sends
    /// (`initialize`), echoes it, and exits 3. Proves the threads carry lines
    /// both ways and the exit reaches the pane.
    #[test]
    fn a_process_carries_lines_both_ways_and_its_exit_reaches_the_pane() {
        struct Exit(std::sync::mpsc::Sender<Option<i32>>);
        impl Pane for Exit {
            fn report(&self, _: Activity) {}
            fn print(&self, _: &str) {}
            fn changed(&self) {}
            fn topic(&self, _: String) {}
            fn session(&self, _: &str) {}
            fn failed(&self, _: &str) {}
            fn exited(&self, code: Option<i32>) { let _ = self.0.send(code); }
        }
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        let args = vec!["-c".to_string(), "head -n 1; exit 3".to_string()];
        let launch = Launch { mcp: None, pick: Pick::New, prompt: None, on_session: None };
        let started = start("/bin/sh", &args, &dir, &[], launch, "pane-2").unwrap();
        let conn: Arc<Conn> = started.conn.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        started.run(Box::new(Exit(tx)));
        let code = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(code, Some(3));
        assert!(conn.view(None).exited);
    }
}
