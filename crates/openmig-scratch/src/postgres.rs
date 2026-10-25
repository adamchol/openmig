use crate::process::{
    Account, ServerProcess, binary_version, blocking, check_version, create_scratch_dir,
    describe_exit, find_binary, log_tail,
};
use crate::{PgSpec, ScratchError};
use etcetera::BaseStrategy;
use nix::sys::signal::Signal;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const SUPERUSER: &str = "openmig";
/// Fixed: every server listens only on a unix socket in its own directory.
const PORT: u16 = 5432;
const DEFAULT_RUN_AS: &str = "nobody";
#[cfg(not(target_os = "macos"))]
const DEFAULT_INITDB_ARGS: [&str; 2] = ["--encoding=UTF8", "--locale=C.UTF-8"];
// macOS has no `C.UTF-8` locale; `C` sorts the same way.
#[cfg(target_os = "macos")]
const DEFAULT_INITDB_ARGS: [&str; 2] = ["--encoding=UTF8", "--locale=C"];
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const LOG_LINES: usize = 40;
const CACHE_FORMAT: &str = "openmig-pg-cache-1";

/// A private PostgreSQL cluster listening on a unix socket in a temporary directory.
pub struct PostgresServer {
    process: ServerProcess,
    dir: TempDir,
    url: String,
    version: String,
    bin_dir: PathBuf,
}

