//! Finding Chrome and running it: one headless process for the whole app,
//! driven over a pipe rather than a port (BRW-8).
//!
//! `--remote-debugging-port` would let any process on the machine drive the
//! browser, and with it every site the user signed in to there. With
//! `--remote-debugging-pipe`, Chrome reads commands from its fd 3 and writes
//! to its fd 4, and only the app holds the other ends.

use std::io::{PipeReader, PipeWriter};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Where a Chrome or Chromium is found, in order. The app's own profile
/// (BRW-1) makes any of them fit; the first one installed is used.
const APPS: &[(&str, &str)] = &[
    ("Google Chrome.app", "Google Chrome"),
    ("Chromium.app", "Chromium"),
    ("Google Chrome for Testing.app", "Google Chrome for Testing"),
    ("Google Chrome Beta.app", "Google Chrome Beta"),
    ("Google Chrome Canary.app", "Google Chrome Canary"),
];

/// The browser to run, if one is installed.
pub fn find() -> Option<PathBuf> {
    #[allow(deprecated)]
    let home = std::env::home_dir();
    let roots: Vec<PathBuf> = std::iter::once(PathBuf::from("/Applications"))
        .chain(home.map(|h| h.join("Applications")))
        .collect();
    APPS.iter()
        .flat_map(|(app, exe)| roots.iter().map(move |r| r.join(app).join("Contents/MacOS").join(exe)))
        .find(|p| p.is_file())
}

/// What to tell someone who has no Chrome.
pub const MISSING: &str = "No Chrome or Chromium is installed in /Applications or ~/Applications. \
     The browser runs on Google Chrome (or Chromium): install it, and the browser starts the next time it is needed.";

/// The running browser's process.
pub struct Process {
    child: Child,
}

/// Start Chrome on `profile`, returning the process, the end the app writes
/// commands to, and the end it reads Chrome's answers from.
///
/// Blocking: spawning waits on the disk. Call it through `off_runtime`.
pub fn launch(binary: &Path, profile: &Path) -> Result<(Process, PipeWriter, PipeReader)> {
    std::fs::create_dir_all(profile)?;
    let (commands_in, commands_out) = std::io::pipe()?;
    let (answers_in, answers_out) = std::io::pipe()?;

    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(binary);
    cmd.arg("--headless")
        .arg("--remote-debugging-pipe")
        .arg(format!("--user-data-dir={}", profile.display()))
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            // No sync, no update checks, no GCM registration: a profile no
            // one signs in to has nothing to sync, and each of these logs
            // errors or phones home on every start.
            "--disable-background-networking",
            "--disable-sync",
            "--disable-features=Translate,MediaRouter,OptimizationHints",
            "--mute-audio",
            "--window-size=1280,800",
            // A screencast is drawn at the window's scale, not the one a tab
            // emulates: without this, the panel on a Retina screen showed a
            // picture of half its pixels, stretched, and text was soft. A
            // tab's own scale still decides what an agent's screenshot is.
            "--force-device-scale-factor=2",
            "about:blank",
        ])
        .stdin(Stdio::null())
        // Chrome is chatty on stderr (display links, GCM), none of it ours.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own group, so stopping it reaches the renderer and GPU
        // helpers it starts, not only the browser process.
        .process_group(0);

    {
        use std::os::fd::AsRawFd;
        let (read, write) = (commands_in.as_raw_fd(), answers_out.as_raw_fd());
        // SAFETY: between fork and exec only async-signal-safe calls are
        // made (fcntl, dup2), and nothing is allocated.
        unsafe {
            cmd.pre_exec(move || {
                // Moved out of the way first: either end may already be fd 3
                // or 4, and dup2 onto it would close the other. The copies
                // close on exec; dup2 clears that flag on 3 and 4 only.
                let r = libc::fcntl(read, libc::F_DUPFD_CLOEXEC, 10);
                let w = libc::fcntl(write, libc::F_DUPFD_CLOEXEC, 10);
                if r < 0 || w < 0 || libc::dup2(r, 3) < 0 || libc::dup2(w, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| Error::Other(format!("could not start {}: {e}", binary.display())))?;
    // The child's ends, closed here: while the app held a copy of Chrome's
    // writing end, a Chrome that died never read as gone.
    drop(commands_in);
    drop(answers_out);
    Ok((Process { child }, commands_out, answers_in))
}

impl Process {
    /// Wait up to `grace` for it to exit on its own (it was asked to close),
    /// then kill its whole group. Blocking.
    pub fn stop(&mut self, grace: Duration) {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // The helpers may outlive the browser process; the group goes either way.
        // SAFETY: a signal to a process group the app started.
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        let _ = self.child.wait();
    }
}
