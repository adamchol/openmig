//! Child process supervision shared by the scratch servers.

use crate::ScratchError;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::{Gid, Pid, Uid, User, geteuid};
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, lchown};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const PID_FILE: &str = "openmig.pid";
const DIR_PREFIXES: [&str; 2] = ["openmig-pg-", "openmig-ch-"];
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Directory that holds scratch server directories.
///
/// `/tmp` rather than `$TMPDIR`: unix socket paths are limited to about 107 bytes and `$TMPDIR` can
/// be long (macOS, nested sandboxes).
pub(crate) fn base_dir() -> PathBuf {
    let tmp = Path::new("/tmp");
    if tmp.is_dir() {
        tmp.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

/// Creates `<base>/<prefix>XXXXXX` (mode 0755) with a pid file naming this process, after removing
/// directories left behind by dead OpenMig processes.
pub(crate) fn create_scratch_dir(prefix: &str) -> io::Result<TempDir> {
    let base = base_dir();
    remove_stale_dirs(&base);
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .rand_bytes(6)
        .tempdir_in(&base)?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755))?;
    fs::write(
        dir.path().join(PID_FILE),
        format!("{}\n", std::process::id()),
    )?;
    Ok(dir)
}

/// Best-effort removal of scratch directories whose pid file names a process that no longer exists.
/// Their servers died with their parent (parent-death signal), only the files remain.
fn remove_stale_dirs(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !DIR_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = entry.path();
        let Ok(content) = fs::read_to_string(path.join(PID_FILE)) else {
            continue;
        };
        let Ok(pid) = content.trim().parse::<i32>() else {
            continue;
        };
        if pid > 0 && !process_exists(pid) {
            tracing::debug!(dir = %path.display(), pid, "removing stale scratch directory");
            let _ = fs::remove_dir_all(&path);
        }
    }
}

fn process_exists(pid: i32) -> bool {
    !matches!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH))
}

/// Unprivileged account a server runs as.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Account {
    uid: Uid,
    gid: Gid,
}

impl Account {
    /// The account for a server: `None` runs it as the current user. Switching users is only
    /// possible (and only needed) when OpenMig runs as root.
    pub(crate) fn resolve(
        run_as: Option<&str>,
        default: Option<&str>,
    ) -> Result<Option<Self>, ScratchError> {
        if !geteuid().is_root() {
            return Ok(None);
        }
        let Some(name) = run_as.or(default) else {
            return Ok(None);
        };
        match User::from_name(name) {
            Ok(Some(user)) => Ok(Some(Account {
                uid: user.uid,
                gid: user.gid,
            })),
            Ok(None) => Err(ScratchError::UnknownUser(name.to_owned())),
            Err(errno) => Err(ScratchError::Io(errno.into())),
        }
    }

    pub(crate) fn apply(&self, cmd: &mut Command) {
        cmd.uid(self.uid.as_raw()).gid(self.gid.as_raw());
    }

    pub(crate) fn chown(&self, path: &Path) -> io::Result<()> {
        lchown(path, Some(self.uid.as_raw()), Some(self.gid.as_raw()))
    }

    pub(crate) fn chown_tree(&self, path: &Path) -> io::Result<()> {
        self.chown(path)?;
        if fs::symlink_metadata(path)?.is_dir() {
            for entry in fs::read_dir(path)? {
                self.chown_tree(&entry?.path())?;
            }
        }
        Ok(())
    }
}

/// Finds `name` in `dir` when given, otherwise on `PATH` (`name` may also be a path).
pub(crate) fn find_binary(name: &str, dir: Option<&Path>) -> Result<PathBuf, ScratchError> {
    let (found, searched) = match dir {
        Some(dir) => {
            let candidate = dir.join(name);
            (
                candidate.is_file().then_some(candidate),
                dir.display().to_string(),
            )
        }
        None => (which::which(name).ok(), "PATH".to_owned()),
    };
    found.ok_or_else(|| ScratchError::BinaryNotFound {
        binary: name.to_owned(),
        searched,
    })
}

