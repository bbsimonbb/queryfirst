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
    /// Show the database schema: tables and views, or the columns, keys and enums of the named
    /// tables. Read-only.
    Schema {
        /// Tables to detail (`name` or `schema.name`); none lists every table and view.
        tables: Vec<String>,
        /// Connection string (overrides the configured environment variable).
        #[arg(long, value_name = "CONN")]
        db: Option<String>,
    },
    /// Write a default queryfirst.json (never overwrites an existing one).
    Init {
        /// Convert pgtyped-config.json (or .pgtyped.ts) in the project directory.
        #[arg(long)]
        from_pgtyped: bool,
    },
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
    match cli.command {
        Command::Generate { paths, db } => {
            // Literal paths, relative to the current directory.
            let paths: Vec<PathBuf> = paths
                .into_iter()
                .map(|p| match &cwd {
                    Ok(cwd) if p.is_relative() => cwd.join(p),
                    _ => p,
                })
                .collect();
            ExitCode::from(queryfirst::cmd::generate::run(
                &project,
                &paths,
                db.as_deref(),
                cli.json,
            ))
        }
        Command::Describe { file, sql, db } => {
            let file = file.map(|p| match &cwd {
                Ok(cwd) if p.is_relative() => cwd.join(p),
                _ => p,
            });
            ExitCode::from(queryfirst::cmd::describe::run(
                &project,
                file.as_deref(),
                sql.as_deref(),
                db.as_deref(),
                cli.json,
            ))
        }
        Command::Check { paths } => {
            let paths: Vec<PathBuf> = paths
                .into_iter()
                .map(|p| match &cwd {
                    Ok(cwd) if p.is_relative() => cwd.join(p),
                    _ => p,
                })
                .collect();
            ExitCode::from(queryfirst::cmd::check::run(&project, &paths, cli.json))
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
                        // Absolute, so manifest path filters can be made relative to it.
                        (Some(p), _) => absolute(&cwd, p.clone()),
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
            ExitCode::from(code)
        }
        Command::Bundle { output } => {
            let output = output.map(|p| absolute(&cwd, p));
            ExitCode::from(queryfirst::cmd::bundle::run(
                &project,
                output.as_deref(),
                cli.json,
            ))
        }
        Command::Schema { tables, db } => ExitCode::from(queryfirst::cmd::schema::run(
            &project,
            &tables,
            db.as_deref(),
            cli.json,
        )),
        Command::Init { from_pgtyped } => {
            // Never an ancestor's queryfirst.json: init writes where the user is.
            let dir = match (&explicit_project, &cwd) {
                (Some(p), _) => absolute(&cwd, p.clone()),
                (None, Ok(cwd)) => cwd.clone(),
                (None, Err(_)) => project.clone(),
            };
            ExitCode::from(queryfirst::cmd::init::run(&dir, from_pgtyped, cli.json))
        }
    }
}
