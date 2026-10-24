use crate::cli::{GlobalArgs, LogFormat};
use tracing_subscriber::EnvFilter;

/// Logs go to stderr. `RUST_LOG` overrides the level derived from `-v`/`-q`.
pub fn init(global: &GlobalArgs, color: bool) {
    let level = match (global.quiet, global.verbose) {
        (true, _) => "error",
        (false, 0) => "warn",
        (false, 1) => "info,openmig=info",
        (false, 2) => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false);
    let _ = match global.log_format {
        LogFormat::Json => builder.json().flatten_event(true).try_init(),
        LogFormat::Text => builder.with_ansi(color).without_time().try_init(),
    };
}