/// Runs `<binary> --version` and returns the first dotted version number in its output.
pub(crate) async fn binary_version(binary: &Path) -> Result<String, ScratchError> {
    let mut cmd = Command::new(binary);
    cmd.arg("--version").stdin(Stdio::null());
    let output = blocking(move || cmd.output()).await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(ScratchError::StartFailed {
            message: format!(
                "`{} --version` {}",
                binary.display(),
                describe_exit(output.status)
            ),
            log: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    parse_version(&stdout).ok_or_else(|| ScratchError::StartFailed {
        message: format!(
            "could not read the version from `{} --version`",
            binary.display()
        ),
        log: stdout.into_owned(),
    })
}

fn parse_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|token| token.starts_with(|c: char| c.is_ascii_digit()))
        .map(|token| {
            token
                .trim_end_matches(|c: char| !c.is_ascii_digit())
                .to_owned()
        })
}

/// Leading number of a version string: `17` for `17.11`, `26` for `26.5.1.882`.
pub(crate) fn major_version(version: &str) -> Option<u32> {
    let digits: String = version.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

pub(crate) fn check_version(expected: Option<u32>, found: &str) -> Result<(), ScratchError> {
    match expected {
        Some(expected) if major_version(found) != Some(expected) => {
            Err(ScratchError::VersionMismatch {
                expected,
                found: found.to_owned(),
            })
        }
        _ => Ok(()),
    }
}

/// Runs blocking work (child processes that run to completion, file copies) off the async
/// executor.
pub(crate) async fn blocking<T, E, F>(work: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: From<io::Error> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| E::from(io::Error::other(error)))?
}

pub(crate) fn describe_exit(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exited with status {code}"),
        (None, Some(signal)) => match Signal::try_from(signal) {
            Ok(signal) => format!("was killed by {signal}"),
            Err(_) => format!("was killed by signal {signal}"),
        },
        (None, None) => "exited".to_owned(),
    }
}

