//! The app log: one JSON line per event in a small, rotating file on this
//! Mac (`~/Library/Logs/RoughCut/roughcut.log`, or `<data dir>/logs/`
//! elsewhere), so a failure or a fallback can be diagnosed after the fact.
//! Design: `docs/designs/logging.md`.
//!
//! Three rules:
//!
//! - **Nothing private.** Events are typed: their fields are numbers, ids,
//!   enums, model names, step labels and error text. There is no field for
//!   transcript text, prompts, replies or keys, and error text can only be
//!   built through [`ErrText`] / [`Tail`], which run [`scrub`].
//! - **Local only.** File I/O, nothing else. Nothing reads the file over the
//!   network.
//! - **Never in the way.** Until [`init`] runs (tests, the CLI) events go
//!   nowhere; a write that fails turns the log off for the session and is
//!   never returned to a caller.

use crate::adapters::inference::{ServedBy, Usage};
use crate::engine::Editor;
use crate::error::CoreError;
use crate::model::{ModelMode, RouteReason};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use uuid::Uuid;

/// The current file; older ones are `roughcut.1.log` and `roughcut.2.log`.
pub const FILE_NAME: &str = "roughcut.log";
/// Rotate before a write would take the file past this.
const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// `error` and `detail` fields are cut to this many characters.
const ERR_CAP: usize = 300;
/// `stderr_tail` keeps this many characters from the end.
const TAIL_KEEP: usize = 1000;
/// A run of this many letters, digits or `_` is a token, not a word.
const TOKEN_RUN: usize = 40;

// ------------------------------------------------------------------ events

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// One line of the log. Fields that don't apply are left out.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    AppStart {
        version: &'static str,
        os: &'static str,
        arch: &'static str,
        mode: ModelMode,
        hosted_model: String,
        local_model: String,
        demo: bool,
    },
    /// The editor couldn't start (a locked or broken database).
    AppStartFailed { code: &'static str, error: ErrText },
    AppExit {},
    /// The MCP server didn't start, or stopped.
    McpFailed { error: ErrText },
    /// One model request (a JSON-contract call, or one chat turn).
    AiRequest(AiRequest),
    /// Where Hosted requests run changed (what the top-bar chip shows).
    ModelRoute {
        effective: ModelMode,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<RouteReason>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<ErrText>,
    },
    /// One AI step: how it ended and why.
    AiStep(AiStep),
    /// ffmpeg or ffprobe exited with an error, or couldn't start.
    FfmpegFailed {
        what: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        ms: u64,
        #[serde(skip_serializing_if = "Tail::is_empty")]
        stderr_tail: Tail,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<ErrText>,
    },
    TranscribeStart {
        project: Uuid,
        media_s: f64,
        language: String,
        engine: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        whisper_model: Option<String>,
    },
    TranscribeEnd { project: Uuid, ms: u64, segments: usize, model: String },
    TranscribeFailed { project: Uuid, stage: &'static str, ms: u64, code: &'static str, error: ErrText },
}

#[derive(Debug, Serialize)]
pub struct AiRequest {
    /// The call's label: `outline`, `short_picks`, `chat`, …
    step: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    served_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hosted: Option<bool>,
    ms: u64,
    attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<u64>,
    outcome: RequestOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrText>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    Ok,
    /// The reply wasn't the JSON asked for (after the repair retry).
    BadJson,
    Error,
}

#[derive(Debug, Serialize)]
pub struct AiStep {
    step: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<Uuid>,
    /// The user's setting when the step began.
    mode: ModelMode,
    outcome: StepOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrText>,
    ms: u64,
    /// The models that answered, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    models: Vec<String>,
    #[serde(flatten)]
    counts: Counts,
    /// Edits had landed before it failed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stopped: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Ok,
    Fallback,
    Failed,
    /// Dropped without an outcome (an early exit or a cancelled request).
    Aborted,
}

