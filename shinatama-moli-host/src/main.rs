//! `shinatama-moli`: Moli's protocol router served on a private named pipe.
//!
//! Stdin carries one JSON line, `{"pipe": "\\\\.\\pipe\\…", "token": "<64 hex>",
//! "args": ["--layout", …]}`, where `args` are `moli serve` options other than
//! `--host` and `--port`. Stdout gets `{"ready":true}` once the pipe exists.
//! Closing stdin shuts the instance down.

mod pipe_listener;

use std::{net::SocketAddr, process::ExitCode, sync::Arc};

use anyhow::{Context, Result, bail};
use clap::Parser;
use moli::{
    cli::{Cli, Commands},
    config::AppConfig,
};
use moli_core::runtime::{NavigationRuntimeConfig, storage_partition::StoragePartitionState};
use moli_protocol_server::protocol_server::{BrowserInstanceControl, InstanceCommand, ProtocolServer};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use pipe_listener::{PipeListener, TOKEN_LEN};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Launch {
    pipe: String,
    token: String,
    #[serde(default)]
    args: Vec<String>,
}

/// The Host and Origin guard compares against this origin; the pipe has no
/// socket address, and the token is the authentication.
const GUARD_ADDR: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 9222);

fn parse_token(hex: &str) -> Result<[u8; TOKEN_LEN]> {
    if hex.len() != TOKEN_LEN * 2 {
        bail!("token must be {} hex digits", TOKEN_LEN * 2);
    }
    let mut token = [0u8; TOKEN_LEN];
    for (i, byte) in token.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).context("token is not hex")?;
    }
    Ok(token)
}

fn serve_cli(args: &[String]) -> Result<Cli> {
    if args.iter().any(|a| {
        let a = a.split('=').next().unwrap_or(a);
        a == "--host" || a == "--port" || a == "--remote-allow-origins"
    }) {
        bail!("--host, --port and --remote-allow-origins are fixed by the host");
    }
    let argv = ["moli", "serve"].into_iter().map(str::to_owned).chain(args.iter().cloned());
    let cli = Cli::try_parse_from(argv).context("invalid moli serve options")?;
    if !matches!(cli.command, Commands::Serve(_)) {
        bail!("only serve options are accepted");
    }
    Ok(cli)
}

async fn run() -> Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    stdin.read_line(&mut line).await.context("failed to read the launch line")?;
    let launch: Launch = serde_json::from_str(line.trim()).context("invalid launch line")?;
    if !launch.pipe.starts_with(r"\\.\pipe\shinatama-moli-") {
        bail!("pipe name must start with \\\\.\\pipe\\shinatama-moli-");
    }
    let token = parse_token(&launch.token)?;
    let cli = serve_cli(&launch.args)?;
    let config = AppConfig::from_cli(&cli).context("failed to build app configuration")?;

    let storage_partition = Arc::new(StoragePartitionState::open(config.browser.profile_dir())?);
    let server = ProtocolServer::new_with_storage_partition_and_runtime_config(
        config.server.clone(),
        Arc::clone(&storage_partition),
        NavigationRuntimeConfig::from(&config.browser),
    );
    let listener = PipeListener::create(&launch.pipe, token, GUARD_ADDR)
        .with_context(|| format!("failed to create {}", launch.pipe))?;

    let control = BrowserInstanceControl::new();
    let on_eof = control.clone();
    tokio::spawn(async move {
        let mut sink = [0u8; 64];
        while matches!(stdin.read(&mut sink).await, Ok(n) if n > 0) {}
        on_eof.request(InstanceCommand::Shutdown);
    });

    let mut stdout = tokio::io::stdout();
    stdout.write_all(b"{\"ready\":true}\n").await?;
    stdout.flush().await?;

    server.serve_on(listener, GUARD_ADDR, control).await
}

fn main() -> ExitCode {
    let budget = moli::runtime_thread_budget::tokio_runtime_thread_budget();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(budget)
        .max_blocking_threads(budget)
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(run()),
        Err(error) => Err(error.into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shinatama-moli: {error:#}");
            ExitCode::FAILURE
        }
    }
}
