//! The LOCAL / HOSTED switch: where each AI request runs.
//!
//! `Editor::inference()` is the one door every AI step goes through. In
//! Local mode it hands out the local client, as always. In Hosted mode it
//! hands out a [`HostedRouter`] — one per run — that sends requests to Claude
//! and keeps three promises:
//!
//! 1. **One model per run.** Once a run has used the local model it never
//!    goes back to Claude (local-made turns are never sent to Anthropic).
//!    A conversation run (chat, story edit) that already used Claude stops
//!    rather than switch models mid-way.
//! 2. **Errors stay visible.** No key, no consent, a rejected key or a
//!    rejected request is returned as `CoreError::Hosted` — never quietly
//!    replaced by a local or rule-based answer.
//! 3. **The user's choice wins mid-run.** Each request re-reads the mode: a
//!    switch to Local sends nothing more to Anthropic.
//!
//! Only "unreachable" (offline, rate limited, Anthropic down) falls back to
//! the local model. [`RouteState`] keeps the key (read once from the OS key
//! store) and what the top-bar chip shows, and sends `model-route` events
//! when that changes.

use super::Editor;
use crate::adapters::anthropic::{AnthropicClient, HostedBackend, HostedFailure};
use crate::adapters::inference::{ChatRequest, ChatResponse, InferenceClient, ServedBy};
use crate::adapters::OpenAiCompatClient;
use crate::error::{CoreError, HostedErrorKind, Result};
use crate::events::CoreEvent;
use crate::model::{
    hosted_model_name, HostedKeyResult, KeyCheck, ModelMode, ModelStatus, Preferences, RouteReason, HOSTED_MODELS,
};
use crate::secrets::SecretStore;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex};

/// How a run reacts to Anthropic going away after it has used it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPolicy {
    /// Independent requests (shorts picks per topic, outline chunks, cleanup): the
    /// rest of the run finishes on the local model.
    Batch,
    /// A conversation (chat, story edit) whose later steps build on earlier
    /// ones: stop instead of continuing on another model.
    Conversation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pin {
    Unpinned,
    Hosted,
    Local,
    /// A conversation stopped rather than switch models: it stays stopped.
    Stopped,
}

/// What we know about the stored key.
enum KeyState {
    Unread,
    Present(String),
    Absent,
    /// The key store couldn't be read (e.g. access denied): remembered so the
    /// Setup poll doesn't ask the OS again every few seconds.
    Failed(String),
}

#[derive(Default)]
struct ChipStatus {
    /// None until a request ran since the mode was last set.
    effective: Option<ModelMode>,
    reason: Option<RouteReason>,
    last_ok_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    /// The last `(effective, reason)` sent as a `model-route` event.
    sent: Option<(ModelMode, Option<RouteReason>)>,
}

/// Hosted mode's shared state, in the Editor.
pub struct RouteState {
    secrets: Mutex<Arc<dyn SecretStore>>,
    key: Mutex<KeyState>,
    /// One client per key, reused across runs (keeps its connections).
    client: Mutex<Option<(String, Arc<AnthropicClient>)>>,
    status: Mutex<ChipStatus>,
    /// Tests: stand-ins for the local client and for Anthropic.
    overrides: Mutex<Option<(Arc<dyn InferenceClient>, Arc<dyn HostedBackend>)>>,
}

impl RouteState {
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            secrets: Mutex::new(secrets),
            key: Mutex::new(KeyState::Unread),
            client: Mutex::new(None),
            status: Mutex::new(ChipStatus::default()),
            overrides: Mutex::new(None),
        }
    }

    /// The key, read from the store the first time only (a failed read is
    /// remembered too, until the key is next set or forgotten).
    fn key(&self) -> Result<Option<String>> {
        let mut state = self.key.lock().unwrap();
        match &*state {
            KeyState::Present(k) => return Ok(Some(k.clone())),
            KeyState::Absent => return Ok(None),
            KeyState::Failed(e) => return Err(CoreError::Unavailable(e.clone())),
            KeyState::Unread => {}
        }
        let store = self.secrets.lock().unwrap().clone();
        match store.get() {
            Ok(Some(k)) => {
                *state = KeyState::Present(k.clone());
                Ok(Some(k))
            }
            Ok(None) => {
                *state = KeyState::Absent;
                Ok(None)
            }
            Err(e) => {
                *state = KeyState::Failed(e.to_string());
                Err(e)
            }
        }
    }

    fn key_present(&self) -> bool {
        // Setup polls status every few seconds: never touch the store for it
        // once the key has been read.
        matches!(self.key(), Ok(Some(_)))
    }

    fn store_key(&self, key: Option<&str>) -> Result<()> {
        let store = self.secrets.lock().unwrap().clone();
        match key {
            Some(k) => store.set(k)?,
            None => store.delete()?,
        }
        *self.key.lock().unwrap() = match key {
            Some(k) => KeyState::Present(k.to_string()),
            None => KeyState::Absent,
        };
        Ok(())
    }

    /// The client for the CURRENT key (a key replaced mid-run takes effect on
    /// the next request).
    fn hosted_client(&self, key: &str) -> Arc<AnthropicClient> {
        let mut cached = self.client.lock().unwrap();
        if let Some((k, c)) = cached.as_ref() {
            if k == key {
                return c.clone();
            }
        }
        let c = Arc::new(AnthropicClient::new(key));
        *cached = Some((key.to_string(), c.clone()));
        c
    }
}