/// A step's counts (`beats`, `topics`, …): names are code, values numbers.
#[derive(Debug, Default)]
struct Counts(Vec<(&'static str, u64)>);

impl Serialize for Counts {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

/// Error text, scrubbed when it's made: the only way to put free text in a
/// line.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct ErrText(String);

impl ErrText {
    pub fn new(text: &str) -> Self {
        ErrText(scrub(text))
    }
}

impl From<&CoreError> for ErrText {
    fn from(e: &CoreError) -> Self {
        ErrText::new(&e.to_string())
    }
}

/// The end of a tool's stderr, scrubbed for keys, tokens and the home
/// folder (but not quotes or braces, which would blank ffmpeg's own
/// messages).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(transparent)]
pub struct Tail(String);

impl Tail {
    pub fn new(stderr: &str) -> Self {
        Tail(scrub_tail(stderr))
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Serialize)]
struct Line<'a> {
    ts: String,
    level: Level,
    #[serde(flatten)]
    event: &'a Event,
}

// ------------------------------------------------------------------ writing

/// Write one event. Never fails, never blocks on anything but the log's own
/// lock.
pub fn write(level: Level, event: Event) {
    let line = Line { ts: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(), level, event: &event };
    let Ok(text) = serde_json::to_string(&line) else { return };
    #[cfg(test)]
    if capture::push(&text) {
        return;
    }
    if let Some(log) = LOG.get() {
        log.file.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).write_line(&text);
    }
}

struct Global {
    path: PathBuf,
    file: Mutex<LogFile>,
}

static LOG: OnceLock<Global> = OnceLock::new();

/// Start writing to `dir/roughcut.log`. Returns the file's path, or None if
/// the folder or the file can't be opened (the log then stays off).
pub fn init(dir: &Path) -> Option<PathBuf> {
    if let Some(log) = LOG.get() {
        return Some(log.path.clone());
    }
    let file = LogFile::open(dir, MAX_BYTES).ok()?;
    let path = file.path();
    let _ = LOG.set(Global { path, file: Mutex::new(file) });
    LOG.get().map(|log| log.path.clone())
}

/// The log folder: `~/Library/Logs/RoughCut/` on macOS (Console.app lists
/// it), `<data dir>/logs/` elsewhere.
pub fn log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        return home.join("Library").join("Logs").join("RoughCut");
    }
    crate::store::data_dir().join("logs")
}

/// The file being written, once [`init`] succeeded.
pub fn log_file() -> Option<PathBuf> {
    LOG.get().map(|log| log.path.clone())
}

/// An append-only file that rotates by size: `roughcut.log` →
/// `roughcut.1.log` → `roughcut.2.log` → dropped.
struct LogFile {
    dir: PathBuf,
    /// None once a write failed: the log is off for the session.
    file: Option<File>,
    size: u64,
    max: u64,
}

impl LogFile {
    fn open(dir: &Path, max: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new().create(true).append(true).open(dir.join(FILE_NAME))?;
        let size = file.metadata()?.len();
        Ok(Self { dir: dir.to_path_buf(), file: Some(file), size, max })
    }

    fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    fn rotated(&self, n: u8) -> PathBuf {
        self.dir.join(format!("roughcut.{n}.log"))
    }

    fn write_line(&mut self, line: &str) {
        if self.file.is_none() {
            return;
        }
        let bytes = line.len() as u64 + 1;
        if self.size > 0 && self.size + bytes > self.max && self.rotate().is_err() {
            self.file = None;
            return;
        }
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        // One write per line, unbuffered: the last lines survive a crash.
        match self.file.as_mut().map(|f| f.write_all(buf.as_bytes())) {
            Some(Ok(())) => self.size += bytes,
            _ => self.file = None,
        }
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        let (one, two) = (self.rotated(1), self.rotated(2));
        // Windows won't rename over an existing file.
        let _ = std::fs::remove_file(&two);
        if one.exists() {
            std::fs::rename(&one, &two)?;
        }
        std::fs::rename(self.path(), &one)?;
        self.file = Some(OpenOptions::new().create(true).append(true).open(self.path())?);
        self.size = 0;
        Ok(())
    }
}

/// The `app_start` line: version, platform and the model settings.
pub fn app_start(editor: &Editor, version: &'static str) {
    let prefs = editor.get_preferences().unwrap_or_default();
    write(
        Level::Info,
        Event::AppStart {
            version,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            mode: prefs.model_mode,
            hosted_model: prefs.hosted_model,
            local_model: prefs.inference_model,
            demo: editor.demo_mode(),
        },
    );
}

// ----------------------------------------------------------------- requests

/// How one model request ended.
pub enum Requested<'a> {
    Ok,
    /// serde's category and position only: its message quotes the reply.
    BadJson(String),
    Error(&'a CoreError),
}

/// Log one model request: which model answered, how long it took, its
/// tokens, how it ended.
pub fn request(
    step: &str,
    served: Option<&ServedBy>,
    started: Instant,
    attempts: u32,
    usage: Option<Usage>,
    outcome: Requested<'_>,
) {
    let (outcome, code, error, level) = match outcome {
        Requested::Ok => (RequestOutcome::Ok, None, None, Level::Info),
        Requested::BadJson(at) => (RequestOutcome::BadJson, Some("inference"), Some(ErrText::new(&at)), Level::Warn),
        Requested::Error(e) => (RequestOutcome::Error, Some(e.code()), Some(ErrText::from(e)), Level::Warn),
    };
    write(
        level,
        Event::AiRequest(AiRequest {
            step: step.to_string(),
            served_by: served.map(|s| s.model.clone()),
            hosted: served.map(|s| s.hosted),
            ms: started.elapsed().as_millis() as u64,
            attempts,
            input_tokens: usage.map(|u| u.input_tokens),
            output_tokens: usage.map(|u| u.output_tokens),
            outcome,
            code,
            error,
        }),
    );
}

