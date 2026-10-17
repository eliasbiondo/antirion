//! The `antirion` command: `serve`, `validate`, `schema`, `version`, `healthcheck` and `auth`.

#![forbid(unsafe_code)]

mod cluster;
mod deferred;
mod hardening;
mod lifecycle;
mod login;
mod logstore;
pub mod startup;
mod watch;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The command line.
#[derive(Debug, Parser)]
#[command(name = "antirion", version, about = "An AI gateway")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a node.
    Serve {
        /// The config document (YAML or JSON).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Enable test mode: the test clock, seed and reset endpoints and the test-only headers.
        #[arg(long)]
        test_mode: bool,
        #[arg(long, value_enum, default_value = "text")]
        log_format: LogFormat,
    },
    /// Validate a config document offline; exit 0 when it is valid.
    Validate {
        #[arg(short = 'f', long = "file")]
        file: PathBuf,
    },
    /// Print the published config schema.
    Schema,
    /// Print the version.
    Version {
        #[arg(long)]
        json: bool,
    },
    /// Probe a node's readiness; exit 0 when it is ready.
    Healthcheck {
        /// Base URL of the data port.
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        url: String,
    },
    /// Sign in to a gateway's Admin API from the command line, or out again.
    Auth {
        #[command(subcommand)]
        action: AuthCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Get an admin token by approving this sign-in in the console.
    Login {
        #[command(flatten)]
        target: TargetArgs,
        /// Sign in to the console with single sign-on.
        #[arg(long)]
        sso: bool,
        /// The organization to start in.
        #[arg(long)]
        organization: Option<String>,
    },
    /// Revoke the kept token and forget it.
    Logout {
        #[command(flatten)]
        target: TargetArgs,
    },
}

#[derive(Debug, clap::Args)]
struct TargetArgs {
    /// Base URL of the admin port; default `ANTIRION_ADMIN_URL`, else `http://127.0.0.1:8081`.
    #[arg(long)]
    admin_url: Option<String>,
    /// A PEM bundle to trust besides the public roots.
    #[arg(long)]
    ca_file: Option<PathBuf>,
}

