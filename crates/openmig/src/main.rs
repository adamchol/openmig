use clap::Parser;
use openmig::cli::Cli;

fn main() {
    let cli = Cli::parse();
    let _ = cli;
    eprintln!("openmig: not implemented yet");
    std::process::exit(1);
}
