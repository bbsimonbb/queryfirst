use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Queryfirst: validate .sql query files against Postgres and generate TypeScript wrappers.
#[derive(Parser)]
#[command(name = "qf", version, about)]
struct Cli {
    /// Emit machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,

    /// Project directory (default: nearest ancestor containing queryfirst.json, else the cwd).
    #[arg(long, global = true, value_name = "DIR")]
    project: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate TypeScript wrappers and metadata.
    Generate,
    /// Validate query files without writing output.
    Check,
    /// Run query tests against the database.
    Test,
    /// Bundle generated output.
    Bundle,
    /// Inspect the database schema.
    Schema,
    /// Initialise a Queryfirst project.
    Init,
}

/// Nearest ancestor of `start` (inclusive) containing `queryfirst.json`, else `start`.
fn find_project_dir(start: &Path) -> PathBuf {
    start
        .ancestors()
        .find(|d| d.join("queryfirst.json").is_file())
        .unwrap_or(start)
        .to_path_buf()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let _project = cli.project.unwrap_or_else(|| {
        let cwd = std::env::current_dir().expect("cannot read current directory");
        find_project_dir(&cwd)
    });
    let name = match cli.command {
        Command::Generate => "generate",
        Command::Check => "check",
        Command::Test => "test",
        Command::Bundle => "bundle",
        Command::Schema => "schema",
        Command::Init => "init",
    };
    eprintln!("qf {name}: not implemented");
    ExitCode::from(2)
}