impl TargetArgs {
    fn target(self) -> login::Target {
        login::Target {
            admin_url: self
                .admin_url
                .or_else(|| std::env::var("ANTIRION_ADMIN_URL").ok())
                .unwrap_or_else(|| "http://127.0.0.1:8081".to_owned()),
            ca_file: self.ca_file,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

fn init_logging(format: LogFormat) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    let _ = match format {
        LogFormat::Text => builder.try_init(),
        LogFormat::Json => builder.json().try_init(),
    };
}

fn out(text: &str) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn fail(e: &anyhow::Error) -> ExitCode {
    let _ = writeln!(std::io::stderr().lock(), "antirion: {e:#}");
    ExitCode::FAILURE
}

/// The stack of each worker thread of the gateway's runtime, which serves the admin plane: twice
/// the default, as a margin over what its deepest operations need.
pub const WORKER_STACK: usize = 4 * 1024 * 1024;

/// The features compiled into this binary.
fn features() -> Vec<&'static str> {
    let mut f = Vec::new();
    if cfg!(feature = "test-hooks") {
        f.push("test-hooks");
    }
    f
}

/// Runs the command line and returns the process exit code.
#[must_use]
pub fn run(cli: Cli) -> ExitCode {
    run_with(cli, antirion_ext::api::Registry::builtin())
}

/// Runs the command line of a custom build whose `extensions` can be configured as `type: builtin`
/// (see `antirion_ext::api`).
#[must_use]
pub fn run_with(cli: Cli, extensions: antirion_ext::api::Registry) -> ExitCode {
    let test_mode = matches!(cli.command, Command::Serve { test_mode: true, .. });
    let extensions = if test_mode {
        extensions.with(
            antirion_ext::builtins::TEST_SCRIPT,
            std::sync::Arc::new(antirion_ext::builtins::TestScript),
        )
    } else {
        extensions
    };
    antirion_ext::api::install(extensions);
    match cli.command {
        Command::Serve {
            config,
            test_mode,
            log_format,
        } => {
            init_logging(log_format);
            hardening::harden();
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .thread_stack_size(WORKER_STACK)
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => return fail(&e.into()),
            };
            match runtime.block_on(startup::serve(startup::ServeOptions { config, test_mode })) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    let status = fail(&e);
                    let too_new = e.chain().any(|c| {
                        matches!(
                            c.downcast_ref::<antirion_store::StoreError>(),
                            Some(antirion_store::StoreError::SchemaTooNew { .. })
                        )
                    });
                    if too_new {
                        ExitCode::from(cluster::EXIT_SCHEMA_TOO_NEW)
                    } else {
                        status
                    }
                }
            }
        }
        Command::Validate { file } => match startup::load_document(&file) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
        Command::Schema => out(antirion_config::schema::JSON_SCHEMA),
        Command::Version { json } => {
            if json {
                let v = serde_json::json!({"version": startup::VERSION, "commit": startup::COMMIT, "features": features()});
                out(&format!("{v}\n"))
            } else {
                out(&format!("antirion {} ({})\n", startup::VERSION, startup::COMMIT))
            }
        }
        Command::Auth { action } => {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(r) => r,
                Err(e) => return fail(&e.into()),
            };
            let done = match action {
                AuthCommand::Login {
                    target,
                    sso,
                    organization,
                } => runtime.block_on(login::login(&target.target(), sso, organization)),
                AuthCommand::Logout { target } => runtime.block_on(login::logout(&target.target())),
            };
            match done {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => fail(&e),
            }
        }
        Command::Healthcheck { url } => {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(r) => r,
                Err(e) => return fail(&e.into()),
            };
            match runtime.block_on(healthcheck(&url)) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(e) => fail(&e),
            }
        }
    }
}

/// `GET /-/readyz` over plain HTTP/1.1, within 2 s.
async fn healthcheck(url: &str) -> anyhow::Result<bool> {
    let authority = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow::anyhow!("healthcheck needs an http:// URL"))?;
    let authority = authority.trim_end_matches('/');
    let probe = async {
        let mut stream = tokio::net::TcpStream::connect(authority).await?;
        let request = format!("GET /-/readyz HTTP/1.1\r\nhost: {authority}\r\nconnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await?;
        let mut head = vec![0u8; 32];
        let n = stream.read(&mut head).await?;
        let line = String::from_utf8_lossy(head.get(..n).unwrap_or_default()).into_owned();
        Ok::<bool, std::io::Error>(line.starts_with("HTTP/1.1 200"))
    };
    Ok(tokio::time::timeout(Duration::from_secs(2), probe)
        .await
        .unwrap_or(Ok(false))?)
}

#[cfg(test)]
mod tests {
    use antirion_testkit::ScratchDir;

    use super::*;

    #[test]
    fn the_command_line_parses() {
        let cli = Cli::try_parse_from(["antirion", "serve", "--config", "gw.yaml", "--test-mode"]).unwrap();
        assert!(matches!(cli.command, Command::Serve { test_mode: true, .. }));
        assert!(Cli::try_parse_from(["antirion", "serve", "--unknown"]).is_err());
    }

    #[test]
    fn validate_reports_every_error_with_its_path() {
        let dir = ScratchDir::new("validate");
        let file = dir.path().join("gw.yaml");
        std::fs::write(
            &file,
            "version: 1\nmodels:\n  m: {route: nowhere}\nfrobnicate: {}\n",
        )
        .unwrap();
        let err = startup::load_document(&file).unwrap_err().to_string();
        assert!(err.contains("models.m.route: unresolved_reference"), "{err}");
        assert!(err.contains("frobnicate: unknown_field"), "{err}");
        std::fs::write(&file, "version: 1\n").unwrap();
        assert!(startup::load_document(&file).is_ok());
    }
}
