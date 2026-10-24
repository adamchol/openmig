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
    match runtime.block_on(openmig::flows::run(cli, &ui)) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            openmig::flows::report_error(&e, color);
            ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}
