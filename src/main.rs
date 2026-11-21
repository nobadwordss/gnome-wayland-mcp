mod mcp;
mod mutter;
mod runtime;

use std::{ffi::OsString, path::PathBuf, time::Duration};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use rmcp::ServiceExt;
use tokio::signal::unix::{SignalKind, signal};

use crate::{
    mcp::Server,
    runtime::{Program, Size},
};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Width of the private monitor, in pixels.
    #[arg(long, default_value_t = 1280, value_parser = clap::value_parser!(u32).range(640..=1920))]
    width: u32,
    /// Height of the private monitor, in pixels.
    #[arg(long, default_value_t = 800, value_parser = clap::value_parser!(u32).range(480..=1080))]
    height: u32,
    /// Optional native Wayland program and arguments, after --. Use an absolute path.
    #[arg(last = true, value_name = "PROGRAM [ARG...]")]
    application: Vec<OsString>,
}

fn main() -> Result<()> {
    let arguments = Cli::parse();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(serve(arguments));
    // Tokio's stdio reader can remain blocked until the host closes stdin.
    // The private processes have already been stopped by serve before this.
    runtime.shutdown_timeout(Duration::from_millis(250));
    result
}

async fn serve(args: Cli) -> Result<()> {
    let mut command = args.application.into_iter();
    let program = if let Some(executable) = command.next() {
        let path = PathBuf::from(executable);
        ensure!(
            path.is_absolute() && path.is_file(),
            "the configured application must be an absolute file path"
        );
        Some(Program {
            path,
            args: command.collect(),
        })
    } else {
        None
    };
    let server = Server::new(
        Size {
            width: args.width,
            height: args.height,
        },
        program,
    );
    let mut terminate = signal(SignalKind::terminate()).context("watch SIGTERM")?;
    let shutdown = async {
        tokio::select! { _ = terminate.recv() => (), _ = tokio::signal::ctrl_c() => () }
    };
    tokio::pin!(shutdown);
    let service = tokio::select! {
        result = server.clone().serve(rmcp::transport::stdio()) => result.context("initialize MCP stdio")?,
        _ = &mut shutdown => return Ok(()),
    };
    let cancellation = service.cancellation_token();
    let waiting = service.waiting();
    tokio::pin!(waiting);
    let result = tokio::select! {
        result = &mut waiting => result,
        _ = &mut shutdown => { cancellation.cancel(); waiting.await }
    };
    server.shutdown().await;
    result.context("MCP service failed")?;
    Ok(())
}
