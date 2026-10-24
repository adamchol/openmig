//! One module per command. Every flow is generic over `E: Engine`.

use crate::cli::Cli;
use crate::ui::Ui;
use openmig_core::Error;
use owo_colors::OwoColorize;

/// Run a parsed command line. Returns the process exit code for non-error outcomes
/// (e.g. 4 when `check` finds drift is reported as an error; 0 otherwise).
pub async fn run(cli: Cli, ui: &dyn Ui) -> Result<u8, Error> {
    let _ = (cli, ui);
    Err(Error::other("not implemented yet"))
}

/// Print an error to stderr.
pub fn report_error(error: &Error, color: bool) {
    let label = if color {
        "error:".red().bold().to_string()
    } else {
        "error:".to_owned()
    };
    match error {
        Error::Schema { rendered } => eprintln!("{rendered}"),
        Error::Unsupported {
            hint: Some(hint), ..
        } => eprintln!("{label} {error}\n  hint: {hint}"),
        _ => eprintln!("{label} {error}"),
    }
}