/// Where a reply stopped being the JSON asked for, without quoting it:
/// "syntax error at line 3 column 14".
pub fn json_error_at(e: &serde_json::Error) -> String {
    let kind = match e.classify() {
        serde_json::error::Category::Io => "io error",
        serde_json::error::Category::Syntax => "syntax error",
        serde_json::error::Category::Data => "wrong shape",
        serde_json::error::Category::Eof => "cut short",
    };
    format!("bad JSON: {kind} at line {} column {}", e.line(), e.column())
}

/// Add two requests' token counts (None when neither had any).
pub fn add_usage(a: Option<Usage>, b: Option<Usage>) -> Option<Usage> {
    match (a, b) {
        (None, None) => None,
        (a, b) => {
            let (a, b) = (a.unwrap_or_default(), b.unwrap_or_default());
            Some(Usage { input_tokens: a.input_tokens + b.input_tokens, output_tokens: a.output_tokens + b.output_tokens })
        }
    }
}

// -------------------------------------------------------------------- steps

/// One AI step's line. Start it, note counts and the models that answered,
/// then end it with [`ok`](Step::ok), [`fallback`](Step::fallback),
/// [`failed`](Step::failed) or [`finish`](Step::finish). A step dropped
/// without one writes `aborted`.
pub struct Step {
    step: &'static str,
    project: Option<Uuid>,
    mode: ModelMode,
    started: Instant,
    method: Option<&'static str>,
    counts: Vec<(&'static str, u64)>,
    models: Vec<String>,
    mark: Option<Mark>,
    partial: bool,
    stopped: Option<&'static str>,
    done: bool,
}

enum Mark {
    Fallback { reason: &'static str, code: Option<&'static str>, error: Option<ErrText> },
    Failed { code: &'static str, error: ErrText },
}

impl Step {
    /// A step in the editor's current mode.
    pub fn start(editor: &Editor, step: &'static str, project: Option<Uuid>) -> Self {
        let mode = editor.get_preferences().map(|p| p.model_mode).unwrap_or_default();
        Self::with_mode(step, project, mode)
    }

    pub fn with_mode(step: &'static str, project: Option<Uuid>, mode: ModelMode) -> Self {
        Self {
            step,
            project,
            mode,
            started: Instant::now(),
            method: None,
            counts: vec![],
            models: vec![],
            mark: None,
            partial: false,
            stopped: None,
            done: false,
        }
    }

    pub fn method(&mut self, method: &'static str) -> &mut Self {
        self.method = Some(method);
        self
    }

    /// Set a count (replaces an earlier value of the same name).
    pub fn count(&mut self, name: &'static str, n: usize) -> &mut Self {
        match self.counts.iter_mut().find(|(k, _)| *k == name) {
            Some((_, v)) => *v = n as u64,
            None => self.counts.push((name, n as u64)),
        }
        self
    }

    /// Note the model that answered a request (each one once).
    pub fn served(&mut self, served: Option<&ServedBy>) -> &mut Self {
        if let Some(s) = served {
            self.model(&s.model);
        }
        self
    }

    pub fn model(&mut self, model: &str) -> &mut Self {
        if !self.models.iter().any(|m| m == model) {
            self.models.push(model.to_string());
        }
        self
    }

    /// The step will end in rules, not the model: `reason` is
    /// `model_unreachable`, `too_few_beats`, … or an error's code.
    pub fn mark_fallback(&mut self, reason: &'static str, err: Option<&CoreError>) -> &mut Self {
        self.mark = Some(Mark::Fallback { reason, code: err.map(CoreError::code), error: err.map(ErrText::from) });
        self
    }

    /// The step failed though its function returns Ok (a partial result).
    pub fn mark_failed(&mut self, err: &CoreError) -> &mut Self {
        self.mark = Some(Mark::Failed { code: err.code(), error: ErrText::from(err) });
        self
    }

    /// Edits had landed before the failure.
    pub fn mark_partial(&mut self) -> &mut Self {
        self.partial = true;
        self
    }