impl PostgresServer {
    pub async fn start(spec: &PgSpec) -> Result<Self, ScratchError> {
        let started = Instant::now();
        let initdb = find_binary("initdb", spec.bin_dir.as_deref())?;
        let postgres = find_binary("postgres", spec.bin_dir.as_deref())?;
        let version = binary_version(&postgres).await?;
        check_version(spec.version, &version)?;
        let account = Account::resolve(spec.run_as.as_deref(), Some(DEFAULT_RUN_AS))?;
        let initdb_args = spec.initdb_args.clone().unwrap_or_else(|| {
            DEFAULT_INITDB_ARGS
                .iter()
                .map(|arg| (*arg).to_owned())
                .collect()
        });

        let dir = create_scratch_dir("openmig-pg-")?;
        if let Some(account) = &account {
            account.chown(dir.path())?;
        }
        let cache = cache_entry(&initdb, &postgres, &version, &initdb_args);
        let source = {
            let dir = dir.path().to_path_buf();
            blocking(move || {
                prepare_data_dir(
                    &initdb,
                    &initdb_args,
                    &dir,
                    cache.as_deref(),
                    account.as_ref(),
                )
            })
            .await?
        };
        tracing::debug!(source, elapsed = ?started.elapsed(), "postgres data directory ready");

        let cmd = server_command(&postgres, dir.path(), &spec.settings, account.as_ref())?;
        let process = ServerProcess::spawn(cmd)?;
        let mut server = PostgresServer {
            url: url(dir.path()),
            process,
            dir,
            version,
            bin_dir: postgres.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        server.wait_ready().await?;
        tracing::debug!(
            dir = %server.dir.path().display(),
            elapsed = ?started.elapsed(),
            "postgres scratch server ready"
        );
        Ok(server)
    }

    async fn wait_ready(&mut self) -> Result<(), ScratchError> {
        let deadline = Instant::now() + READY_TIMEOUT;
        let pid_file = self.data_dir().join("postmaster.pid");
        loop {
            if let Some(status) = self.process.exit_status()? {
                return Err(ScratchError::StartFailed {
                    message: format!("postgres {}", describe_exit(status)),
                    log: self.log_tail(),
                });
            }
            // The postmaster writes its status on the 8th line of postmaster.pid.
            if fs::read_to_string(&pid_file)
                .is_ok_and(|content| content.lines().nth(7).map(str::trim) == Some("ready"))
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ScratchError::Timeout {
                    seconds: READY_TIMEOUT.as_secs(),
                    log: self.log_tail(),
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    fn log_tail(&self) -> String {
        log_tail(&[self.dir.path().join("postgres.log")], LOG_LINES)
    }

    /// `postgresql:///postgres?host=<socket dir>&port=5432&user=openmig`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Server version reported by `postgres --version`, e.g. `17.11`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Directory holding the unix socket, the data directory and the server log.
    pub fn socket_dir(&self) -> &Path {
        self.dir.path()
    }

    pub fn port(&self) -> u16 {
        PORT
    }

    /// Bootstrap superuser (trust authentication).
    pub fn user(&self) -> &str {
        SUPERUSER
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// Directory of the `postgres` binary; client tools such as `psql` usually live next to it.
    pub fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }

    /// Process id of the postmaster.
    pub fn pid(&self) -> u32 {
        self.process.id()
    }

    /// Immediate shutdown (the data is throwaway), then removal of the directory.
    pub async fn stop(self) -> Result<(), ScratchError> {
        let PostgresServer {
            mut process, dir, ..
        } = self;
        process.stop(Signal::SIGQUIT, STOP_TIMEOUT).await?;
        drop(process);
        dir.close()?;
        Ok(())
    }
}

fn server_command(
    postgres: &Path,
    dir: &Path,
    settings: &BTreeMap<String, String>,
    account: Option<&Account>,
) -> io::Result<Command> {
    let log = File::create(dir.join("postgres.log"))?;
    let mut cmd = Command::new(postgres);
    cmd.arg("-D").arg(dir.join("data")).arg("-k").arg(dir);
    let defaults = [
        "listen_addresses=".to_owned(),
        format!("port={PORT}"),
        "fsync=off".to_owned(),
        "synchronous_commit=off".to_owned(),
        "full_page_writes=off".to_owned(),
        "max_connections=100".to_owned(),
        // Clients may run as another user than the server (root vs `run_as`).
        "unix_socket_permissions=0777".to_owned(),
    ];
    let configured = settings.iter().map(|(key, value)| format!("{key}={value}"));
    for setting in defaults.into_iter().chain(configured) {
        cmd.arg("-c").arg(setting);
    }
    cmd.current_dir(dir)
        .env("HOME", dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    if let Some(account) = account {
        account.apply(&mut cmd);
    }
    Ok(cmd)
}

fn url(socket_dir: &Path) -> String {
    format!(
        "postgresql:///postgres?host={}&port={PORT}&user={SUPERUSER}",
        encode_query_value(&socket_dir.to_string_lossy())
    )
}

fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Fills `<dir>/data` from the cache or with `initdb` (then caches the result). Returns where the
/// data directory came from.
fn prepare_data_dir(
    initdb: &Path,
    initdb_args: &[String],
    dir: &Path,
    cache: Option<&Path>,
    account: Option<&Account>,
) -> Result<&'static str, ScratchError> {
    let data = dir.join("data");
    if let Some(cached) = cache.map(|entry| entry.join("data"))
        && is_complete(&cached)
    {
        match copy_tree(&cached, &data, account) {
            Ok(()) => return Ok("cache"),
            Err(error) => {
                tracing::warn!(%error, cache = %cached.display(), "could not copy the cached postgres data directory");
                let _ = fs::remove_dir_all(&data);
            }
        }
    }

    fs::DirBuilder::new().mode(0o700).create(&data)?;
    if let Some(account) = account {
        account.chown(&data)?;
    }
    let log_path = dir.join("initdb.log");
    let log = File::create(&log_path)?;
    let mut cmd = Command::new(initdb);
    cmd.arg("-D")
        .arg(&data)
        .args(["-U", SUPERUSER, "--auth=trust", "-N"])
        .args(initdb_args)
        .current_dir(dir)
        .env("HOME", dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    if let Some(account) = account {
        account.apply(&mut cmd);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(ScratchError::StartFailed {
            message: format!("initdb {}", describe_exit(status)),
            log: log_tail(&[log_path], LOG_LINES),
        });
    }

    if let Some(entry) = cache
        && let Err(error) = populate_cache(&data, entry)
    {
        tracing::debug!(%error, cache = %entry.display(), "could not cache the postgres data directory");
    }
    Ok("initdb")
}

/// Environment that `initdb` bakes into the data directory when the arguments leave it open.
const INITDB_ENVIRONMENT: [&str; 4] = ["LC_ALL", "LC_CTYPE", "LANG", "TZ"];

/// Cache directory for one (binaries, version, initdb arguments, environment) combination, or
/// `None` when there is no cache directory.
fn cache_entry(
    initdb: &Path,
    postgres: &Path,
    version: &str,
    initdb_args: &[String],
) -> Option<PathBuf> {
    let base = etcetera::choose_base_strategy().ok()?.cache_dir();
    let mut hasher = Sha256::new();
    let initdb = fs::canonicalize(initdb).unwrap_or_else(|_| initdb.to_path_buf());
    let postgres = fs::canonicalize(postgres).unwrap_or_else(|_| postgres.to_path_buf());
    let environment: Vec<String> = INITDB_ENVIRONMENT
        .iter()
        .map(|name| format!("{name}={}", std::env::var(name).unwrap_or_default()))
        .collect();
    let parts = [
        CACHE_FORMAT.as_bytes(),
        initdb.as_os_str().as_encoded_bytes(),
        postgres.as_os_str().as_encoded_bytes(),
        version.as_bytes(),
    ];
    for part in parts
        .into_iter()
        .chain(environment.iter().map(|var| var.as_bytes()))
        .chain(initdb_args.iter().map(|arg| arg.as_bytes()))
    {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    let digest = hasher.finalize();
    Some(
        base.join("openmig")
            .join("pg")
            .join(hex::encode(&digest[..16])),
    )
}

fn is_complete(data: &Path) -> bool {
    data.join("PG_VERSION").is_file() && data.join("global").join("pg_control").is_file()
}

/// Copies a pristine data directory into `<entry>/data`. The copy is staged next to the target and
/// renamed into place, so concurrent processes never observe a partial cache entry.
fn populate_cache(data: &Path, entry: &Path) -> io::Result<()> {
    let target = entry.join("data");
    if is_complete(&target) {
        return Ok(());
    }
    fs::create_dir_all(entry)?;
    if target.exists() && !is_complete(&target) {
        fs::remove_dir_all(&target)?;
    }
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(entry)?;
    let staged = staging.path().join("data");
    copy_tree(data, &staged, None)?;
    match fs::rename(&staged, &target) {
        Ok(()) => Ok(()),
        Err(_) if is_complete(&target) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Recursive copy preserving permission bits, owned by `owner` (default: the current user).
fn copy_tree(src: &Path, dst: &Path, owner: Option<&Account>) -> io::Result<()> {
    let permissions = fs::metadata(src)?.permissions();
    fs::create_dir(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&entry.path(), &to, owner)?;
        } else if file_type.is_symlink() {
            symlink(fs::read_link(entry.path())?, &to)?;
        } else {
            fs::copy(entry.path(), &to)?;
        }
        if let Some(owner) = owner {
            owner.chown(&to)?;
        }
    }
    fs::set_permissions(dst, permissions)?;
    if let Some(owner) = owner {
        owner.chown(dst)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn url_uses_socket_dir() {
        assert_eq!(
            url(Path::new("/tmp/openmig-pg-abc123")),
            "postgresql:///postgres?host=/tmp/openmig-pg-abc123&port=5432&user=openmig"
        );
        assert_eq!(
            url(Path::new("/tmp/a b&c")),
            "postgresql:///postgres?host=/tmp/a%20b%26c&port=5432&user=openmig"
        );
    }

    #[test]
    fn cache_key_depends_on_arguments() {
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let a = cache_entry(
            Path::new("/bin/initdb"),
            Path::new("/bin/postgres"),
            "17.11",
            &args(&["--no-locale"]),
        );
        let b = cache_entry(
            Path::new("/bin/initdb"),
            Path::new("/bin/postgres"),
            "17.11",
            &args(&["--no-locale"]),
        );
        let c = cache_entry(
            Path::new("/bin/initdb"),
            Path::new("/bin/postgres"),
            "17.11",
            &args(&["--no-local", "e"]),
        );
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn copy_tree_preserves_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::DirBuilder::new().mode(0o700).create(&src).unwrap();
        fs::create_dir(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("file"), "x").unwrap();
        fs::set_permissions(
            src.join("sub").join("file"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let dst = tmp.path().join("dst");
        copy_tree(&src, &dst, None).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dst), 0o700);
        assert_eq!(mode(&dst.join("sub").join("file")), 0o600);
        assert_eq!(
            fs::read_to_string(dst.join("sub").join("file")).unwrap(),
            "x"
        );
    }
}