impl Editor {
    /// The client for one run of an AI step that sends independent requests.
    pub fn inference(&self) -> Result<Box<dyn InferenceClient>> {
        self.inference_with(RunPolicy::Batch)
    }

    /// The client for one conversation run (chat, story edit).
    pub fn inference_conversation(&self) -> Result<Box<dyn InferenceClient>> {
        self.inference_with(RunPolicy::Conversation)
    }

    fn inference_with(&self, policy: RunPolicy) -> Result<Box<dyn InferenceClient>> {
        let prefs = self.inner.store.load_preferences()?;
        let overrides = self.inner.route.overrides.lock().unwrap().clone();
        let local: Arc<dyn InferenceClient> = match &overrides {
            Some((local, _)) => local.clone(),
            None => Arc::new(OpenAiCompatClient::new(&prefs.inference_endpoint, None)),
        };
        if prefs.model_mode == ModelMode::Local {
            return Ok(Box::new(Shared(local)));
        }
        let hosted_override = overrides.map(|(_, hosted)| hosted);
        Ok(Box::new(HostedRouter { editor: self.clone(), hosted_override, local, policy, pin: Mutex::new(Pin::Unpinned) }))
    }

    /// Tests: route AI requests to these instead of a local server and
    /// Anthropic (the key store is already in memory in `test_instance`).
    pub fn set_inference_for_tests(&self, local: Arc<dyn InferenceClient>, hosted: Arc<dyn HostedBackend>) {
        *self.inner.route.overrides.lock().unwrap() = Some((local, hosted));
    }

    /// Tests: store a key without checking it against Anthropic.
    pub fn set_hosted_key_for_tests(&self, key: Option<&str>) -> Result<()> {
        self.inner.route.store_key(key)
    }

    /// Tests: keep the key in memory, never in the real OS key store.
    pub fn set_secret_store_for_tests(&self, store: Arc<dyn SecretStore>) {
        *self.inner.route.secrets.lock().unwrap() = store;
        *self.inner.route.key.lock().unwrap() = KeyState::Unread;
    }

    /// The model the next AI step will use, for progress text: "Claude Opus
    /// 5.5" or "the on-device model".
    pub fn model_label(&self) -> String {
        match self.inner.store.load_preferences() {
            // Hosted, unless the last request had to fall back.
            Ok(p) if p.model_mode == ModelMode::Hosted
                && self.inner.route.status.lock().unwrap().effective != Some(ModelMode::Local) =>
            {
                hosted_model_name(&p.hosted_model)
            }
            _ => "the on-device model".into(),
        }
    }

    /// The switch as the app shows it. No network, no key-store read after
    /// the first.
    pub fn model_status(&self) -> Result<ModelStatus> {
        let prefs = self.inner.store.load_preferences()?;
        Ok(self.status_for(&prefs))
    }

    fn status_for(&self, prefs: &Preferences) -> ModelStatus {
        // Read the key first: never hold the status lock across a key-store read.
        let key_present = self.inner.route.key_present();
        let s = self.inner.route.status.lock().unwrap();
        ModelStatus {
            mode: prefs.model_mode,
            hosted_model: prefs.hosted_model.clone(),
            hosted_model_name: hosted_model_name(&prefs.hosted_model),
            key_present,
            consented: prefs.hosted_consent_at.is_some(),
            effective: if prefs.model_mode == ModelMode::Local {
                ModelMode::Local
            } else {
                s.effective.unwrap_or(prefs.model_mode)
            },
            reason: if prefs.model_mode == ModelMode::Local { None } else { s.reason },
            last_ok_at: s.last_ok_at,
            last_error: s.last_error.clone(),
        }
    }

