use crate::http;
use crate::process::{
    Account, ServerProcess, binary_version, check_version, create_scratch_dir, describe_exit,
    find_binary, log_tail,
};
use crate::{ChSpec, ScratchError};
use nix::sys::signal::Signal;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const DEFAULT_CLUSTER: &str = "openmig";
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const START_ATTEMPTS: usize = 3;
const LOG_LINES: usize = 40;
const CACHE_SIZE: u64 = 64 << 20;
const KEEPER_PROBE: &str = "SELECT count() FROM system.zookeeper WHERE path = '/'";

/// A single-node ClickHouse server with embedded Keeper, a one-shard cluster and macros.
pub struct ClickHouseServer {
    process: ServerProcess,
    dir: TempDir,
    url: String,
    version: String,
    ports: Ports,
    cluster: String,
}

#[derive(Clone, Copy, Debug)]
struct Ports {
    http: u16,
    tcp: u16,
    interserver: u16,
    keeper: u16,
    raft: u16,
}

impl Ports {
    /// Free ports chosen by the kernel. They are released before the server binds them, so another
    /// process can take one in between; startup is retried with new ports in that case.
    fn pick() -> io::Result<Self> {
        let listeners = (0..5)
            .map(|_| TcpListener::bind(("127.0.0.1", 0)))
            .collect::<io::Result<Vec<_>>>()?;
        let ports = listeners
            .iter()
            .map(|listener| Ok(listener.local_addr()?.port()))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Ports {
            http: ports[0],
            tcp: ports[1],
            interserver: ports[2],
            keeper: ports[3],
            raft: ports[4],
        })
    }
}

/// Resolved server configuration.
struct Config {
    cluster: String,
    macros: BTreeMap<String, String>,
    storage_policies: BTreeSet<String>,
    profile_settings: BTreeMap<String, String>,
    extra_config: Option<PathBuf>,
}

impl Config {
    fn new(spec: &ChSpec) -> Result<Self, ScratchError> {
        let cluster = spec
            .cluster
            .clone()
            .or_else(|| spec.macros.get("cluster").cloned())
            .unwrap_or_else(|| DEFAULT_CLUSTER.to_owned());
        let mut macros = BTreeMap::from([
            ("shard".to_owned(), "01".to_owned()),
            ("replica".to_owned(), "r1".to_owned()),
            ("cluster".to_owned(), cluster.clone()),
        ]);
        macros.extend(spec.macros.clone());
        let storage_policies: BTreeSet<String> = spec
            .storage_policies
            .iter()
            .filter(|name| *name != "default")
            .cloned()
            .collect();
        for (what, name) in [("cluster", &cluster)]
            .into_iter()
            .chain(macros.keys().map(|key| ("macro", key)))
            .chain(storage_policies.iter().map(|name| ("storage policy", name)))
            .chain(
                spec.profile_settings
                    .keys()
                    .map(|key| ("profile setting", key)),
            )
        {
            if !is_xml_name(name) {
                return Err(ScratchError::InvalidSpec(format!(
                    "{what} name `{name}` must start with a letter or `_` and contain only letters, digits, `_`, `-` and `.`"
                )));
            }
        }
        Ok(Config {
            cluster,
            macros,
            storage_policies,
            profile_settings: spec.profile_settings.clone(),
            extra_config: spec.extra_config.clone(),
        })
    }
}

