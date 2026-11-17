mod tasks;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cargo xtask")]
struct Args {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Runs the Windows WSL Bubblewrap sandbox behavior tests.
    WslSandboxTests(tasks::wsl_sandbox_tests::WslSandboxTestsArgs),
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        CliCommand::WslSandboxTests(args) => tasks::wsl_sandbox_tests::run_wsl_sandbox_tests(args),
    }
}