    /// The user's switch. Hosted needs a stored key, and the user's consent
    /// the first time (`consent: true` once they've read the notice).
    pub fn set_model_mode(&self, mode: ModelMode, consent: bool) -> Result<ModelStatus> {
        let _write = self.inner.prefs_write.lock().unwrap();
        let mut prefs = self.inner.store.load_preferences()?;
        if mode == ModelMode::Hosted {
            if self.inner.route.key()?.is_none() {
                return Err(CoreError::hosted(HostedErrorKind::NoKey, "add an Anthropic API key in Setup first"));
            }
            if prefs.hosted_consent_at.is_none() {
                if !consent {
                    return Err(CoreError::hosted(
                        HostedErrorKind::NotConsented,
                        "confirm what Hosted mode sends before turning it on",
                    ));
                }
                prefs.hosted_consent_at = Some(Utc::now());
            }
        }
        prefs.model_mode = mode;
        self.inner.store.save_preferences(&prefs)?;
        {
            let mut s = self.inner.route.status.lock().unwrap();
            s.effective = None;
            s.reason = None;
        }
        self.send_route(mode, mode, None);
        Ok(self.status_for(&prefs))
    }

    pub fn set_hosted_model(&self, model: &str) -> Result<ModelStatus> {
        if !HOSTED_MODELS.iter().any(|(id, _)| *id == model) {
            let known: Vec<&str> = HOSTED_MODELS.iter().map(|(id, _)| *id).collect();
            return Err(CoreError::InvalidArg(format!("unknown hosted model {model}; one of {}", known.join(", "))));
        }
        let _write = self.inner.prefs_write.lock().unwrap();
        let mut prefs = self.inner.store.load_preferences()?;
        prefs.hosted_model = model.to_string();
        self.inner.store.save_preferences(&prefs)?;
        Ok(self.status_for(&prefs))
    }

    /// Save (`Some`) or forget (`None`) the API key. A new key is checked
    /// first without spending tokens and kept only if it works, or if it
    /// couldn't be checked because Anthropic is unreachable. Forgetting the
    /// key switches to Local.
    pub async fn set_hosted_key(&self, key: Option<&str>) -> Result<HostedKeyResult> {
        let Some(key) = key.map(str::trim) else {
            self.inner.route.store_key(None)?;
            let prefs = {
                let _write = self.inner.prefs_write.lock().unwrap();
                let mut prefs = self.inner.store.load_preferences()?;
                if prefs.model_mode == ModelMode::Hosted {
                    prefs.model_mode = ModelMode::Local;
                    self.inner.store.save_preferences(&prefs)?;
                    self.send_route(ModelMode::Local, ModelMode::Local, None);
                }
                prefs
            };
            return Ok(HostedKeyResult { status: self.status_for(&prefs), check: None, saved: false });
        };
        if key.is_empty() {
            return Err(CoreError::InvalidArg("the API key is empty".into()));
        }
        // Keys are printable ASCII; anything else is a paste accident (a line
        // break, a stray character) that would only fail later.
        if !key.chars().all(|c| c.is_ascii_graphic()) {
            return Err(CoreError::InvalidArg(
                "the API key has spaces, line breaks or other characters an Anthropic key doesn't have".into(),
            ));
        }
        let prefs = self.inner.store.load_preferences()?;
        let (check, save) = match self.check_key(key, &prefs.hosted_model).await {
            Ok(ms) => (KeyCheck { ok: true, latency_ms: Some(ms), error: None }, true),
            Err(HostedFailure::Unreachable { detail, .. }) => (
                KeyCheck { ok: false, latency_ms: None, error: Some(format!("saved; couldn't verify while offline ({detail})")) },
                true,
            ),
            Err(HostedFailure::Fatal(e)) => (KeyCheck { ok: false, latency_ms: None, error: Some(e.to_string()) }, false),
        };
        if save {
            self.inner.route.store_key(Some(key))?;
            let mut s = self.inner.route.status.lock().unwrap();
            // A new key replaces whatever was wrong with the old one (or the
            // lack of one), checked or not.
            if s.reason == Some(RouteReason::KeyRejected) || s.reason == Some(RouteReason::NoKey) {
                s.reason = None;
            }
            if check.ok {
                s.last_ok_at = Some(Utc::now());
                s.last_error = None;
            }
        }
        Ok(HostedKeyResult { status: self.status_for(&prefs), check: Some(check), saved: save })
    }