impl ClickHouseServer {
    pub async fn start(spec: &ChSpec) -> Result<Self, ScratchError> {
        let started = Instant::now();
        let config = Config::new(spec)?;
        let binary = match &spec.bin {
            Some(bin) => find_binary(&bin.to_string_lossy(), None)?,
            None => find_binary("clickhouse", None)?,
        };
        let version = binary_version(&binary).await?;
        check_version(spec.version, &version)?;
        let account = Account::resolve(spec.run_as.as_deref(), None)?;

        let mut attempt = 1;
        loop {
            match Self::start_once(&binary, &version, &config, account.as_ref()).await {
                Ok(server) => {
                    tracing::debug!(
                        dir = %server.dir.path().display(),
                        elapsed = ?started.elapsed(),
                        "clickhouse scratch server ready"
                    );
                    return Ok(server);
                }
                Err(error) if attempt < START_ATTEMPTS && is_port_conflict(&error) => {
                    tracing::debug!(%error, attempt, "clickhouse port conflict, retrying");
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn start_once(
        binary: &Path,
        version: &str,
        config: &Config,
        account: Option<&Account>,
    ) -> Result<Self, ScratchError> {
        let dir = create_scratch_dir("openmig-ch-")?;
        let ports = Ports::pick()?;
        write_files(dir.path(), &ports, config, account)?;
        let mut cmd = Command::new(binary);
        cmd.arg("server")
            .arg(format!(
                "--config-file={}",
                dir.path().join("config.xml").display()
            ))
            .current_dir(dir.path())
            .env("HOME", dir.path())
            // Without the watchdog the server is a single process: the one we supervise.
            .env("CLICKHOUSE_WATCHDOG_ENABLE", "0")
            .stdin(Stdio::null())
            .stdout(File::create(dir.path().join("stdout.log"))?)
            .stderr(File::create(dir.path().join("stderr.log"))?);
        if let Some(account) = account {
            account.apply(&mut cmd);
        }
        let process = ServerProcess::spawn(cmd)?;

        let mut server = ClickHouseServer {
            url: format!("http://default@127.0.0.1:{}/", ports.http),
            process,
            dir,
            version: version.to_owned(),
            ports,
            cluster: config.cluster.clone(),
        };
        server.wait_ready().await?;
        Ok(server)
    }

    async fn wait_ready(&mut self) -> Result<(), ScratchError> {
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut serving = false;
        loop {
            if let Some(status) = self.process.exit_status()? {
                return Err(ScratchError::StartFailed {
                    message: format!("clickhouse server {}", describe_exit(status)),
                    log: self.log_tail(),
                });
            }
            if !serving {
                serving = self
                    .probe("GET", "/ping", "")
                    .await
                    .is_some_and(|body| body.trim() == "Ok.");
            }
            if serving
                && self
                    .probe("POST", "/", KEEPER_PROBE)
                    .await
                    .is_some_and(|body| body.trim().parse::<u64>().is_ok())
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

    async fn probe(&self, method: &str, target: &str, body: &str) -> Option<String> {
        let request = http::request(self.ports.http, method, target, body);
        match tokio::time::timeout(PROBE_TIMEOUT, request).await {
            Ok(Ok(response)) if response.status == 200 => Some(response.body),
            _ => None,
        }
    }

    fn log_tail(&self) -> String {
        let root = self.dir.path();
        log_tail(
            &[
                root.join("clickhouse-server.err.log"),
                root.join("stderr.log"),
            ],
            LOG_LINES,
        )
    }

    /// `http://default@127.0.0.1:<http port>/`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Server version reported by `clickhouse --version`, e.g. `26.5.1.882`.
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn http_port(&self) -> u16 {
        self.ports.http
    }

    pub fn tcp_port(&self) -> u16 {
        self.ports.tcp
    }

    pub fn keeper_port(&self) -> u16 {
        self.ports.keeper
    }

    /// Name of the single-node cluster (also the `cluster` macro unless overridden).
    pub fn cluster(&self) -> &str {
        &self.cluster
    }

    /// Directory holding the configuration, data, Keeper state and logs.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    pub fn pid(&self) -> u32 {
        self.process.id()
    }

    /// Runs one statement as `default` over HTTP and returns the response body (TabSeparated unless
    /// the statement says otherwise). Meant for administration and tests, not bulk data.
    pub async fn query(&self, sql: &str) -> Result<String, ScratchError> {
        let response = http::request(self.ports.http, "POST", "/?wait_end_of_query=1", sql).await?;
        if response.status == 200 {
            Ok(response.body)
        } else {
            Err(ScratchError::Query {
                status: response.status,
                message: response.body.trim_end().to_owned(),
            })
        }
    }

    /// Graceful shutdown (SIGTERM), then removal of the directory.
    pub async fn stop(self) -> Result<(), ScratchError> {
        let ClickHouseServer {
            mut process, dir, ..
        } = self;
        process.stop(Signal::SIGTERM, STOP_TIMEOUT).await?;
        drop(process);
        dir.close()?;
        Ok(())
    }
}

fn write_files(
    root: &Path,
    ports: &Ports,
    config: &Config,
    account: Option<&Account>,
) -> Result<(), ScratchError> {
    for sub in [
        "data",
        "tmp",
        "user_files",
        "format_schemas",
        "access",
        "keeper/log",
        "keeper/snapshots",
        "config.d",
    ] {
        fs::create_dir_all(root.join(sub))?;
    }
    fs::write(root.join("config.xml"), server_config(root, ports, config))?;
    fs::write(
        root.join("users.xml"),
        users_config(&config.profile_settings),
    )?;
    if let Some(extra) = &config.extra_config {
        // ClickHouse only merges config.d files with these extensions.
        let name = match extra.extension().and_then(|ext| ext.to_str()) {
            Some(ext @ ("yaml" | "yml")) => format!("extra.{ext}"),
            _ => "extra.xml".to_owned(),
        };
        fs::copy(extra, root.join("config.d").join(name)).map_err(|error| {
            ScratchError::InvalidSpec(format!(
                "cannot read extra_config `{}`: {error}",
                extra.display()
            ))
        })?;
    }
    if let Some(account) = account {
        account.chown_tree(root)?;
    }
    Ok(())
}

fn is_port_conflict(error: &ScratchError) -> bool {
    match error {
        ScratchError::StartFailed { log, .. } | ScratchError::Timeout { log, .. } => {
            log.contains("Address already in use")
        }
        _ => false,
    }
}

fn is_xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

// System log tables are left out entirely: ClickHouse only creates the ones present in the config.
// `<query_log remove="1"/>` is only meaningful in config.d overrides; in the main file it would
// define (and so enable) the table.
fn server_config(dir: &Path, ports: &Ports, config: &Config) -> String {
    let dir = escape(&dir.to_string_lossy());
    let Ports {
        http,
        tcp,
        interserver,
        keeper,
        raft,
    } = *ports;
    let cluster = &config.cluster;
    let mut macros = String::new();
    for (key, value) in &config.macros {
        let _ = writeln!(macros, "    <{key}>{}</{key}>", escape(value));
    }
    let mut policies = String::new();
    for name in &config.storage_policies {
        let _ = writeln!(
            policies,
            "      <{name}><volumes><default><disk>default</disk></default></volumes></{name}>"
        );
    }
    let storage = if policies.is_empty() {
        String::new()
    } else {
        format!(
            "  <storage_configuration>\n    <policies>\n{policies}    </policies>\n  </storage_configuration>\n"
        )
    };
    format!(
        r#"<clickhouse>
  <logger>
    <level>warning</level>
    <log>{dir}/clickhouse-server.log</log>
    <errorlog>{dir}/clickhouse-server.err.log</errorlog>
    <console>0</console>
  </logger>
  <listen_host>127.0.0.1</listen_host>
  <http_port>{http}</http_port>
  <tcp_port>{tcp}</tcp_port>
  <interserver_http_port>{interserver}</interserver_http_port>
  <interserver_http_host>127.0.0.1</interserver_http_host>
  <path>{dir}/data/</path>
  <tmp_path>{dir}/tmp/</tmp_path>
  <user_files_path>{dir}/user_files/</user_files_path>
  <format_schema_path>{dir}/format_schemas/</format_schema_path>
  <timezone>UTC</timezone>
  <send_crash_reports><enabled>false</enabled></send_crash_reports>
  <max_server_memory_usage_to_ram_ratio>0.5</max_server_memory_usage_to_ram_ratio>
  <mark_cache_size>{CACHE_SIZE}</mark_cache_size>
  <uncompressed_cache_size>{CACHE_SIZE}</uncompressed_cache_size>
  <index_mark_cache_size>{CACHE_SIZE}</index_mark_cache_size>
  <index_uncompressed_cache_size>{CACHE_SIZE}</index_uncompressed_cache_size>
  <mmap_cache_size>1024</mmap_cache_size>
  <background_schedule_pool_size>16</background_schedule_pool_size>
  <background_distributed_schedule_pool_size>4</background_distributed_schedule_pool_size>
  <background_message_broker_schedule_pool_size>2</background_message_broker_schedule_pool_size>
  <background_buffer_flush_schedule_pool_size>2</background_buffer_flush_schedule_pool_size>
  <keeper_server>
    <tcp_port>{keeper}</tcp_port>
    <server_id>1</server_id>
    <log_storage_path>{dir}/keeper/log/</log_storage_path>
    <snapshot_storage_path>{dir}/keeper/snapshots/</snapshot_storage_path>
    <coordination_settings>
      <force_sync>false</force_sync>
      <operation_timeout_ms>10000</operation_timeout_ms>
      <session_timeout_ms>30000</session_timeout_ms>
      <heart_beat_interval_ms>100</heart_beat_interval_ms>
      <election_timeout_lower_bound_ms>200</election_timeout_lower_bound_ms>
      <election_timeout_upper_bound_ms>400</election_timeout_upper_bound_ms>
    </coordination_settings>
    <raft_configuration>
      <server>
        <id>1</id>
        <hostname>127.0.0.1</hostname>
        <port>{raft}</port>
      </server>
    </raft_configuration>
  </keeper_server>
  <zookeeper>
    <node>
      <host>127.0.0.1</host>
      <port>{keeper}</port>
    </node>
  </zookeeper>
  <macros>
{macros}  </macros>
  <remote_servers>
    <{cluster}>
      <shard>
        <internal_replication>true</internal_replication>
        <replica>
          <host>127.0.0.1</host>
          <port>{tcp}</port>
        </replica>
      </shard>
    </{cluster}>
  </remote_servers>
  <distributed_ddl>
    <path>/clickhouse/task_queue/ddl</path>
  </distributed_ddl>
  <keeper_map_path_prefix>/keeper_map_tables</keeper_map_path_prefix>
  <user_directories>
    <users_xml>
      <path>{dir}/users.xml</path>
    </users_xml>
    <local_directory>
      <path>{dir}/access/</path>
    </local_directory>
    <replicated>
      <zookeeper_path>/clickhouse/access</zookeeper_path>
    </replicated>
  </user_directories>
{storage}  <access_control_improvements>
    <enable_read_write_grants>true</enable_read_write_grants>
  </access_control_improvements>
</clickhouse>
"#
    )
}

fn users_config(profile_settings: &BTreeMap<String, String>) -> String {
    let mut settings = String::new();
    for (key, value) in profile_settings {
        let _ = writeln!(settings, "      <{key}>{}</{key}>", escape(value));
    }
    format!(
        r#"<clickhouse>
  <profiles>
    <default>
{settings}    </default>
  </profiles>
  <users>
    <default>
      <password></password>
      <networks>
        <ip>::/0</ip>
      </networks>
      <profile>default</profile>
      <access_management>1</access_management>
      <named_collection_control>1</named_collection_control>
      <show_named_collections>1</show_named_collections>
      <show_named_collections_secrets>1</show_named_collections_secrets>
    </default>
  </users>
</clickhouse>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ChSpec {
        ChSpec {
            storage_policies: vec!["encrypted".to_owned(), "default".to_owned()],
            profile_settings: BTreeMap::from([(
                "allow_experimental_time_series_table".to_owned(),
                "1".to_owned(),
            )]),
            ..ChSpec::default()
        }
    }

    #[test]
    fn config_defaults() {
        let config = Config::new(&spec()).unwrap();
        assert_eq!(config.cluster, "openmig");
        assert_eq!(
            config.macros,
            BTreeMap::from([
                ("cluster".to_owned(), "openmig".to_owned()),
                ("replica".to_owned(), "r1".to_owned()),
                ("shard".to_owned(), "01".to_owned()),
            ])
        );
        assert_eq!(
            config.storage_policies,
            BTreeSet::from(["encrypted".to_owned()])
        );
    }

    #[test]
    fn cluster_follows_macro() {
        let spec = ChSpec {
            macros: BTreeMap::from([
                ("cluster".to_owned(), "wax".to_owned()),
                ("replica".to_owned(), "0".to_owned()),
            ]),
            ..ChSpec::default()
        };
        let config = Config::new(&spec).unwrap();
        assert_eq!(config.cluster, "wax");
        assert_eq!(config.macros["replica"], "0");
        assert_eq!(config.macros["shard"], "01");
    }

    #[test]
    fn rejects_names_that_are_not_xml_elements() {
        let spec = ChSpec {
            storage_policies: vec!["bad name".to_owned()],
            ..ChSpec::default()
        };
        assert!(matches!(
            Config::new(&spec),
            Err(ScratchError::InvalidSpec(_))
        ));
    }

    #[test]
    fn detects_port_conflicts() {
        let failed = |log: &str| ScratchError::StartFailed {
            message: "clickhouse server exited with status 210".to_owned(),
            log: log.to_owned(),
        };
        assert!(is_port_conflict(&failed(
            "Listen [127.0.0.1]:9000 failed: Poco::Exception. Code: 1000, e.code() = 98, Net Exception: Address already in use"
        )));
        assert!(!is_port_conflict(&failed("Unknown setting 'x'")));
        assert!(!is_port_conflict(&ScratchError::UnknownUser(
            "x".to_owned()
        )));
    }

    #[test]
    fn renders_config() {
        let config = Config::new(&spec()).unwrap();
        let ports = Ports {
            http: 1,
            tcp: 2,
            interserver: 3,
            keeper: 4,
            raft: 5,
        };
        let xml = server_config(Path::new("/tmp/openmig-ch-x"), &ports, &config);
        assert!(xml.contains(
            "<encrypted><volumes><default><disk>default</disk></default></volumes></encrypted>"
        ));
        assert!(!xml.contains("<default><volumes>"));
        assert!(xml.contains("<cluster>openmig</cluster>"));
        assert!(xml.contains("<openmig>\n      <shard>"));
        assert!(xml.contains("<path>/tmp/openmig-ch-x/data/</path>"));
        let users = users_config(&config.profile_settings);
        assert!(users.contains(
            "<allow_experimental_time_series_table>1</allow_experimental_time_series_table>"
        ));
        assert_eq!(escape("a<b>&'\""), "a&lt;b&gt;&amp;&apos;&quot;");
    }
}
