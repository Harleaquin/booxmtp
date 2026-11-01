//! `booxmtp` binary entry point.
//!
//! Parsing, tracing setup and exit codes live here; everything else is in the library, so the
//! same code paths can be reused by a TUI or GUI frontend.

use std::process::ExitCode;

use clap::Parser;

use booxmtp::cli::{self, Cli};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    cli::init_tracing(cli.verbose);

    match cli::run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            cli::report_error(&error);
            ExitCode::from(error.exit_code())
        }
    }
}