    /// Setup's "Test": check the stored key and model again.
    pub async fn check_hosted_key(&self) -> Result<KeyCheck> {
        let prefs = self.inner.store.load_preferences()?;
        let Some(key) = self.inner.route.key()? else {
            return Ok(KeyCheck { ok: false, latency_ms: None, error: Some("no API key is stored".into()) });
        };
        Ok(match self.check_key(&key, &prefs.hosted_model).await {
            Ok(ms) => {
                let mut s = self.inner.route.status.lock().unwrap();
                s.last_ok_at = Some(Utc::now());
                s.last_error = None;
                KeyCheck { ok: true, latency_ms: Some(ms), error: None }
            }
            Err(f) => {
                let e = CoreError::from(f).to_string();
                self.inner.route.status.lock().unwrap().last_error = Some(e.clone());
                KeyCheck { ok: false, latency_ms: None, error: Some(e) }
            }
        })
    }

    async fn check_key(&self, key: &str, model: &str) -> std::result::Result<u64, HostedFailure> {
        let elapsed = AnthropicClient::new(key).check_model(model).await?;
        Ok(elapsed.as_millis() as u64)
    }

    /// Record the outcome of a hosted request and tell the chip if what it
    /// shows changed.
    fn record_route(&self, effective: ModelMode, reason: Option<RouteReason>, error: Option<String>, ok: bool) {
        let mode = self.inner.store.load_preferences().map(|p| p.model_mode).unwrap_or(ModelMode::Local);
        // The app log gets the same line the chip does: once per change.
        let detail = error.as_deref().map(crate::applog::ErrText::new);
        let changed = {
            let mut s = self.inner.route.status.lock().unwrap();
            s.effective = Some(effective);
            s.reason = reason;
            if ok {
                s.last_ok_at = Some(Utc::now());
                s.last_error = None;
            }
            if error.is_some() {
                s.last_error = error;
            }
            let now = (effective, reason);
            let changed = s.sent != Some(now);
            s.sent = Some(now);
            changed
        };
        if changed {
            let level = if effective == ModelMode::Hosted && reason.is_none() {
                crate::applog::Level::Info
            } else {
                crate::applog::Level::Warn
            };
            crate::applog::write(level, crate::applog::Event::ModelRoute { effective, reason, detail });
            crate::events::send(&self.sink(), CoreEvent::ModelRoute { mode, effective, reason });
        }
    }

    fn send_route(&self, mode: ModelMode, effective: ModelMode, reason: Option<RouteReason>) {
        self.inner.route.status.lock().unwrap().sent = Some((effective, reason));
        crate::events::send(&self.sink(), CoreEvent::ModelRoute { mode, effective, reason });
    }
}

/// An `Arc`'d client as a `Box<dyn InferenceClient>`.
struct Shared(Arc<dyn InferenceClient>);

#[async_trait]
impl InferenceClient for Shared {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.0.chat(request).await
    }
    async fn healthy(&self) -> bool {
        self.0.healthy().await
    }
}

/// One run's client in Hosted mode. See the module docs for its rules.
pub struct HostedRouter {
    editor: Editor,
    /// Tests: a stand-in for Anthropic. Otherwise each request uses the
    /// client for the key stored at that moment.
    hosted_override: Option<Arc<dyn HostedBackend>>,
    local: Arc<dyn InferenceClient>,
    policy: RunPolicy,
    pin: Mutex<Pin>,
}

impl HostedRouter {
    fn pin(&self) -> Pin {
        *self.pin.lock().unwrap()
    }

    fn set_pin(&self, p: Pin) {
        *self.pin.lock().unwrap() = p;
    }

    /// The request on the local model: its own model name, nothing from
    /// Claude's replies (`provider_blocks` is never serialised).
    async fn local_chat(&self, request: &ChatRequest, prefs: &Preferences) -> Result<ChatResponse> {
        let mut req = request.clone();
        req.model = prefs.inference_model.clone();
        let mut resp = self.local.chat(req).await?;
        if resp.served_by.is_none() {
            resp.served_by = Some(ServedBy { model: prefs.inference_model.clone(), hosted: false });
        }
        Ok(resp)
    }

    fn stop(&self, why: &str) -> CoreError {
        self.set_pin(Pin::Stopped);
        CoreError::Unavailable(format!("{why}; stopped so this run doesn't switch models half-way"))
    }
}

#[async_trait]
impl InferenceClient for HostedRouter {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let prefs = self.editor.inner.store.load_preferences()?;
        let pin = self.pin();
        if pin == Pin::Stopped {
            return Err(CoreError::Unavailable("this run was stopped so it doesn't switch models half-way".into()));
        }