/// Last `lines` lines of each existing, non-empty file, without stack trace frames (`12. DB::...`)
/// which would otherwise push the actual error out of the tail.
pub(crate) fn log_tail(files: &[PathBuf], lines: usize) -> String {
    let mut out = String::new();
    for file in files {
        let Ok(content) = fs::read(file) else {
            continue;
        };
        let content = String::from_utf8_lossy(&content);
        let all: Vec<&str> = content
            .lines()
            .filter(|line| !is_stack_frame(line) && !line.trim().is_empty())
            .collect();
        if all.is_empty() {
            continue;
        }
        let tail = &all[all.len().saturating_sub(lines)..];
        out.push_str(&format!("==> {} <==\n", file.display()));
        for line in tail {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn is_stack_frame(line: &str) -> bool {
    let line = line.trim_start();
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && line[digits..].starts_with(". ")
}

type SpawnRequest = (Command, mpsc::Sender<io::Result<Child>>);

/// Forks `cmd` from a thread that lives as long as the process.
///
/// `PR_SET_PDEATHSIG` fires when the *thread* that forked the child exits, not the process. Servers
/// outlive the task and the thread that started them (tests share servers across runtimes), so they
/// must not be forked from a runtime or test thread.
fn spawn_from_spawner_thread(cmd: Command) -> io::Result<Child> {
    static SPAWNER: OnceLock<Option<mpsc::Sender<SpawnRequest>>> = OnceLock::new();
    let spawner = SPAWNER.get_or_init(|| {
        let (requests, incoming) = mpsc::channel::<SpawnRequest>();
        std::thread::Builder::new()
            .name("openmig-scratch-spawner".to_owned())
            .spawn(move || {
                for (mut cmd, reply) in incoming {
                    let _ = reply.send(cmd.spawn());
                }
            })
            .ok()
            .map(|_| requests)
    });
    let spawner = spawner
        .as_ref()
        .ok_or_else(|| io::Error::other("could not start the scratch spawner thread"))?;
    let (reply, response) = mpsc::channel();
    spawner
        .send((cmd, reply))
        .map_err(|_| io::Error::other("the scratch spawner thread is gone"))?;
    response
        .recv()
        .map_err(|_| io::Error::other("the scratch spawner thread is gone"))?
}

/// A server process running in its own process group, killed when OpenMig dies.
pub(crate) struct ServerProcess {
    child: Child,
    status: Option<ExitStatus>,
    killed: bool,
}

impl ServerProcess {
    pub(crate) fn spawn(mut cmd: Command) -> io::Result<Self> {
        cmd.process_group(0);
        set_parent_death_signal(&mut cmd);
        let child = spawn_from_spawner_thread(cmd)?;
        Ok(ServerProcess {
            child,
            status: None,
            killed: false,
        })
    }

    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    fn group(&self) -> Pid {
        Pid::from_raw(self.child.id() as i32)
    }

    /// `Some` once the process has exited (the process is reaped).
    pub(crate) fn exit_status(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status)
    }

    fn signal_group(&self, signal: Signal) {
        match killpg(self.group(), signal) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(errno) => tracing::debug!(%errno, %signal, "could not signal scratch server"),
        }
    }

    /// Sends `signal` to the process group, waits up to `grace` for the leader to exit, then kills
    /// whatever is left of the group.
    pub(crate) async fn stop(&mut self, signal: Signal, grace: Duration) -> io::Result<()> {
        if self.exit_status()?.is_none() {
            self.signal_group(signal);
            let deadline = Instant::now() + grace;
            while self.exit_status()?.is_none() && Instant::now() < deadline {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
        self.kill();
        Ok(())
    }

    /// The group can outlive its leader (PostgreSQL backends), so it is killed even when the
    /// leader has already exited.
    fn kill(&mut self) {
        if self.killed {
            return;
        }
        self.killed = true;
        self.signal_group(Signal::SIGKILL);
        if self.status.is_none() {
            self.status = self.child.wait().ok();
        }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn set_parent_death_signal(cmd: &mut Command) {
    let parent = std::process::id();
    // SAFETY: the hook runs in the forked child before exec and only makes the prctl(2) and
    // getppid(2) system calls, which are async-signal-safe; it neither allocates nor locks.
    // std applies `uid`/`gid` before running pre-exec hooks, and credential changes clear the
    // parent-death signal, so it has to be set here rather than earlier.
    unsafe {
        cmd.pre_exec(move || {
            nix::sys::prctl::set_pdeathsig(Signal::SIGKILL)?;
            // The parent may have died between fork and prctl; then nothing would kill us.
            if nix::unistd::getppid().as_raw() as u32 != parent {
                return Err(Errno::ESRCH.into());
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn set_parent_death_signal(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(
            parse_version("postgres (PostgreSQL) 17.11\n").as_deref(),
            Some("17.11")
        );
        assert_eq!(
            parse_version("postgres (PostgreSQL) 16.4 (Debian 16.4-1.pgdg120+2)").as_deref(),
            Some("16.4")
        );
        assert_eq!(
            parse_version("ClickHouse local version 26.5.1.882.\n").as_deref(),
            Some("26.5.1.882")
        );
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(major_version("17.11"), Some(17));
        assert_eq!(major_version("18beta1"), Some(18));
        assert_eq!(major_version("26.5.1.882"), Some(26));
    }

    #[test]
    fn version_check() {
        assert!(check_version(None, "17.11").is_ok());
        assert!(check_version(Some(17), "17.11").is_ok());
        assert!(matches!(
            check_version(Some(16), "17.11"),
            Err(ScratchError::VersionMismatch { expected: 16, .. })
        ));
    }

    #[test]
    fn stale_directories_are_removed() {
        let base = tempfile::tempdir().unwrap();
        let dead = base.path().join("openmig-pg-dead00");
        let alive = base.path().join("openmig-ch-alive0");
        let unrelated = base.path().join("other-dir");
        let no_pid = base.path().join("openmig-pg-nopid0");
        for dir in [&dead, &alive, &unrelated, &no_pid] {
            fs::create_dir(dir).unwrap();
        }
        let mut exited = Command::new("true").spawn().unwrap();
        let dead_pid = exited.id();
        exited.wait().unwrap();
        fs::write(dead.join(PID_FILE), format!("{dead_pid}\n")).unwrap();
        fs::write(alive.join(PID_FILE), format!("{}\n", std::process::id())).unwrap();
        fs::write(unrelated.join(PID_FILE), format!("{dead_pid}\n")).unwrap();

        remove_stale_dirs(base.path());

        assert!(!dead.exists());
        assert!(alive.exists());
        assert!(unrelated.exists());
        assert!(no_pid.exists());
    }

    #[test]
    fn log_tail_keeps_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("server.log");
        fs::write(&log, "1\n2\n3\n0. DB::Exception\n 1. main @ 0x0\n\n4\n").unwrap();
        let tail = log_tail(&[log.clone(), dir.path().join("missing.log")], 2);
        assert_eq!(tail, format!("==> {} <==\n3\n4\n", log.display()));
    }
}
