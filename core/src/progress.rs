//! Child-process progress streaming: one reader task over a pipe, a per-line
//! parser closure at the call site, changed-percent dedup here. The third
//! consumer (recording's long-lived `ffmpeg -progress pipe:1`) is why this
//! is a seam and not two private copies that drift.

use crate::events::{send, CoreEvent, ProgressTask, SharedSink};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// How the child delimits progress records on its pipe.
#[derive(Clone, Copy)]
pub enum Delimiter {
    /// Plain lines (`ffmpeg -progress pipe:1` key=value records).
    Newline,
    /// Carriage-return updates (ollama's redraw-style output).
    CarriageReturn,
}

/// Spawn a reader over `stream`; for each record, `parse` may yield
/// `(fraction, message)` which is emitted as a `progress` event — but only
/// when the integer percent changed (event-storm guard). The caller MUST
/// `.await` the handle after the child exits or the tail is lost.
pub fn stream_progress<R>(
    stream: R,
    sink: SharedSink,
    task: ProgressTask,
    project_id: Option<Uuid>,
    delimiter: Delimiter,
    mut parse: impl FnMut(&str) -> Option<(f64, String)> + Send + 'static,
) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut last_percent = -1i64;
        let mut emit = |fraction: f64, message: String| {
            let percent = (fraction * 100.0) as i64;
            if percent != last_percent {
                last_percent = percent;
                send(&sink, CoreEvent::progress(task, project_id, fraction, message));
            }
        };
        match delimiter {
            Delimiter::Newline => {
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some((f, m)) = parse(&line) {
                        emit(f, m);
                    }
                }
            }
            Delimiter::CarriageReturn => {
                let mut reader = BufReader::new(stream).split(b'\r');
                while let Ok(Some(seg)) = reader.next_segment().await {
                    let text = String::from_utf8_lossy(&seg);
                    if let Some((f, m)) = parse(&text) {
                        emit(f, m);
                    }
                }
            }
        }
    })
}