    /// It ended early on a limit (`max_steps`).
    pub fn stopped(&mut self, why: &'static str) -> &mut Self {
        self.stopped = Some(why);
        self
    }

    /// Done: `ok`, unless a fallback or failure was marked.
    pub fn ok(mut self) {
        self.emit(None);
    }

    pub fn fallback(mut self, reason: &'static str, err: Option<&CoreError>) {
        self.mark_fallback(reason, err);
        self.emit(None);
    }

    pub fn failed(mut self, err: &CoreError) {
        self.mark_failed(err);
        self.emit(None);
    }

    /// End a wrapped function's step from its result: `Err` is `failed`;
    /// `Ok` is `ok` unless a fallback or failure was marked.
    pub fn finish<T>(mut self, result: &crate::error::Result<T>) {
        if let Err(e) = result {
            self.mark_failed(e);
        }
        self.emit(None);
    }

    fn emit(&mut self, forced: Option<StepOutcome>) {
        if self.done {
            return;
        }
        self.done = true;
        let (outcome, reason, code, error) = match (forced, self.mark.take()) {
            (Some(o), _) => (o, None, None, None),
            (None, None) => (StepOutcome::Ok, None, None, None),
            (None, Some(Mark::Fallback { reason, code, error })) => (StepOutcome::Fallback, Some(reason), code, error),
            (None, Some(Mark::Failed { code, error })) => (StepOutcome::Failed, None, Some(code), Some(error)),
        };
        let level = match outcome {
            StepOutcome::Ok => Level::Info,
            StepOutcome::Fallback | StepOutcome::Aborted => Level::Warn,
            StepOutcome::Failed => Level::Error,
        };
        write(
            level,
            Event::AiStep(AiStep {
                step: self.step,
                project: self.project,
                mode: self.mode,
                outcome,
                method: self.method,
                reason,
                code,
                error,
                ms: self.started.elapsed().as_millis() as u64,
                models: std::mem::take(&mut self.models),
                counts: Counts(std::mem::take(&mut self.counts)),
                partial: self.partial,
                stopped: self.stopped,
            }),
        );
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        self.emit(Some(StepOutcome::Aborted));
    }
}

// ----------------------------------------------------------------- scrubber

/// Make error text safe to log: blank the inside of quotes (`"…"`, `“…”`,
/// backticks other than plain identifiers) and `{…}` spans (JSON bodies and
/// replies), API keys and long tokens; the home folder becomes `~`; at most
/// 300 characters. Quotes are read with backslash escapes, as serde writes
/// them, so a reply fragment can't end a quote early.
pub fn scrub(text: &str) -> String {
    let s = home_to_tilde(text);
    let s = blank_quotes(&s);
    let s = blank_braces(&s);
    let s = blank_tokens(&s);
    cap_head(&s, ERR_CAP)
}

/// Make a tool's stderr safe to log: keys, long tokens and the home folder,
/// then the last 1,000 characters (ffmpeg says what went wrong last).
pub fn scrub_tail(text: &str) -> String {
    let s = home_to_tilde(text.trim_end());
    let s = blank_tokens(&s);
    keep_tail(&s, TAIL_KEEP)
}

fn home_to_tilde(text: &str) -> String {
    match dirs::home_dir().map(|h| h.to_string_lossy().into_owned()) {
        Some(home) if home.len() > 1 => text.replace(&home, "~"),
        _ => text.to_string(),
    }
}

fn blank_quotes(text: &str) -> String {
    let cs: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        let close = match c {
            '"' => Some('"'),
            '“' => Some('”'),
            '`' => Some('`'),
            _ => None,
        };
        let Some(close) = close else {
            out.push(c);
            i += 1;
            continue;
        };
        // Find the closing quote; `"` honours backslash escapes.
        let mut j = i + 1;
        let mut escaped = false;
        let mut end = None;
        while j < cs.len() {
            let d = cs[j];
            if escaped {
                escaped = false;
            } else if d == '\\' && c == '"' {
                escaped = true;
            } else if d == close {
                end = Some(j);
                break;
            }
            j += 1;
        }
        let Some(end) = end else {
            // Unclosed: everything after it could be the reply.
            out.push(c);
            out.push('…');
            return out;
        };
        let inner: String = cs[i + 1..end].iter().collect();
        out.push(c);
        if c == '`' && is_identifier(&inner) {
            out.push_str(&inner);
        } else if !inner.is_empty() {
            out.push('…');
        }
        out.push(close);
        i = end + 1;
    }
    out
}

/// serde's field names (`cut_priority`) are kept; anything else in
/// backticks is blanked.
fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn blank_braces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for c in text.chars() {
        if depth == 0 {
            if c == '{' {
                depth = 1;
                out.push_str("{…");
            } else {
                out.push(c);
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    out.push('}');
                }
            }
            _ => {}
        }
    }
    // Unbalanced: cut to the end (already "{…").
    out
}

