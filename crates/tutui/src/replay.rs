//! Re-emit a recorded events file with its original timing (scaled by `speed`).

use crate::protocol::Event;
use crate::scenario::Recorder;
use serde::Deserialize;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Deserialize)]
struct Recorded {
    t_ms: u128,
    event: Event,
}

pub async fn feed(path: &Path, speed: f64, recorder: Recorder) {
    let Ok(file) = tokio::fs::File::open(path).await else {
        recorder.emit(Event::Error {
            message: format!("cannot open {}", path.display()),
        });
        return;
    };
    let mut lines = BufReader::new(file).lines();
    let start = std::time::Instant::now();
    let speed = if speed <= 0.0 { 1.0 } else { speed };
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(rec) = serde_json::from_str::<Recorded>(&line) else {
            continue;
        };
        let due = std::time::Duration::from_millis((rec.t_ms as f64 / speed) as u64);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            tokio::time::sleep(wait).await;
        }
        let terminal = matches!(rec.event, Event::Done { .. } | Event::Error { .. });
        recorder.emit(rec.event);
        if terminal {
            return;
        }
    }
    recorder.emit(Event::Done {
        summary: serde_json::Value::Null,
    });
}
