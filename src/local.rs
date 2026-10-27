//! This host's runners: what state each is in, and starting and stopping them.
//!
//! A runner is stopped by sending SIGINT to its Runner.Listener. The listener
//! then ends its session with GitHub, so the runner shows offline at once and
//! is given no more jobs. A plain kill leaves the session open for minutes.

use crate::config::Kind;
use crate::stages::State;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub trait Runners {
    fn state(&mut self, runner: &str) -> State;
    fn start(&mut self, runner: &str) -> Result<(), String>;
    fn stop(&mut self, runner: &str) -> Result<(), String>;
}

pub fn for_kind(kind: Kind, root: Option<PathBuf>) -> Box<dyn Runners> {
    match kind {
        Kind::Docker => Box::new(Docker),
        Kind::Native => Box::new(Native {
            root: root.unwrap_or_default(),
            sys: System::new(),
        }),
    }
}

/// Containers named after the runner. Create them with no restart policy:
/// this program decides when they run.
pub struct Docker;

impl Runners for Docker {
    fn state(&mut self, runner: &str) -> State {
        let running = match output(
            "docker",
            &[
                "inspect",
                "--type",
                "container",
                "-f",
                "{{.State.Running}}",
                runner,
            ],
        ) {
            Ok(out) => out.trim() == "true",
            Err(_) => return State::Down,
        };
        if !running {
            return State::Stopped;
        }
        match output("docker", &["top", runner]) {
            Ok(out) if out.contains("Runner.Worker") => State::Busy,
            Ok(_) => State::Idle,
            Err(_) => State::Down,
        }
    }

    fn start(&mut self, runner: &str) -> Result<(), String> {
        output("docker", &["start", runner]).map(drop)
    }

    fn stop(&mut self, runner: &str) -> Result<(), String> {
        // The pattern's brackets keep pkill from matching its own command line.
        let _ = output(
            "docker",
            &[
                "exec",
                runner,
                "pkill",
                "-INT",
                "-f",
                "[b]in/Runner.Listener",
            ],
        );
        // Marks it stopped in Docker; if the listener ignored INT, this ends it.
        output_within(STOP_WAIT, "docker", &["stop", "-t", "30", runner]).map(drop)
    }
}

/// actions-runner directories at <root>/<runner>, started with run.sh.
pub struct Native {
    root: PathBuf,
    sys: System,
}

impl Native {
    fn dir(&self, runner: &str) -> PathBuf {
        self.root.join(runner)
    }

    /// The pid of the runner's Runner.Listener and whether a Runner.Worker runs.
    fn processes(&mut self, dir: &Path) -> (Option<u32>, bool) {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::Always)
                .with_cmd(UpdateKind::Always),
        );
        let bin = std::fs::canonicalize(dir.join("bin")).unwrap_or_else(|_| dir.join("bin"));
        let (mut listener, mut worker) = (None, false);
        for (pid, p) in self.sys.processes() {
            // The executable, or for a script its first two arguments.
            let is = |name: &str| {
                let want = bin.join(name);
                p.exe() == Some(want.as_path())
                    || p.cmd().iter().take(2).any(|a| Path::new(a) == want)
            };
            if is("Runner.Listener") {
                listener = Some(pid.as_u32());
            } else if is("Runner.Worker") {
                worker = true;
            }
        }
        (listener, worker)
    }
}

impl Runners for Native {
    fn state(&mut self, runner: &str) -> State {
        let dir = self.dir(runner);
        if !dir.join("run.sh").is_file() || !dir.join(".runner").is_file() {
            return State::Down;
        }
        match self.processes(&dir) {
            (_, true) => State::Busy,
            (Some(_), false) => State::Idle,
            (None, false) => State::Stopped,
        }
    }

    fn start(&mut self, runner: &str) -> Result<(), String> {
        let dir = self.dir(runner);
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("staged-runner.log"))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let mut child = Command::new(dir.join("run.sh"))
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err)
            // Its own group, so a signal to this program does not reach it.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }

    fn stop(&mut self, runner: &str) -> Result<(), String> {
        let dir = self.dir(runner);
        match self.processes(&dir).0 {
            Some(pid) => output("kill", &["-INT", &pid.to_string()]).map(drop),
            None => Ok(()),
        }
    }
}

/// Longest a command may take; a hung docker CLI must not stop the loop.
const COMMAND_WAIT: Duration = Duration::from_secs(20);
/// docker stop waits up to 30 s for the runner, then kills it.
const STOP_WAIT: Duration = Duration::from_secs(45);

fn output(program: &str, args: &[&str]) -> Result<String, String> {
    output_within(COMMAND_WAIT, program, args)
}

fn output_within(limit: Duration, program: &str, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut out);
            }
            String::from_utf8_lossy(&out).into_owned()
        })
    };
    let stdout = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} {}: no answer in {limit:?}",
                    args.join(" ")
                ));
            }
            Err(e) => return Err(format!("{program}: {e}")),
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        Err(format!("{program} {}: {}", args.join(" "), stderr.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fake actions-runner: run.sh starts bin/Runner.Listener, which starts
    /// bin/Runner.Worker when the file `job` exists and exits on INT.
    fn fake_runner(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join(".runner"), "{}").unwrap();
        let scripts = [
            (
                "run.sh",
                "#!/bin/sh\nexec \"$(dirname \"$0\")/bin/Runner.Listener\"\n",
            ),
            (
                "bin/Runner.Listener",
                "#!/bin/sh\ntrap 'exit 0' INT\nwhile :; do\n  [ -f job ] && { rm job; \"$(dirname \"$0\")/Runner.Worker\" & }\n  sleep 0.1\ndone\n",
            ),
            ("bin/Runner.Worker", "#!/bin/sh\nsleep 30\n"),
        ];
        for (file, body) in scripts {
            let path = dir.join(file);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    fn eventually(runners: &mut Native, name: &str, want: State) {
        for _ in 0..50 {
            if runners.state(name) == want {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("{name} never became {want:?}, is {:?}", runners.state(name));
    }

    #[test]
    fn native_runner_starts_reports_busy_and_stops_on_int() {
        let root = std::env::temp_dir().join(format!("staged-runner-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = fake_runner(&root, "runner-A-1");
        let mut runners = Native {
            root: std::fs::canonicalize(&root).unwrap(),
            sys: System::new(),
        };
        assert_eq!(runners.state("runner-B-1"), State::Down);
        assert_eq!(runners.state("runner-A-1"), State::Stopped);
        runners.start("runner-A-1").unwrap();
        eventually(&mut runners, "runner-A-1", State::Idle);
        std::fs::write(dir.join("job"), "").unwrap();
        eventually(&mut runners, "runner-A-1", State::Busy);
        // Clean up the worker so only the listener is left to stop.
        let _ = Command::new("pkill")
            .args(["-f", &dir.join("bin/Runner.Worker").to_string_lossy()])
            .status();
        eventually(&mut runners, "runner-A-1", State::Idle);
        runners.stop("runner-A-1").unwrap();
        eventually(&mut runners, "runner-A-1", State::Stopped);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_hung_command_is_cut_off() {
        let t = Instant::now();
        let err = output_within(Duration::from_millis(300), "sleep", &["10"]).unwrap_err();
        assert!(err.contains("no answer"), "{err}");
        assert!(t.elapsed() < Duration::from_secs(3));
        assert_eq!(output("echo", &["hi"]).unwrap(), "hi\n");
        assert!(output("false", &[]).is_err());
    }
}