fn blank_tokens(text: &str) -> String {
    let cs: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut i = 0;
    while i < cs.len() {
        // An Anthropic key: `sk-ant-` and everything key-like after it.
        if cs[i..].starts_with(&['s', 'k', '-', 'a', 'n', 't', '-']) {
            out.push_str("sk-ant-…");
            i += 7;
            while i < cs.len() && (word(cs[i]) || cs[i] == '-') {
                i += 1;
            }
            continue;
        }
        if word(cs[i]) {
            let start = i;
            while i < cs.len() && word(cs[i]) {
                i += 1;
            }
            if i - start >= TOKEN_RUN {
                out.push('…');
            } else {
                out.extend(&cs[start..i]);
            }
            continue;
        }
        out.push(cs[i]);
        i += 1;
    }
    out
}

fn cap_head(text: &str, n: usize) -> String {
    if text.chars().count() <= n {
        return text.to_string();
    }
    let mut s: String = text.chars().take(n).collect();
    s.push('…');
    s
}

fn keep_tail(text: &str, n: usize) -> String {
    let count = text.chars().count();
    if count <= n {
        return text.to_string();
    }
    let mut s = String::from("…");
    s.extend(text.chars().skip(count - n));
    s
}

// ------------------------------------------------------------ test capture

/// Tests: collect this thread's lines instead of writing them (a
/// `#[tokio::test]` runs on one thread, so parallel tests don't mix).
#[cfg(test)]
pub(crate) mod capture {
    use std::cell::RefCell;

