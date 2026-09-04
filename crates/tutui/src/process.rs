//! Generic scenario that runs any external command speaking the JSONL protocol
//! on stdout. Lets scenarios be written in other languages without touching the dashboard.

use crate::protocol::{Control, Event, LogLevel, MetricSpec};
use crate::scenario::{RunContext, Scenario};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Deserialize)]
pub struct ProcessParams {
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Forwarded to the child as TUTUI_PARAMS (JSON).
    #[serde(default)]
    pub params: Value,
    /// Seconds to wait after sending `stop` before killing the child.
    #[serde(default = "default_grace")]
    pub stop_grace_seconds: u64,
    /// Injected by the runner: directory of the run config. Relative `cwd` resolves against it.
    #[serde(default, rename = "_base_dir")]
    pub base_dir: Option<PathBuf>,
}

fn default_grace() -> u64 {
    5
}

pub struct ExternalProcess;

#[async_trait]
impl Scenario for ExternalProcess {
    fn id(&self) -> &str {
        "process"
    }

    fn description(&self) -> &str {
        "Run an external command that reports metrics as JSON lines on stdout"
    }

    fn metrics(&self) -> Vec<MetricSpec> {
        Vec::new() // declared by the child's hello event
    }

    async fn run(&self, ctx: RunContext) -> Result<Value> {
        let p: ProcessParams = ctx.params()?;
        anyhow::ensure!(!p.command.is_empty(), "process.command must not be empty");
        let mut cmd = Command::new(&p.command[0]);
        cmd.args(&p.command[1..])
            .envs(&p.env)
            .env("TUTUI_PARAMS", serde_json::to_string(&p.params)?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let base = p.base_dir.clone().unwrap_or_else(|| PathBuf::from("."));
        cmd.current_dir(p.cwd.as_ref().map(|c| base.join(c)).unwrap_or(base));
        let mut child = cmd.spawn().with_context(|| format!("spawn {:?}", p.command))?;
        let mut stdin = child.stdin.take().context("child stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("child stdout")?).lines();
        let stderr = BufReader::new(child.stderr.take().context("child stderr")?).lines();

        let rec = ctx.recorder.clone();
        let stderr_task = tokio::spawn(async move {
            let mut lines = stderr;
            while let Ok(Some(line)) = lines.next_line().await {
                rec.log(LogLevel::Warn, format!("stderr: {line}"));
            }
        });

        let rec = ctx.recorder.clone();
        let mut summary = Value::Null;
        let mut lines = stdout;
        let mut stop_sent = false;
        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled(), if !stop_sent => {
                    stop_sent = true;
                    let msg = serde_json::to_string(&Control::Stop)? + "\n";
                    let _ = stdin.write_all(msg.as_bytes()).await;
                    let _ = stdin.flush().await;
                    let grace = p.stop_grace_seconds;
                    let rec_kill = rec.clone();
                    let id = child.id();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(grace)).await;
                        if let Some(pid) = id {
                            rec_kill.warn(format!("child {pid} ignored stop for {grace}s; killing"));
                            unsafe { libc_kill(pid) };
                        }
                    });
                }
                line = lines.next_line() => {
                    match line? {
                        None => break,
                        Some(line) => match serde_json::from_str::<Event>(&line) {
                            Ok(Event::Done { summary: s }) => { summary = s; rec.emit(Event::Done { summary: summary.clone() }); }
                            Ok(ev) => rec.emit(ev),
                            Err(_) => rec.info(line),
                        },
                    }
                }
            }
        }
        let status = child.wait().await?;
        let _ = stderr_task.await;
        if !status.success() && !stop_sent {
            anyhow::bail!("scenario process exited with {status}");
        }
        Ok(summary)
    }
}

/// SIGKILL by pid without pulling in the libc crate for one call.
unsafe fn libc_kill(pid: u32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid as i32, 9);
}
