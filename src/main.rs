mod mcp;
mod mutter;
mod runtime;

use std::{
    env,
    ffi::OsString,
    future::Future,
    io,
    path::PathBuf,
    pin::Pin,
    process,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use rmcp::ServiceExt;
use rustix::process::{
    Pid, Signal, getppid, kill_process, set_parent_process_death_signal, setsid,
};
use tokio::{
    io::{AsyncRead, ReadBuf, Stdin},
    process::Command,
    signal::unix::{SignalKind, signal},
};

use tokio_util::sync::CancellationToken;

use crate::{
    mcp::Server,
    runtime::{Program, Size},
};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, hide = true, value_parser = clap::value_parser!(i32).range(1..))]
    worker_parent_pid: Option<i32>,
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
    let result = runtime.block_on(run(arguments));
    // Tokio's stdio reader can remain blocked until the host closes stdin.
    // The private processes have already been stopped by serve before this.
    runtime.shutdown_timeout(Duration::from_millis(250));
    result
}

async fn run(args: Cli) -> Result<()> {
    let mut terminate = signal(SignalKind::terminate()).context("watch SIGTERM")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("watch SIGINT")?;
    let shutdown = async {
        tokio::select! { _ = terminate.recv() => (), _ = interrupt.recv() => () }
    };
    let Some(parent) = args.worker_parent_pid else {
        return supervise(shutdown).await;
    };

    // Install the handler first, then link shutdown to the supervisor's lifetime.
    // The parent check covers a supervisor exit before the kernel link was set.
    set_parent_process_death_signal(Some(Signal::TERM)).context("watch MCP supervisor")?;
    let parent = Pid::from_raw(parent).context("identify MCP supervisor")?;
    if getppid() != Some(parent) {
        return Ok(());
    }
    // Keep a signal to Codex's child process group from killing the cleanup worker.
    setsid().context("detach the private desktop worker from Codex's process group")?;
    serve(args, shutdown).await
}

async fn supervise(shutdown: impl Future<Output = ()>) -> Result<()> {
    // Codex owns this process. The worker inherits MCP stdio and owns all desktop resources.
    let executable = env::current_exe().context("locate the private desktop worker executable")?;
    let mut worker = Command::new(executable)
        .arg("--worker-parent-pid")
        .arg(process::id().to_string())
        .args(env::args_os().skip(1))
        .spawn()
        .context("start the private desktop worker")?;
    let pid = worker
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
        .context("identify the private desktop worker")?;
    tokio::select! {
        status = worker.wait() => {
            let status = status.context("wait for the private desktop worker")?;
            ensure!(status.success(), "private desktop worker exited with {status}");
        }
        _ = shutdown => {
            let _ = kill_process(pid, Signal::TERM);
            worker.wait().await.context("wait for private desktop cleanup")?;
        }
    }
    Ok(())
}

async fn serve(args: Cli, shutdown: impl Future<Output = ()>) -> Result<()> {
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
    let stopping = CancellationToken::new();
    let server = Server::new(
        Size {
            width: args.width,
            height: args.height,
        },
        program,
        stopping.clone(),
    );
    let (stdin, stdout) = rmcp::transport::stdio();
    let stdin = ShutdownInput {
        stdin,
        stopping: stopping.clone(),
    };
    tokio::pin!(shutdown);
    let service = tokio::select! {
        result = server.clone().serve((stdin, stdout)) => result.context("initialize MCP stdio")?,
        _ = &mut shutdown => return Ok(()),
    };
    let cancellation = service.cancellation_token();
    let waiting = service.waiting();
    tokio::pin!(waiting);
    let result = tokio::select! {
        result = &mut waiting => result,
        _ = &mut shutdown => { stopping.cancel(); cancellation.cancel(); waiting.await }
    };
    server.shutdown().await;
    result.context("MCP service failed")?;
    Ok(())
}

struct ShutdownInput {
    stdin: Stdin,
    stopping: CancellationToken,
}

impl AsyncRead for ShutdownInput {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let remaining = buf.remaining();
        let result = Pin::new(&mut self.stdin).poll_read(cx, buf);
        // The MCP transport drains pending requests after EOF. Notify startup
        // immediately so it can clean up while the transport is draining.
        if matches!(result, Poll::Ready(Err(_)))
            || (remaining > 0
                && matches!(result, Poll::Ready(Ok(())))
                && buf.filled().len() == before)
        {
            self.stopping.cancel();
        }
        result
    }
}