        // The user switched to Local since this run began.
        if prefs.model_mode == ModelMode::Local {
            if pin == Pin::Hosted && self.policy == RunPolicy::Conversation {
                return Err(self.stop("switched to Local"));
            }
            self.set_pin(Pin::Local);
            return self.local_chat(&request, &prefs).await;
        }
        if pin == Pin::Local {
            return self.local_chat(&request, &prefs).await;
        }

        // Hosted: key and consent first.
        let key = match self.editor.inner.route.key() {
            Ok(Some(k)) => k,
            Ok(None) => {
                let msg = "Hosted mode is on but no Anthropic API key is stored. Add it in Setup.";
                self.editor.record_route(ModelMode::Hosted, Some(RouteReason::NoKey), Some(msg.into()), false);
                return Err(CoreError::hosted(HostedErrorKind::NoKey, msg));
            }
            Err(e) => {
                self.editor.record_route(ModelMode::Hosted, Some(RouteReason::NoKey), Some(e.to_string()), false);
                return Err(e);
            }
        };
        if prefs.hosted_consent_at.is_none() {
            let msg = "Hosted mode needs your confirmation of what it sends. Switch it on again in the app.";
            self.editor.record_route(ModelMode::Hosted, None, Some(msg.into()), false);
            return Err(CoreError::hosted(HostedErrorKind::NotConsented, msg));
        }
        let hosted: Arc<dyn HostedBackend> = match &self.hosted_override {
            Some(h) => h.clone(),
            None => self.editor.inner.route.hosted_client(&key),
        };

