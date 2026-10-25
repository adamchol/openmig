use clap::Parser;
use openmig::cli::Cli;
use openmig::term::TerminalUi;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let color = TerminalUi::detect_color(cli.global.color);
    openmig::logging::init(&cli.global, color);
    let ui = TerminalUi::new(color, cli.global.quiet);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::from(1);
        }
    };
    let result = runtime.block_on(async {
        tokio::select! {
            result = openmig::flows::run(cli, &ui) => Ok(result),
            signal = shutdown_signal() => Err(signal),
        }
    });
    match result {
        Ok(Ok(code)) => ExitCode::from(code),
        Ok(Err(e)) => {
            openmig::flows::report_error(&e, color);
            ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
        // The command's future has been dropped by now, which stops any scratch server it started.
        Err(signal) => ExitCode::from(128 + signal),
    }
}

/// Wait for SIGINT, SIGTERM or SIGHUP and return its number.
async fn shutdown_signal() -> u8 {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut int), Ok(mut term), Ok(mut hup)) = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = int.recv() => 2,
        _ = term.recv() => 15,
        _ = hup.recv() => 1,
    }
}
