//! Chat history for troubleshooting: one JSON line per chat turn in
//! `<log dir>/chats/<project id>.jsonl` (on macOS
//! `~/Library/Logs/RoughCut/chats/`).
//!
//! Unlike the app log (`applog`), this file DOES hold private text: what the
//! user typed, the model's replies, the tool calls with their arguments and
//! the tool results. That is the point: it lets you see why the agent made a
//! cut. It stays on this Mac. Nothing reads it over the network, and it is
//! only written once the app log is on, so tests and the CLI write nothing.
//!
//! How it works: `begin` opens a turn for a project, `record` is called from
//! every agent step the chat panel shows (the same data as the `agent-step`
//! events), and `finish` appends the line. A project has at most one chat
//! turn at a time, so a turn is keyed by project id.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use uuid::Uuid;

/// A file stops growing past this; delete it to start over.
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// Any single argument or result is cut to this many characters.
const VALUE_CAP: usize = 8_000;

struct Turn {
    started: Instant,
    record: Value,
    steps: Vec<Value>,
}

fn turns() -> &'static Mutex<HashMap<Uuid, Turn>> {
    static TURNS: OnceLock<Mutex<HashMap<Uuid, Turn>>> = OnceLock::new();
    TURNS.get_or_init(Default::default)
}

/// The folder chat histories go in.
pub fn dir() -> PathBuf {
    // Next to the app log that is actually open (tests open theirs in a
    // temp folder), else the default log folder.
    crate::applog::log_file()
        .and_then(|f| f.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(crate::applog::log_dir)
        .join("chats")
}

/// The history file for one project.
pub fn file_for(project_id: Uuid) -> PathBuf {
    dir().join(format!("{project_id}.jsonl"))
}

fn enabled() -> bool {
    crate::applog::log_file().is_some()
}

/// Start a chat turn. `history_turns` is how many earlier turns the panel
/// sent along.
pub fn begin(project_id: Uuid, instruction: &str, history_turns: usize) {
    if !enabled() {
        return;
    }
    let record = json!({
        "ts": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        "project_id": project_id,
        "instruction": instruction,
        "history_turns": history_turns,
    });
    if let Ok(mut map) = turns().lock() {
        map.insert(project_id, Turn { started: Instant::now(), record, steps: vec![] });
    }
}

/// Add a top-level field to the open turn (model, context size, durations).
pub fn note(project_id: Uuid, key: &str, value: Value) {
    if let Ok(mut map) = turns().lock() {
        if let Some(turn) = map.get_mut(&project_id) {
            turn.record[key] = value;
        }
    }
}

/// One agent step: thinking text, a tool call, a tool result, the final reply.
pub fn record(
    project_id: Uuid,
    step: usize,
    kind: &str,
    tool: Option<&str>,
    args: Option<&Value>,
    result: Option<&Value>,
    text: Option<&str>,
) {
    let Ok(mut map) = turns().lock() else { return };
    let Some(turn) = map.get_mut(&project_id) else { return };
    let mut entry = json!({
        "ms": turn.started.elapsed().as_millis() as u64,
        "step": step,
        "kind": kind,
    });
    if let Some(t) = tool {
        entry["tool"] = json!(t);
    }
    if let Some(a) = args {
        entry["args"] = capped(a);
    }
    if let Some(r) = result {
        entry["result"] = capped(r);
    }
    if let Some(t) = text {
        entry["text"] = json!(t);
    }
    turn.steps.push(entry);
}

/// Close the turn and append it to the project's file. `outcome` is the
/// reply and action count, or the error.
pub fn finish(project_id: Uuid, outcome: std::result::Result<(&str, usize), String>) {
    let Some(turn) = turns().lock().ok().and_then(|mut m| m.remove(&project_id)) else { return };
    let mut record = turn.record;
    record["ms"] = json!(turn.started.elapsed().as_millis() as u64);
    match outcome {
        Ok((summary, actions)) => {
            record["outcome"] = json!("ok");
            record["reply"] = json!(summary);
            record["actions"] = json!(actions);
        }
        Err(e) => {
            record["outcome"] = json!("error");
            record["error"] = json!(e);
        }
    }
    record["steps"] = Value::Array(turn.steps);
    let _ = append(project_id, &record);
}

fn append(project_id: Uuid, record: &Value) -> std::io::Result<()> {
    fs::create_dir_all(dir())?;
    let path = file_for(project_id);
    if fs::metadata(&path).map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(false) {
        return Ok(());
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    f.write_all(line.as_bytes())
}

/// Long id lists stay readable: arrays over 40 items keep the first 40 plus
/// a count, and big values are cut to VALUE_CAP characters.
fn capped(v: &Value) -> Value {
    let v = shorten_arrays(v);
    let s = v.to_string();
    if s.len() <= VALUE_CAP {
        return v;
    }
    let cut: String = s.chars().take(VALUE_CAP).collect();
    json!({ "truncated": true, "chars": s.len(), "head": cut })
}

fn shorten_arrays(v: &Value) -> Value {
    match v {
        Value::Array(a) if a.len() > 40 => {
            let mut out: Vec<Value> = a.iter().take(40).map(shorten_arrays).collect();
            out.push(json!(format!("(+{} more)", a.len() - 40)));
            Value::Array(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(shorten_arrays).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), shorten_arrays(v))).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_id_lists_keep_a_count() {
        let ids: Vec<Value> = (0..433).map(|i| json!(format!("id-{i}"))).collect();
        let v = capped(&json!({ "segment_ids": ids }));
        let arr = v["segment_ids"].as_array().unwrap();
        assert_eq!(arr.len(), 41);
        assert_eq!(arr[40], json!("(+393 more)"));
    }

    #[test]
    fn huge_values_are_cut() {
        let v = capped(&json!("x".repeat(VALUE_CAP * 2)));
        assert_eq!(v["truncated"], json!(true));
    }

    #[test]
    fn nothing_is_kept_without_an_open_turn() {
        let pid = Uuid::new_v4();
        record(pid, 0, "thinking", None, None, None, Some("hi"));
        assert!(turns().lock().unwrap().get(&pid).is_none());
    }
}
