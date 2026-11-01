//! `booxmtp` binary entry point.
//!
//! Parsing, tracing setup and exit codes live here; everything else is in the library, so the
//! same code paths can be reused by a TUI or GUI frontend.

use std::process::ExitCode;

use clap::Parser;

use booxmtp::cli::{self, Cli, Command};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // The TUI must keep the terminal free of log output; it logs to a file instead.
    cli::init_tracing(cli.verbose, matches!(cli.command, Command::Tui { .. }));

    match cli::run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            cli::report_error(&error);
            ExitCode::from(error.exit_code())
        }
    }
}
