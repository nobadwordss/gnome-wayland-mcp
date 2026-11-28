use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    task::JoinHandle,
    time,
};
use tokio_util::sync::CancellationToken;

use super::{PrivateEnv, Process};

#[derive(Deserialize)]
struct Global {
    id: u32,
    // In monitor mode a removal has only an id and null info.
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    info: Value,
}

pub(super) struct CaptureLink {
    original: LinkId,
    pub(super) interrupted: CancellationToken,
    monitor: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy)]
struct LinkId {
    id: u32,
    serial: u64,
}

impl CaptureLink {
    pub(super) async fn bind(env: &PrivateEnv, source: u32, pid: u32) -> Result<Self> {
        let graph = snapshot(env, None).await?;
        let client = graph
            .iter()
            .find(|g| {
                g.kind == "PipeWire:Interface:Client" && g.info["props"]["pipewire.sec.pid"] == pid
            })
            .context("private GStreamer client missing from PipeWire")?;
        let sink = graph
            .iter()
            .find(|g| {
                g.kind == "PipeWire:Interface:Node" && g.info["props"]["client.id"] == client.id
            })
            .context("private GStreamer node missing from PipeWire")?;
        let link = graph
            .iter()
            .find(|g| {
                g.kind == "PipeWire:Interface:Link"
                    && g.info["output-node-id"] == source
                    && g.info["input-node-id"] == sink.id
                    && g.info["state"] == "active"
            })
            .context("private capture link is inactive; take a new screenshot")?;
        let original = LinkId {
            id: link.id,
            serial: link.info["props"]["object.serial"]
                .as_u64()
                .context("missing PipeWire link serial")?,
        };
        let mut command = pw_dump(env);
        command
            .args(["--monitor", "--raw"])
            .arg(original.id.to_string())
            .stderr(env.service_log("capture-link")?);
        let mut process = Process::spawn("PipeWire capture link monitor", &mut command)?;
        let output = process
            .child
            .stdout
            .take()
            .context("open capture link monitor pipe")?;
        let mut output = BufReader::new(output);
        // --raw emits one complete JSON array per line. Fence startup on the
        // original link so a disconnect between discovery and subscription fails closed.
        let mut buffer = Vec::new();
        let initial = read_update(&mut output, &mut buffer).await?;
        ensure!(
            original.active_in(&initial) == Some(true),
            "private capture link changed during monitor startup"
        );

        let interrupted = CancellationToken::new();
        let loss = interrupted.clone().drop_guard();
        let monitor = tokio::spawn(async move {
            let updates = async {
                while let Ok(batch) = read_update(&mut output, &mut buffer).await {
                    if original.active_in(&batch) == Some(false) {
                        break;
                    }
                }
            };
            loss.token().run_until_cancelled(updates).await;
            // Removal, EOF and malformed updates all irrevocably invalidate this capture.
            drop(loss);
            process.stop().await;
        });
        Ok(Self {
            original,
            interrupted,
            monitor: Some(monitor),
        })
    }

    pub(super) async fn is_active(&self, env: &PrivateEnv) -> Result<bool> {
        // A fresh registry round trip observes a completed unlink even while gst-launch
        // is still running. A recreated link can reuse the global ID, but has a new serial.
        let Some(graph) = self
            .interrupted
            .run_until_cancelled(snapshot(env, Some(self.original.id)))
            .await
        else {
            return Ok(false);
        };
        if self.original.active_in(&graph?) != Some(true) {
            self.interrupted.cancel();
        }
        Ok(!self.interrupted.is_cancelled())
    }

    pub(super) async fn stop(&mut self) {
        self.interrupted.cancel();
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.await;
        }
    }
}

impl Drop for CaptureLink {
    fn drop(&mut self) {
        self.interrupted.cancel();
    }
}

impl LinkId {
    fn active_in(&self, batch: &[Global]) -> Option<bool> {
        // The numeric pw-dump filter can also match a different object's serial.
        let mut seen = None;
        for entry in batch.iter().filter(|entry| entry.id == self.id) {
            if entry.kind != "PipeWire:Interface:Link"
                || entry.info["state"] != "active"
                || entry.info["props"]["object.serial"] != self.serial
            {
                return Some(false);
            }
            seen = Some(true);
        }
        seen
    }
}

async fn read_update(
    reader: &mut (impl AsyncBufRead + Unpin),
    buffer: &mut Vec<u8>,
) -> Result<Vec<Global>> {
    buffer.clear();
    reader.take(1024 * 1024).read_until(b'\n', buffer).await?;
    ensure!(
        buffer.last() == Some(&b'\n'),
        "incomplete capture link monitor update"
    );
    Ok(serde_json::from_slice(buffer)?)
}

fn pw_dump(env: &PrivateEnv) -> Command {
    let mut command = env.command(Path::new("/usr/bin/pw-dump"));
    command.args(["-N", "-r"]).arg(env.root.join("pipewire-0"));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn snapshot(env: &PrivateEnv, id: Option<u32>) -> Result<Vec<Global>> {
    let mut command = pw_dump(env);
    if let Some(id) = id {
        command.arg(id.to_string());
    }
    let output = time::timeout(Duration::from_secs(2), command.output())
        .await
        .context("private PipeWire graph query timed out")?
        .context("run /usr/bin/pw-dump for the private capture link")?;
    ensure!(
        output.status.success(),
        "private PipeWire graph query failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    // pw-dump writes no JSON when its ID filter matches no globals.
    if output.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&output.stdout).context("read private PipeWire graph")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn active_link_with_recycled_global_id_cannot_validate_old_frames() {
        let link = LinkId { id: 40, serial: 85 };
        let mut global = Global {
            id: 40,
            kind: "PipeWire:Interface:Link".into(),
            info: json!({"state": "active", "props": {"object.serial": 85}}),
        };
        assert_eq!(link.active_in(std::slice::from_ref(&global)), Some(true));
        global.info["props"]["object.serial"] = json!(86);
        assert_eq!(link.active_in(&[global]), Some(false));
    }

    #[test]
    fn monitor_deltas_only_invalidate_the_original_link() {
        let link = LinkId { id: 40, serial: 85 };
        let decode = |s| serde_json::from_str::<Vec<Global>>(s).unwrap();
        assert_eq!(link.active_in(&decode(r#"[{"id":3,"info":null}]"#)), None);
        assert_eq!(
            link.active_in(&decode(r#"[{"id":40,"info":null}]"#)),
            Some(false)
        );
    }
}
