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
    /// Describe queries against the dev database; write meta files and wrappers.
    Generate {
        /// Files or directories to narrow the scope (taken literally, never globbed). None means
        /// the whole project, which also removes the output of deleted queries.
        paths: Vec<PathBuf>,
        /// Connection string (overrides the configured environment variable).
        #[arg(long, value_name = "CONN")]
        db: Option<String>,
    },
    /// Describe one query (a file, --sql, or stdin) against any database; writes nothing.
    Describe {
        /// The query file, relative to the current directory.
        file: Option<PathBuf>,
        /// The query text, as the body of a virtual file named `adhoc`.
        #[arg(long, value_name = "TEXT")]
        sql: Option<String>,
        /// Connection string (overrides the configured environment variable).
        #[arg(long, value_name = "CONN")]
        db: Option<String>,
    },
    /// Verify, offline, that meta files and wrappers match their query files.
    Check {
        /// Files or directories to narrow the scope (taken literally, never globbed).
        paths: Vec<PathBuf>,
    },
    /// Re-describe every committed query against a target database and report every
    /// difference; fails on breaking ones. Read-only; uses only meta files.
    Test {
        /// Files or directories to narrow the scope (taken literally, never globbed). With
        /// --manifest, they select queries by source path.
        paths: Vec<PathBuf>,
        /// Connection string (else test.connection.env, DATABASE_URL / PG*, then
        /// test.connection.command).
        #[arg(long, value_name = "CONN")]
        db: Option<String>,
        /// Read the metas from this manifest (written by `qf bundle`) instead of the project
        /// tree. Needs no queryfirst.json and no source files.
        #[arg(long, value_name = "FILE")]
        manifest: Option<PathBuf>,
        /// Fail on any difference, notes and warnings included.
        #[arg(long)]
        strict: bool,
    },
    /// Write every meta file into one queryfirst.manifest.json, for deployment.
    Bundle {
        /// Where to write the manifest (default: queryfirst.manifest.json at the project root).
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: Option<PathBuf>,
    },
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

/// `p` relative to the current directory.
fn absolute(cwd: &std::io::Result<PathBuf>, p: PathBuf) -> PathBuf {
    match cwd {
        Ok(cwd) if p.is_relative() => cwd.join(p),
        _ => p,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let cwd = std::env::current_dir();
    let explicit_project = cli.project.clone();
    let project = match cli.project {
        Some(p) => p,
        None => match &cwd {
            Ok(cwd) => find_project_dir(cwd),
            Err(e) => {
                eprintln!("qf: cannot read current directory: {e}");
                return ExitCode::from(2);
            }
        },
    };
    let name = match cli.command {
        Command::Generate { paths, db } => {
            // Literal paths, relative to the current directory.
            let paths: Vec<PathBuf> = paths
                .into_iter()
                .map(|p| match &cwd {
                    Ok(cwd) if p.is_relative() => cwd.join(p),
                    _ => p,
                })
                .collect();
            let code = queryfirst::cmd::generate::run(&project, &paths, db.as_deref(), cli.json);
            return ExitCode::from(code);
        }
        Command::Describe { file, sql, db } => {
            let file = file.map(|p| match &cwd {
                Ok(cwd) if p.is_relative() => cwd.join(p),
                _ => p,
            });
            let code = queryfirst::cmd::describe::run(
                &project,
                file.as_deref(),
                sql.as_deref(),
                db.as_deref(),
                cli.json,
            );
            return ExitCode::from(code);
        }
        Command::Check { paths } => {
            let paths: Vec<PathBuf> = paths
                .into_iter()
                .map(|p| match &cwd {
                    Ok(cwd) if p.is_relative() => cwd.join(p),
                    _ => p,
                })
                .collect();
            let code = queryfirst::cmd::check::run(&project, &paths, cli.json);
            return ExitCode::from(code);
        }
        Command::Test {
            paths,
            db,
            manifest,
            strict,
        } => {
            let paths: Vec<PathBuf> = paths.into_iter().map(|p| absolute(&cwd, p)).collect();
            let code = match manifest {
                // A deployment: the settings come from a queryfirst.json in the current
                // directory (or --project), if there is one.
                Some(m) => {
                    let dir = match (&explicit_project, &cwd) {
                        (Some(p), _) => p.clone(),
                        (None, Ok(cwd)) => cwd.clone(),
                        (None, Err(_)) => project.clone(),
                    };
                    let m = absolute(&cwd, m);
                    let source = queryfirst::cmd::test::Source::Manifest(&m, &paths);
                    queryfirst::cmd::test::run(&dir, source, db.as_deref(), strict, cli.json)
                }
                None => {
                    let source = queryfirst::cmd::test::Source::Tree(&paths);
                    queryfirst::cmd::test::run(&project, source, db.as_deref(), strict, cli.json)
                }
            };
            return ExitCode::from(code);
        }
        Command::Bundle { output } => {
            let output = output.map(|p| absolute(&cwd, p));
            let code = queryfirst::cmd::bundle::run(&project, output.as_deref(), cli.json);
            return ExitCode::from(code);
        }
        Command::Schema => "schema",
        Command::Init => "init",
    };
    eprintln!("qf {name}: not implemented");
    ExitCode::from(2)
}