    thread_local! {
        static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    pub(super) fn push(line: &str) -> bool {
        LINES.with(|l| match l.borrow_mut().as_mut() {
            Some(lines) => {
                lines.push(line.to_string());
                true
            }
            None => false,
        })
    }

    /// Capture until dropped.
    pub(crate) struct Capture;

    impl Capture {
        pub(crate) fn start() -> Self {
            LINES.with(|l| *l.borrow_mut() = Some(vec![]));
            Capture
        }

        pub(crate) fn lines(&self) -> Vec<serde_json::Value> {
            LINES.with(|l| {
                l.borrow()
                    .iter()
                    .flatten()
                    .map(|s| serde_json::from_str(s).expect("every line is JSON"))
                    .collect()
            })
        }

        pub(crate) fn text(&self) -> String {
            LINES.with(|l| l.borrow().iter().flatten().cloned().collect::<Vec<_>>().join("\n"))
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            LINES.with(|l| *l.borrow_mut() = None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn rotates_by_size_and_keeps_three_files() {
        let dir = tempfile::tempdir().unwrap();
        let line = "x".repeat(99); // 100 bytes with the newline
        let mut log = LogFile::open(dir.path(), 1000).unwrap();
        for _ in 0..10 {
            log.write_line(&line);
        }
        assert_eq!(files(dir.path()), vec!["roughcut.log"]);
        assert_eq!(std::fs::metadata(dir.path().join(FILE_NAME)).unwrap().len(), 1000);
        // The next line would pass the cap: rotate first.
        log.write_line("first of the second file");
        assert_eq!(files(dir.path()), vec!["roughcut.1.log", "roughcut.log"]);
        assert_eq!(std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap(), "first of the second file\n");
        // Two more rotations: still three files, the oldest dropped.
        for n in 0..25 {
            log.write_line(&format!("{n:0>99}"));
        }
        assert_eq!(files(dir.path()), vec!["roughcut.1.log", "roughcut.2.log", "roughcut.log"]);
        // The first file's lines are gone; the second file is now the oldest.
        let two = std::fs::read_to_string(log.rotated(2)).unwrap();
        assert!(two.starts_with("first of the second file\n"), "{two}");
        for f in files(dir.path()) {
            let text = std::fs::read_to_string(dir.path().join(&f)).unwrap();
            assert!(!text.contains("xxxx"), "{f} still has the first file's lines");
            assert!(text.len() <= 1000);
        }
    }

    #[test]
    fn a_reopened_log_counts_what_is_already_there() {
        let dir = tempfile::tempdir().unwrap();
        let line = "y".repeat(99);
        {
            let mut log = LogFile::open(dir.path(), 1000).unwrap();
            for _ in 0..9 {
                log.write_line(&line);
            }
        }
        let mut log = LogFile::open(dir.path(), 1000).unwrap();
        assert_eq!(log.size, 900);
        log.write_line(&line); // 1000: still fits
        assert_eq!(files(dir.path()), vec!["roughcut.log"]);
        log.write_line(&line); // would pass it
        assert_eq!(files(dir.path()), vec!["roughcut.1.log", "roughcut.log"]);
    }

    #[test]
    fn a_folder_that_cant_be_made_leaves_the_log_off() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        assert!(LogFile::open(&blocker.join("logs"), 1000).is_err());
    }

    #[test]
    fn lines_are_json_with_ts_level_and_event_first() {
        let cap = capture::Capture::start();
        write(Level::Info, Event::AppExit {});
        write(
            Level::Warn,
            Event::ModelRoute { effective: ModelMode::Local, reason: Some(RouteReason::Offline), detail: None },
        );
        let mut step = Step::with_mode("outline", None, ModelMode::Hosted);
        step.count("beats", 1).model("claude-opus-5-5");
        step.fallback("too_few_beats", None);
        let text = cap.text();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with(r#"{"ts":""#), "{}", lines[0]);
        assert!(lines[0].ends_with(r#","level":"info","event":"app_exit"}"#), "{}", lines[0]);
        assert!(lines[1].contains(r#""event":"model_route","effective":"local","reason":"offline""#), "{}", lines[1]);
        let v = cap.lines();
        assert_eq!(v[2]["event"], "ai_step");
        assert_eq!(v[2]["outcome"], "fallback");
        assert_eq!(v[2]["reason"], "too_few_beats");
        assert_eq!(v[2]["mode"], "hosted");
        assert_eq!(v[2]["beats"], 1);
        assert_eq!(v[2]["models"][0], "claude-opus-5-5");
        assert!(v[2].get("error").is_none() && v[2].get("partial").is_none());
    }

    #[test]
    fn a_dropped_step_is_aborted_and_finish_reads_the_result() {
        let cap = capture::Capture::start();
        {
            let _step = Step::with_mode("chat", None, ModelMode::Local);
        }
        let step = Step::with_mode("story_plan", None, ModelMode::Local);
        step.finish::<()>(&Err(CoreError::Inference("the plan cut nearly every beat".into())));
        let mut step = Step::with_mode("chat", None, ModelMode::Local);
        step.mark_fallback("model_unreachable", None);
        step.finish(&Ok(()));
        let mut step = Step::with_mode("story_plan", None, ModelMode::Local);
        step.mark_failed(&CoreError::Other("reading the timeline".into())).mark_partial().count("actions", 2);
        step.finish(&Ok(()));
        let v = cap.lines();
        let outcomes: Vec<&str> = v.iter().map(|l| l["outcome"].as_str().unwrap()).collect();
        assert_eq!(outcomes, vec!["aborted", "failed", "fallback", "failed"]);
        assert_eq!(v[1]["code"], "inference");
        assert_eq!(v[1]["level"], "error");
        assert_eq!(v[3]["partial"], true);
        assert_eq!(v[3]["actions"], 2);
    }

    #[test]
    fn scrub_blanks_quoted_text_even_with_escaped_quotes() {
        let serde = r#"bad JSON in outline response: invalid type: string "the \"zebracorn\" said hi", expected struct RawBeat at line 1 column 52"#;
        let s = scrub(serde);
        assert!(!s.contains("zebracorn") && !s.contains("said hi"), "{s}");
        assert!(s.contains(r#"string "…", expected struct RawBeat"#), "{s}");
        // Curly quotes, as the app writes them.
        assert_eq!(scrub("nothing matches the theme “zebracorn pricing”"), "nothing matches the theme “…”");
        // serde's field names survive; other backtick text doesn't.
        assert_eq!(scrub("missing field `cut_priority`"), "missing field `cut_priority`");
        assert_eq!(scrub("unknown thing `two words`"), "unknown thing `…`");
        // An unclosed quote: the rest could be the reply.
        assert_eq!(scrub(r#"expected value: "half a zebracorn"#), r#"expected value: "…"#);
    }

    #[test]
    fn scrub_blanks_json_bodies_and_braces_inside_strings() {
        let s = scrub(r#"unexpected message shape: missing field `role` in {"content":"a } zebracorn {","x":{"y":1}} end"#);
        assert_eq!(s, "unexpected message shape: missing field `role` in {…} end");
        let s = scrub(r#"inference HTTP 500 Internal Server Error: {"error":"model 'gemma' failed: zebracorn"}"#);
        assert_eq!(s, "inference HTTP 500 Internal Server Error: {…}");
        assert_eq!(scrub("cut short {\"a\": \"zebra"), "cut short {…");
    }

    #[test]
    fn scrub_blanks_keys_and_tokens_but_keeps_ids_and_paths() {
        // Split so the source holds no key-shaped text for secret scanners.
        let key = concat!("sk-ant-", "api03-", "AbCdEf_ghIJ-klMNopQRstUVwxYZ0123456789-abcdefghijklmnopqrstu");
        let s = scrub(&format!("Anthropic rejected key {key} (401)"));
        assert_eq!(s, "Anthropic rejected key sk-ant-… (401)");
        let token = "a".repeat(20) + &"B9".repeat(10);
        assert_eq!(scrub(&format!("bad token {token}!")), "bad token …!");
        let id = "5f0c2a7e-1b2c-4d3e-8f90-123456789abc";
        assert_eq!(scrub(&format!("project {id} not found")), format!("project {id} not found"));
        let path = "/Volumes/media/obs_recordings/2026-11-03 10-12-01.mkv";
        assert_eq!(scrub(&format!("ffmpeg join failed: {path}")), format!("ffmpeg join failed: {path}"));
    }

    #[test]
    fn scrub_shows_home_as_tilde_and_caps_length() {
        if let Some(home) = dirs::home_dir() {
            let s = scrub(&format!("file {}/Movies/talk.mp4", home.display()));
            assert_eq!(s, "file ~/Movies/talk.mp4");
        }
        let long = "word ".repeat(200);
        let s = scrub(&long);
        assert_eq!(s.chars().count(), ERR_CAP + 1);
        assert!(s.ends_with('…'));
    }

    /// The no-secrets rule, end to end: an outline, a shorts scan and a chat
    /// turn against fake local and Hosted models, with a real-looking key
    /// stored and a marker word in the transcript, the instruction, every
    /// reply, and a reply that fails to parse with the marker in escaped
    /// quotes (serde quotes it back in its error).
    #[tokio::test]
    async fn the_log_never_holds_transcript_text_prompts_replies_or_keys() {
        use crate::engine::route::tests::{hosted_editor, unreachable, FAKE_USAGE};
        const MARK: &str = "zebracorn";
        // Split so the source holds no key-shaped text for secret scanners.
        const KEY: &str = concat!(
            "sk-ant-",
            "api03-",
            "Zx9qL2mN8vB4cR7tY1uI6oP3aS5dF0gH-jK2lQ8wE4rT6yU1iO9pA3sD7fG5hJ0kL-AbCdEfAA"
        );
        let cap = capture::Capture::start();

        async fn project_with_marked_transcript(e: &Editor) -> (Uuid, usize) {
            let p = e.create_project("t", Some("/demo/footage.mp4")).await.unwrap();
            e.transcribe(p.id, None).await.unwrap();
            let t = e.get_transcript(p.id).unwrap().unwrap();
            let changes: Vec<(Uuid, String)> = t
                .segments
                .iter()
                .filter(|s| !s.is_silence && !s.text.is_empty())
                .map(|s| (s.id, format!("{} {MARK}", s.text)))
                .collect();
            e.update_segment_texts(p.id, &changes).unwrap();
            (p.id, changes.len())
        }
        // A reply serde can't read: its error quotes the reply, escapes and all.
        let unreadable = || Ok(format!(r#"[{{"title":"x","first":"the "{MARK}" line","last":3}}]"#));

        // 1. Outline: two unreadable replies → falls back to rules.
        let (e, _, _) = hosted_editor(vec![unreadable(), unreadable()], true);
        e.set_hosted_key_for_tests(Some(KEY)).unwrap();
        let (pid, _) = project_with_marked_transcript(&e).await;
        assert_eq!(crate::story::outline(&e, pid).await.unwrap().method, "heuristic");

        // 2. A shorts scan: a good outline, then picks with the marker in
        // the title and reason (every line range, so some fit the length
        // bounds); any later topic gets "{}" and fails.
        let outline_reply = |n: usize| {
            Ok(format!(
                r#"[{{"title":"{MARK} one","summary":"{MARK}","role":"hook","cut_priority":1,"first":0,"last":{}}},
                   {{"title":"{MARK} two","summary":"{MARK}","role":"point","cut_priority":2,"first":{},"last":{}}}]"#,
                n / 2,
                n / 2 + 1,
                n - 1
            ))
        };
        let picks = || {
            let all: Vec<serde_json::Value> = (1..=30)
                .flat_map(|a| (a..=30).map(move |b| (a, b)))
                .map(|(a, b)| {
                    serde_json::json!({"first": a, "last": b, "hook": 4, "standalone": 4, "payoff": 4,
                        "ending": 4, "title": format!("{MARK}!"), "reason": format!("says {MARK}")})
                })
                .collect();
            Ok(serde_json::Value::Array(all).to_string())
        };
        let (e2, hosted2, _) = hosted_editor(vec![], true);
        e2.set_hosted_key_for_tests(Some(KEY)).unwrap();
        let (pid2, n) = project_with_marked_transcript(&e2).await;
        hosted2.script.lock().unwrap().extend([outline_reply(n), picks()]);
        crate::clip_finder::find_clips(&e2, pid2, Default::default()).await.unwrap();

        // 3. A chat turn: Anthropic is offline, so the local model answers.
        let (e3, _, _) = hosted_editor(vec![unreachable()], true);
        e3.set_hosted_key_for_tests(Some(KEY)).unwrap();
        let (pid3, _) = project_with_marked_transcript(&e3).await;
        let history = [crate::agent::HistoryTurn { role: "user".into(), text: format!("earlier {MARK}") }];
        let source = crate::model::ActionSource::LocalAi;
        crate::agent::run_instruction(&e3, pid3, &format!("cut the {MARK} part"), &history, source).await.unwrap();

        let text = cap.text();
        if std::env::var("SHOW_APPLOG").is_ok() {
            println!("{text}");
        }
        assert!(!text.to_lowercase().contains(MARK), "a marker leaked:\n{text}");
        assert!(!text.contains("api03") && !text.contains("sk-test"), "a key leaked:\n{text}");
        for prompt in ["talking-head", "STORY", "short-form", "Instruction", "Project context"] {
            assert!(!text.contains(prompt), "a prompt leaked ({prompt}):\n{text}");
        }

        let lines = cap.lines();
        let find = |event: &str, step: &str| -> Vec<&serde_json::Value> {
            lines.iter().filter(|l| l["event"] == event && l["step"] == step).collect()
        };
        // The outline's two failed requests and its fallback, with why.
        let bad = find("ai_request", "outline");
        assert_eq!(bad[0]["outcome"], "bad_json", "{text}");
        assert_eq!(bad[0]["attempts"], 2);
        assert_eq!(bad[0]["input_tokens"], 2 * FAKE_USAGE.input_tokens, "tokens summed over the retry");
        assert_eq!(bad[0]["served_by"], "claude-opus-5-5");
        let outline = find("ai_step", "outline");
        assert_eq!(outline[0]["outcome"], "fallback");
        assert_eq!(outline[0]["reason"], "inference");
        assert_eq!(outline[0]["mode"], "hosted");
        assert!(outline[0]["error"].as_str().unwrap().contains("bad JSON in outline response"), "{text}");
        // The scan: the outline on Claude, then picks with failed topics counted.
        assert_eq!(outline[1]["outcome"], "ok", "{text}");
        assert_eq!(outline[1]["models"][0], "claude-opus-5-5");
        let picks = find("ai_step", "short_picks");
        assert_eq!(picks[0]["outcome"], "ok", "{text}");
        let topics = picks[0]["topics"].as_u64().unwrap();
        assert!(topics >= 1, "{text}");
        assert_eq!(picks[0]["topics_failed"], topics - 1, "{text}");
        assert_eq!(picks[0]["models"][0], "claude-opus-5-5");
        assert_eq!(find("ai_request", "short_picks").len() as u64, topics);
        // The chat: Hosted went offline (one model_route line), local answered.
        let routes: Vec<_> = lines.iter().filter(|l| l["event"] == "model_route").collect();
        assert_eq!(routes.len(), 1, "{text}");
        assert_eq!((routes[0]["effective"].as_str(), routes[0]["reason"].as_str()), (Some("local"), Some("offline")));
        let chat = find("ai_step", "chat");
        assert_eq!(chat[0]["outcome"], "ok", "{text}");
        assert_eq!(chat[0]["turns"], 1);
        assert_eq!(find("ai_request", "chat")[0]["hosted"], false);
        // Transcription lines from the three imports.
        assert_eq!(lines.iter().filter(|l| l["event"] == "transcribe_end").count(), 3);
    }

    #[test]
    fn stderr_tail_keeps_the_end_and_ffmpegs_own_quotes() {
        let head = "ffmpeg version 7.1 banner\n".repeat(100);
        let stderr = format!("{head}[in#0 @ 0x1] Error opening input: No such file 'talk.mkv'\n");
        let t = scrub_tail(&stderr);
        assert!(t.chars().count() <= TAIL_KEEP + 1);
        assert!(t.ends_with("No such file 'talk.mkv'"), "{t}");
        assert!(t.starts_with('…'));
        assert_eq!(scrub_tail(&format!("key sk-ant-{}", "x".repeat(50))), "key sk-ant-…");
    }
}