        let mut req = request.clone();
        req.model = prefs.hosted_model.clone();
        match hosted.send(&req).await {
            Ok(resp) => {
                self.set_pin(Pin::Hosted);
                self.editor.record_route(ModelMode::Hosted, None, None, true);
                Ok(resp)
            }
            Err(HostedFailure::Unreachable { reason, detail }) => {
                self.editor.record_route(ModelMode::Local, Some(reason), Some(detail.clone()), false);
                if pin == Pin::Hosted && self.policy == RunPolicy::Conversation {
                    return Err(self.stop(&format!("Anthropic stopped answering ({detail})")));
                }
                self.set_pin(Pin::Local);
                self.local_chat(&request, &prefs).await
            }
            Err(HostedFailure::Fatal(e)) => {
                let reason = match &e {
                    CoreError::Hosted { kind: HostedErrorKind::KeyRejected, .. } => Some(RouteReason::KeyRejected),
                    _ => None,
                };
                self.editor.record_route(ModelMode::Hosted, reason, Some(e.to_string()), false);
                Err(e)
            }
        }
    }

    /// No network call. In Hosted mode the run can always start: key or
    /// consent problems come back from `chat` as visible errors, which a
    /// "not healthy" answer would hide behind rule-based results.
    async fn healthy(&self) -> bool {
        let mode = self.editor.inner.store.load_preferences().map(|p| p.model_mode).unwrap_or(ModelMode::Local);
        if mode == ModelMode::Local || self.pin() == Pin::Local {
            return self.local.healthy().await;
        }
        true
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::adapters::inference::ChatMessage;
    use crate::events::EventSink;
    use crate::secrets::MemoryStore;
    use std::collections::VecDeque;

    /// A scripted Anthropic: each request takes the next outcome, and costs
    /// [`FAKE_USAGE`] tokens.
    pub(crate) struct FakeHosted {
        pub script: Mutex<VecDeque<std::result::Result<String, HostedFailure>>>,
        pub models: Mutex<Vec<String>>,
    }

    pub(crate) const FAKE_USAGE: crate::adapters::inference::Usage =
        crate::adapters::inference::Usage { input_tokens: 1234, output_tokens: 56 };

    impl FakeHosted {
        pub(crate) fn new(script: Vec<std::result::Result<String, HostedFailure>>) -> Arc<Self> {
            Arc::new(Self { script: Mutex::new(script.into()), models: Mutex::new(vec![]) })
        }
    }

    #[async_trait]
    impl HostedBackend for FakeHosted {
        async fn send(&self, request: &ChatRequest) -> std::result::Result<ChatResponse, HostedFailure> {
            self.models.lock().unwrap().push(request.model.clone());
            let next = self.script.lock().unwrap().pop_front().unwrap_or_else(|| Ok("{}".into()));
            next.map(|text| ChatResponse {
                message: ChatMessage::assistant(&text),
                served_by: Some(ServedBy { model: request.model.clone(), hosted: true }),
                usage: Some(FAKE_USAGE),
            })
        }
    }

    /// A local model that answers `local`, or is down.
    pub(crate) struct FakeLocal {
        pub up: bool,
        pub models: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl InferenceClient for FakeLocal {
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
            self.models.lock().unwrap().push(request.model.clone());
            if !self.up {
                return Err(CoreError::Inference("local model unreachable".into()));
            }
            Ok(ChatResponse::new(ChatMessage::assistant("local")))
        }
        async fn healthy(&self) -> bool {
            self.up
        }
    }

    pub(crate) fn unreachable() -> std::result::Result<String, HostedFailure> {
        Err(HostedFailure::Unreachable { reason: RouteReason::Offline, detail: "no network".into() })
    }

    pub(crate) fn rejected() -> std::result::Result<String, HostedFailure> {
        Err(HostedFailure::Fatal(CoreError::hosted(HostedErrorKind::KeyRejected, "Anthropic rejected the API key (401). Update it in Setup.")))
    }

    /// An editor in Hosted mode with a key and consent, and fakes wired in.
    pub(crate) fn hosted_editor(script: Vec<std::result::Result<String, HostedFailure>>, local_up: bool) -> (Editor, Arc<FakeHosted>, Arc<FakeLocal>) {
        let e = Editor::test_instance();
        let hosted = FakeHosted::new(script);
        let local = Arc::new(FakeLocal { up: local_up, models: Mutex::new(vec![]) });
        e.set_inference_for_tests(local.clone(), hosted.clone());
        e.inner.route.store_key(Some("sk-test")).unwrap();
        e.set_model_mode(ModelMode::Hosted, true).unwrap();
        (e, hosted, local)
    }

    fn ask() -> ChatRequest {
        ChatRequest {
            model: "whatever-the-call-site-sent".into(),
            messages: vec![ChatMessage::user("hi")],
            tools: None,
            temperature: 0.2,
            response_format: None,
        }
    }

    #[tokio::test]
    async fn local_mode_never_touches_anthropic() {
        let e = Editor::test_instance();
        let hosted = FakeHosted::new(vec![]);
        let local = Arc::new(FakeLocal { up: true, models: Mutex::new(vec![]) });
        e.set_inference_for_tests(local.clone(), hosted.clone());
        let c = e.inference().unwrap();
        assert_eq!(c.chat(ask()).await.unwrap().message.content.as_deref(), Some("local"));
        assert!(hosted.models.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn hosted_sends_the_hosted_model_and_local_its_own() {
        let (e, hosted, local) = hosted_editor(vec![Ok("claude".into()), unreachable()], true);
        let c = e.inference().unwrap();
        let r = c.chat(ask()).await.unwrap();
        assert_eq!(r.served_by.unwrap(), ServedBy { model: "claude-opus-5-5".into(), hosted: true });
        // Batch run: Anthropic goes away → the rest runs locally, with the
        // local model's name (never a Claude id to Ollama).
        let r = c.chat(ask()).await.unwrap();
        assert_eq!(r.message.content.as_deref(), Some("local"));
        assert!(!r.served_by.unwrap().hosted);
        assert_eq!(*hosted.models.lock().unwrap(), vec!["claude-opus-5-5", "claude-opus-5-5"]);
        assert_eq!(*local.models.lock().unwrap(), vec![e.get_preferences().unwrap().inference_model]);
        // …and never goes back to Anthropic in the same run.
        c.chat(ask()).await.unwrap();
        assert_eq!(hosted.models.lock().unwrap().len(), 2);
        // The chip says why.
        let s = e.model_status().unwrap();
        assert_eq!((s.mode, s.effective, s.reason), (ModelMode::Hosted, ModelMode::Local, Some(RouteReason::Offline)));
        // The next run tries Anthropic again.
        e.inference().unwrap().chat(ask()).await.unwrap();
        assert_eq!(hosted.models.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_conversation_that_used_claude_stops_instead_of_switching() {
        let (e, _, local) = hosted_editor(vec![Ok("step 1".into()), unreachable()], true);
        let c = e.inference_conversation().unwrap();
        c.chat(ask()).await.unwrap();
        let err = c.chat(ask()).await.unwrap_err().to_string();
        assert!(err.contains("stopped"), "{err}");
        assert!(local.models.lock().unwrap().is_empty(), "no local step after a Claude step");
        // Unreachable from the very first request: the whole conversation
        // runs locally.
        let (e, hosted, local) = hosted_editor(vec![unreachable()], true);
        let c = e.inference_conversation().unwrap();
        assert_eq!(c.chat(ask()).await.unwrap().message.content.as_deref(), Some("local"));
        c.chat(ask()).await.unwrap();
        assert_eq!(hosted.models.lock().unwrap().len(), 1);
        assert_eq!(local.models.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_rejected_key_is_an_error_not_a_fallback() {
        let (e, _, local) = hosted_editor(vec![rejected()], true);
        let err = e.inference().unwrap().chat(ask()).await.unwrap_err();
        assert!(err.is_fatal_model_error());
        assert!(local.models.lock().unwrap().is_empty(), "never papered over locally");
        let s = e.model_status().unwrap();
        assert_eq!(s.reason, Some(RouteReason::KeyRejected));
        assert!(s.last_error.unwrap().contains("401"));
    }

    fn fatal(kind: HostedErrorKind) -> std::result::Result<String, HostedFailure> {
        Err(HostedFailure::Fatal(CoreError::hosted(kind, "x")))
    }

    #[tokio::test]
    async fn bad_requests_refusals_and_truncation_are_errors_on_both_policies() {
        for kind in [HostedErrorKind::BadRequest, HostedErrorKind::Refused, HostedErrorKind::Truncated] {
            for convo in [false, true] {
                let (e, _, local) = hosted_editor(vec![fatal(kind)], true);
                let c = if convo { e.inference_conversation() } else { e.inference() }.unwrap();
                let err = c.chat(ask()).await.unwrap_err();
                assert!(matches!(err, CoreError::Hosted { kind: k, .. } if k == kind), "{kind:?} convo={convo}");
                assert!(local.models.lock().unwrap().is_empty(), "never replaced by a local answer");
            }
        }
        // Only account/config problems are fatal for the caller; a refusal
        // or truncation is about one request.
        assert!(CoreError::hosted(HostedErrorKind::BadRequest, "").is_fatal_model_error());
        assert!(!CoreError::hosted(HostedErrorKind::Refused, "").is_fatal_model_error());
        assert!(!CoreError::hosted(HostedErrorKind::Truncated, "").is_fatal_model_error());
    }

    #[tokio::test]
    async fn a_stopped_conversation_stays_stopped() {
        let (e, hosted, _) = hosted_editor(vec![Ok("a".into()), unreachable(), Ok("c".into())], true);
        let c = e.inference_conversation().unwrap();
        c.chat(ask()).await.unwrap();
        assert!(c.chat(ask()).await.is_err());
        assert!(c.chat(ask()).await.unwrap_err().to_string().contains("stopped"));
        assert_eq!(hosted.models.lock().unwrap().len(), 2, "nothing more reaches Anthropic");
    }

    #[tokio::test]
    async fn consent_withdrawn_behind_the_apps_back_is_visible() {
        let (e, hosted, _) = hosted_editor(vec![], true);
        let mut prefs = e.get_preferences().unwrap();
        prefs.hosted_consent_at = None;
        e.set_preferences(prefs).unwrap();
        let err = e.inference_conversation().unwrap().chat(ask()).await.unwrap_err();
        assert_eq!(err.code(), "hosted_not_consented");
        assert!(hosted.models.lock().unwrap().is_empty());
        assert!(e.model_status().unwrap().last_error.is_some());
    }

    #[tokio::test]
    async fn a_key_store_that_fails_is_asked_once_and_reported() {
        struct Broken(Mutex<usize>);
        impl SecretStore for Broken {
            fn get(&self) -> Result<Option<String>> {
                *self.0.lock().unwrap() += 1;
                Err(CoreError::Unavailable("access denied".into()))
            }
            fn set(&self, _: &str) -> Result<()> {
                Ok(())
            }
            fn delete(&self) -> Result<()> {
                Ok(())
            }
        }
        let e = Editor::test_instance();
        let store = Arc::new(Broken(Mutex::new(0)));
        e.set_secret_store_for_tests(store.clone());
        for _ in 0..5 {
            assert!(!e.model_status().unwrap().key_present);
        }
        assert_eq!(*store.0.lock().unwrap(), 1, "the failure is remembered");
        assert!(e.set_model_mode(ModelMode::Hosted, true).unwrap_err().to_string().contains("access denied"));
    }

    #[tokio::test]
    async fn a_pasted_key_with_a_line_break_is_refused_before_any_check() {
        let e = Editor::test_instance();
        assert_eq!(e.set_hosted_key(Some("sk-ant\nmore")).await.unwrap_err().code(), "invalid_argument");
        assert_eq!(e.set_hosted_key(Some("   ")).await.unwrap_err().code(), "invalid_argument");
    }

    #[tokio::test]
    async fn offline_with_no_local_server_is_the_usual_local_error() {
        let (e, _, _) = hosted_editor(vec![unreachable()], false);
        let err = e.inference().unwrap().chat(ask()).await.unwrap_err();
        assert!(!err.is_fatal_model_error(), "callers fall back to rules, as for any local outage");
    }

    #[tokio::test]
    async fn switching_to_local_mid_run_sends_nothing_more_to_anthropic() {
        let (e, hosted, _) = hosted_editor(vec![Ok("a".into()), Ok("b".into())], true);
        let batch = e.inference().unwrap();
        let convo = e.inference_conversation().unwrap();
        batch.chat(ask()).await.unwrap();
        convo.chat(ask()).await.unwrap();
        e.set_model_mode(ModelMode::Local, false).unwrap();
        assert_eq!(batch.chat(ask()).await.unwrap().message.content.as_deref(), Some("local"));
        assert!(convo.chat(ask()).await.unwrap_err().to_string().contains("switched to Local"));
        assert_eq!(hosted.models.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn no_key_or_no_consent_is_visible_and_healthy_stays_true() {
        let e = Editor::test_instance();
        let hosted = FakeHosted::new(vec![]);
        let local = Arc::new(FakeLocal { up: false, models: Mutex::new(vec![]) });
        e.set_inference_for_tests(local, hosted.clone());
        // Can't switch on without a key, nor without consent.
        assert_eq!(e.set_model_mode(ModelMode::Hosted, true).unwrap_err().code(), "hosted_no_key");
        e.inner.route.store_key(Some("sk")).unwrap();
        assert_eq!(e.set_model_mode(ModelMode::Hosted, false).unwrap_err().code(), "hosted_not_consented");
        e.set_model_mode(ModelMode::Hosted, true).unwrap();
        // The key is forgotten behind the app's back: the run still starts
        // (healthy) and fails visibly.
        e.inner.route.store_key(None).unwrap();
        let c = e.inference().unwrap();
        assert!(c.healthy().await);
        assert_eq!(c.chat(ask()).await.unwrap_err().code(), "hosted_no_key");
        assert!(hosted.models.lock().unwrap().is_empty());
    }

    struct Count(Mutex<Vec<serde_json::Value>>);
    impl EventSink for Count {
        fn emit(&self, event: &str, payload: serde_json::Value) {
            if event == "model-route" {
                self.0.lock().unwrap().push(payload);
            }
        }
    }

    #[tokio::test]
    async fn the_chip_event_fires_only_when_its_text_changes() {
        let sink = Arc::new(Count(Mutex::new(vec![])));
        let e = Editor::new(
            Box::new(crate::store::SqliteStore::open_in_memory().unwrap()),
            Box::new(crate::adapters::MockVideoEngine),
            Box::new(crate::adapters::MockTranscriber),
            sink.clone(),
            true,
        );
        e.set_secret_store_for_tests(Arc::new(MemoryStore::default()));
        let hosted = FakeHosted::new(vec![Ok("a".into()), Ok("b".into()), unreachable(), Ok("c".into())]);
        e.set_inference_for_tests(Arc::new(FakeLocal { up: true, models: Mutex::new(vec![]) }), hosted);
        e.inner.route.store_key(Some("sk")).unwrap();
        e.set_model_mode(ModelMode::Hosted, true).unwrap(); // 1: Hosted
        e.inference().unwrap().chat(ask()).await.unwrap(); // same: nothing
        e.inference().unwrap().chat(ask()).await.unwrap(); // same: nothing
        e.inference().unwrap().chat(ask()).await.unwrap(); // 2: → Local (offline)
        e.inference().unwrap().chat(ask()).await.unwrap(); // 3: back to Hosted
        let events = sink.0.lock().unwrap();
        let shown: Vec<(String, Option<String>)> = events
            .iter()
            .map(|p| (p["effective"].as_str().unwrap().to_string(), p["reason"].as_str().map(str::to_string)))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("hosted".into(), None),
                ("local".into(), Some("offline".into())),
                ("hosted".into(), None),
            ]
        );
    }

    #[tokio::test]
    async fn forgetting_the_key_switches_to_local() {
        let (e, _, _) = hosted_editor(vec![], true);
        let out = e.set_hosted_key(None).await.unwrap();
        assert_eq!(out.status.mode, ModelMode::Local);
        assert!(!out.status.key_present);
        assert!(e.set_hosted_model("claude-haiku-5-5").is_ok());
        assert!(e.set_hosted_model("gpt-9").is_err());
    }
}
