// Setup: one row per capability — media engine, speech model, the LOCAL /
// HOSTED model switch, chat model, semantic search — with live status,
// machine-sized recommendations, and checksum-verified downloads. The app
// ships lean; models arrive on demand.

import { useEffect, useRef, useState } from "react";
import { callTool, isTauri, logPath, openExternal, revealPath } from "../ipc/api";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import {
  checkHostedKey,
  setHostedKey,
  setHostedModel,
  downloadWhisperModel,
  downloadGguf,
  getSetupStatus,
  installLinkImport,
  installLlamaServer,
  installResolvePlugin,
  removeLinkImport,
  ollamaPullModel,
  onAppEvent,
  startManagedLlm,
} from "../ipc/api";
import type { KeyCheck, ProgressEvent, ProgressTask, SetupStatus, WhisperTier } from "../ipc/types";
import {
  HOSTED_MODEL_CHOICES,
  applyModelStatus,
  chooseModelMode,
  refreshModelStatus,
  useModelStatus,
} from "../state/modelStore";
import { errText } from "./clipUtil";

/** Typo-proof: the compiler rejects a name the core can't emit. */
const PROVISIONING_TASKS: ProgressTask[] = ["model_download", "model_pull", "runtime_install"];

function StatusDot({ ok }: { ok: boolean }) {
  return <span className={`status-dot${ok ? " ok" : ""}`} aria-hidden />;
}

/** A link to one of the app's known sites, opened in the system browser
 *  (the desktop webview doesn't follow target=_blank). */
function ExternalLink({ href, className, children }: { href: string; className?: string; children: React.ReactNode }) {
  return (
    <a
      href={href}
      className={className}
      rel="noreferrer"
      onClick={(e) => {
        e.preventDefault();
        void openExternal(href).catch(() => {});
      }}
    >
      {children}
    </a>
  );
}

type KeyResult = { tone: "ok" | "note" | "error"; text: string };

function keyResult(check: KeyCheck, modelName: string, saved: boolean): KeyResult {
  if (check.ok) {
    const ms = check.latency_ms != null ? ` (${Number(check.latency_ms)} ms)` : "";
    return { tone: "ok", text: `Key works · ${modelName}${ms}` };
  }
  // Saved but unverified: Anthropic was unreachable, not a bad key.
  if (saved) return { tone: "note", text: check.error ?? "saved; couldn't verify while offline" };
  return { tone: "error", text: check.error ?? "the key didn't work" };
}

/** Local / Hosted, the Anthropic API key (Save checks it without spending
 *  tokens; Test; Forget switches to Local), the Claude model, the last
 *  Hosted error, and where to see spend. */
function ModelsRow({ focus, notice }: { focus: boolean; notice: string | null }) {
  const status = useModelStatus();
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState<null | "mode" | "save" | "test" | "forget" | "model">(null);
  const [result, setResult] = useState<KeyResult | null>(null);
  const [error, setError] = useState<string | null>(notice);
  // Forget clears the shown "last error" (the core keeps it until the next run).
  const [dismissedError, setDismissedError] = useState<string | null>(null);
  const rowRef = useRef<HTMLElement | null>(null);
  const keyRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (!focus) return;
    rowRef.current?.scrollIntoView({ block: "nearest" });
    if (!status?.key_present) keyRef.current?.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (!status) return null;
  const hosted = status.mode === "hosted";

  const run = async (kind: NonNullable<typeof busy>, fn: () => Promise<void>) => {
    setBusy(kind);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(null);
    }
  };

  const choose = (mode: "local" | "hosted") =>
    run("mode", async () => {
      const r = await chooseModelMode(mode);
      if (r === "needs_key") {
        setError("Add an Anthropic API key first: paste it below and Save.");
        keyRef.current?.focus();
      }
    });

  const save = () =>
    run("save", async () => {
      setResult(null);
      const r = await setHostedKey(key);
      applyModelStatus(r.status);
      if (r.check) setResult(keyResult(r.check, r.status.hosted_model_name, r.saved));
      if (r.saved) setKey("");
    });

  const test = () =>
    run("test", async () => {
      setResult(null);
      const check = await checkHostedKey();
      setResult(keyResult(check, status.hosted_model_name, false));
      await refreshModelStatus();
    });

  const forget = () =>
    run("forget", async () => {
      const r = await setHostedKey(null);
      applyModelStatus(r.status);
      setDismissedError(r.status.last_error);
      setResult({ tone: "note", text: "Key forgotten. AI steps run on this machine." });
    });

  const pickModel = (model: string) =>
    run("model", async () => {
      applyModelStatus(await setHostedModel(model));
      setResult(null);
    });

  const ok = !hosted || (status.key_present && status.reason !== "key_rejected" && status.reason !== "no_key");

  return (
    <section className="setup-row models-row" ref={rowRef}>
      <div className="setup-row-head">
        <StatusDot ok={ok} />
        <strong>Models</strong>
        <span className="setup-sub">where AI steps run — scoring, outline, chat, clean-up</span>
      </div>
      <div className="models-mode" role="radiogroup" aria-label="Where AI steps run">
        {(["local", "hosted"] as const).map((m) => (
          <button
            key={m}
            role="radio"
            aria-checked={status.mode === m}
            className={`tab${status.mode === m ? " active" : ""}`}
            disabled={!!busy}
            onClick={() => status.mode !== m && void choose(m)}
          >
            {m === "local" ? "Local" : "Hosted"}
          </button>
        ))}
      </div>
      <p className="setup-detail">
        {hosted
          ? `On ${status.hosted_model_name} with your Anthropic API key. Text only (transcripts, chat, the app's prompts and results); video, audio and frames stay on this machine.`
          : "On this machine (Ollama or llama-server below); works offline. Hosted runs them on Claude with your own Anthropic API key."}
      </p>
      <div className="models-key">
        <input
          ref={keyRef}
          className="gguf-input"
          type="password"
          autoComplete="off"
          spellCheck={false}
          placeholder={status.key_present ? "a key is stored — paste a new one to replace it" : "Anthropic API key (sk-ant-…)"}
          aria-label="Anthropic API key"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && key.trim() && !busy) void save();
          }}
        />
        <button className="primary-btn tier-btn" disabled={!!busy || !key.trim()} onClick={() => void save()}>
          {busy === "save" ? "Checking…" : "Save"}
        </button>
        <button className="ghost-btn tier-btn" disabled={!!busy || !status.key_present} onClick={() => void test()}>
          {busy === "test" ? "Testing…" : "Test"}
        </button>
        <button className="ghost-btn tier-btn" disabled={!!busy || !status.key_present} onClick={() => void forget()}>
          Forget
        </button>
      </div>
      {result && (
        <p className={`setup-detail${result.tone === "error" ? " warn" : ""}`} role="status">
          {result.tone === "ok" ? "✓ " : ""}
          {result.text}
        </p>
      )}
      <div className="models-pick">
        <label className="setup-detail" htmlFor="hosted-model">
          Claude model
        </label>
        <select
          id="hosted-model"
          className="models-select"
          value={status.hosted_model}
          disabled={!!busy}
          onChange={(e) => void pickModel(e.target.value)}
        >
          {HOSTED_MODEL_CHOICES.map((m) => (
            <option key={m.id} value={m.id}>
              {m.name}
            </option>
          ))}
        </select>
        <ExternalLink className="models-console" href="https://console.anthropic.com">
          See spend in the Anthropic Console
        </ExternalLink>
      </div>
      {status.last_error && status.last_error !== dismissedError && status.last_error !== result?.text && (
        <p className="setup-detail warn">Last Hosted error: {status.last_error}</p>
      )}
      {error && <p className="setup-detail warn">{error}</p>}
    </section>
  );
}

export function SetupPanel({
  onClose,
  focus,
  notice = null,
}: {
  onClose: () => void;
  /** Open scrolled to this row (the model chip with no key: "models"). */
  focus?: "models";
  /** A message for the Models row (why the chip opened Setup). */
  notice?: string | null;
}) {
  const model = useModelStatus();
  const hosted = model?.mode === "hosted";
  // In Hosted mode a missing local server is a missing fallback, not an error.
  const localWarn = hosted ? "" : " warn";
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const [downloading, setDownloading] = useState<WhisperTier | null>(null);
  const [llmBusy, setLlmBusy] = useState<string | null>(null);
  const [ggufUrl, setGgufUrl] = useState("");
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The app log's file; null in the browser build (the row is hidden).
  const [logFile, setLogFile] = useState<string | null>(null);

  useEffect(() => {
    void logPath().then(setLogFile).catch(() => setLogFile(null));
  }, []);

  const refresh = () => {
    void getSetupStatus().then(setStatus).catch(() => setStatus(null));
    // model-route fires only when where runs go changes; last_error (a bad
    // request, a refusal, a background clean-up failure, or one cleared by
    // a success) needs a re-read. In memory in the core: cheap.
    void refreshModelStatus();
  };

  const llmAction = async (kind: string, fn: () => Promise<unknown>) => {
    setLlmBusy(kind);
    setError(null);
    try {
      await fn();
      refresh();
    } catch (err) {
      setError(errText(err));
    } finally {
      setLlmBusy(null);
      setProgress(null);
    }
  };

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 5000); // picks up installs done outside the app
    const off = onAppEvent<ProgressEvent>("progress", (p) => {
      if (PROVISIONING_TASKS.includes(p.task))
        setProgress(p.fraction >= 1 ? null : p);
    });
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => {
      clearInterval(t);
      off();
      window.removeEventListener("keydown", onKey);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const download = async (tier: WhisperTier) => {
    setDownloading(tier);
    setError(null);
    try {
      await downloadWhisperModel(tier);
      refresh();
    } catch (err) {
      setError(errText(err));
    } finally {
      setDownloading(null);
      setProgress(null);
    }
  };

  return (
    <div className="setup-backdrop" onClick={onClose}>
      <div className="setup-panel card" onClick={(e) => e.stopPropagation()}>
        <div className="setup-header">
          <h2>Setup</h2>
          {status?.ram_gb != null && (
            <span className="setup-ram">This machine: {Math.round(status.ram_gb)} GB RAM</span>
          )}
          <button className="icon-btn" title="Close" onClick={onClose}>✕</button>
        </div>

        {!status ? (
          <p className="setup-hint">Checking your toolchain…</p>
        ) : (
          <>
            {status.demo && (
              <p className="setup-demo-note">
                Demo mode: {status.demo_reason ?? "fixture footage"}
              </p>
            )}

            {/* ---- media engine ---- */}
            <section className="setup-row">
              <div className="setup-row-head">
                <StatusDot ok={status.ffmpeg} />
                <strong>Media engine</strong>
                <span className="setup-sub">ffmpeg — import, waveforms, thumbnails, MP4 export</span>
              </div>
              {status.ffmpeg ? (
                <p className="setup-detail">{status.ffmpeg_path}</p>
              ) : (
                <p className="setup-detail warn">
                  Not found. Install with <code>brew install ffmpeg</code> — picked up
                  automatically, no restart needed.
                </p>
              )}
            </section>

            {/* ---- speech-to-text ---- */}
            <section className="setup-row">
              <div className="setup-row-head">
                <StatusDot ok={status.transcription_ready} />
                <strong>Speech-to-text</strong>
                <span className="setup-sub">
                  whisper {status.whisper_native ? "(built-in engine)" : status.whisper_cli ? "(whisper-cli)" : "(no engine!)"}
                </span>
              </div>
              {status.tiers.map((tier) => (
                <div key={tier.id} className="setup-tier">
                  <span className="tier-name">
                    {tier.id === "accurate" ? "Accurate" : "Compact"}
                    {tier.recommended && <em className="tier-badge">recommended for this machine</em>}
                  </span>
                  <span className="tier-size">{tier.approx_mb} MB</span>
                  {tier.downloaded ? (
                    <span className="tier-done">✓ installed</span>
                  ) : (
                    <button
                      className="primary-btn tier-btn"
                      disabled={downloading !== null}
                      onClick={() => void download(tier.id as WhisperTier)}
                    >
                      {downloading === tier.id ? "Downloading…" : "Download"}
                    </button>
                  )}
                </div>
              ))}
              {downloading && (
                <div className="progress-track setup-progress">
                  <div
                    className="progress-fill"
                    style={{ width: `${Math.round((progress?.fraction ?? 0.02) * 100)}%` }}
                  />
                </div>
              )}
              {downloading && progress && <p className="setup-detail">{progress.message}</p>}
              <p className="setup-detail">
                Downloads are sha256-verified. Stored in {status.models_dir}
              </p>
            </section>

            {/* ---- local or hosted ---- */}
            <ModelsRow focus={focus === "models"} notice={notice} />

            {/* ---- chat editing ---- */}
            <section className="setup-row">
              <div className="setup-row-head">
                <StatusDot ok={status.inference_reachable} />
                <strong>Chat editing</strong>
                <span className="setup-sub">
                  {hosted ? "optional: offline fallback" : "local LLM — “cut the part where…”"}
                </span>
              </div>
              {status.inference_reachable && status.runtime.chat_model_present ? (
                <p className="setup-detail">
                  {status.inference_model} via {status.inference_endpoint}
                </p>
              ) : status.inference_reachable && !status.runtime.chat_model_present ? (
                <div className={`setup-detail${localWarn}`}>
                  Server up, but “{status.inference_model}” isn’t pulled.
                  {status.runtime.ollama_cli && (
                    <button
                      className="primary-btn tier-btn"
                      disabled={!!llmBusy}
                      onClick={() => void llmAction("pull", () => ollamaPullModel(status.inference_model))}
                    >
                      {llmBusy === "pull" ? "Pulling…" : `Pull with Ollama`}
                    </button>
                  )}
                </div>
              ) : status.runtime.ollama_cli ? (
                <p className={`setup-detail${localWarn}`}>
                  Ollama is installed but not running — start it (<code>ollama serve</code> or the
                  menu-bar app).{" "}
                  {hosted
                    ? "Hosted runs fall back to it when Anthropic is unreachable."
                    : "Chat falls back to keyword find-and-cut meanwhile."}
                </p>
              ) : (
                <div className={`setup-detail${localWarn}`}>
                  No local model server. Either install <ExternalLink href="https://ollama.com">Ollama</ExternalLink> — or use
                  the managed runtime:
                  <div className="managed-runtime">
                    {!status.runtime.llama_server_installed ? (
                      <button
                        className="primary-btn tier-btn"
                        disabled={!!llmBusy}
                        onClick={() => void llmAction("runtime", installLlamaServer)}
                      >
                        {llmBusy === "runtime" ? "Installing…" : "Install llama-server (~10 MB)"}
                      </button>
                    ) : (
                      <>
                        <input
                          className="gguf-input"
                          placeholder="https://…/model.gguf (chat model to download)"
                          value={ggufUrl}
                          onChange={(e) => setGgufUrl(e.target.value)}
                        />
                        <button
                          className="primary-btn tier-btn"
                          disabled={!!llmBusy || !ggufUrl}
                          onClick={() => void llmAction("gguf", () => downloadGguf(ggufUrl))}
                        >
                          {llmBusy === "gguf" ? "Downloading…" : "Download model"}
                        </button>
                        {status.runtime.ggufs.length > 0 && (
                          <button
                            className="primary-btn tier-btn"
                            disabled={!!llmBusy}
                            onClick={() => void llmAction("start", () => startManagedLlm(status.runtime.ggufs[0]!))}
                          >
                            {llmBusy === "start" ? "Starting…" : "Start local server"}
                          </button>
                        )}
                      </>
                    )}
                  </div>
                </div>
              )}
              {llmBusy && progress && (
                <>
                  <div className="progress-track setup-progress">
                    <div className="progress-fill" style={{ width: `${Math.round(progress.fraction * 100)}%` }} />
                  </div>
                  <p className="setup-detail">{progress.message}</p>
                </>
              )}
            </section>

            {/* ---- semantic search ---- */}
            <section className="setup-row">
              <div className="setup-row-head">
                <StatusDot ok={status.inference_reachable} />
                <strong>Semantic search</strong>
                <span className="setup-sub">
                  {hosted
                    ? "optional: without it, search uses keywords"
                    : "transcript embeddings — “find the part about…”"}
                </span>
              </div>
              <p className="setup-detail">
                {status.inference_reachable && status.runtime.embedding_model_present ? (
                  <>{status.embedding_model} (same local server). Indexes build automatically after transcription.</>
                ) : status.inference_reachable && status.runtime.ollama_cli ? (
                  <>
                    “{status.embedding_model}” isn’t pulled.
                    <button
                      className="primary-btn tier-btn"
                      disabled={!!llmBusy}
                      onClick={() => void llmAction("pull-embed", () => ollamaPullModel(status.embedding_model))}
                    >
                      {llmBusy === "pull-embed" ? "Pulling…" : "Pull with Ollama"}
                    </button>
                  </>
                ) : (
                  <>Needs the local server above. BM25 keyword search works without it.</>
                )}
              </p>
            </section>

            {/* ---- import from a link (ADR-0009) ---- */}
            <section className="setup-row">
              <div className="setup-row-head">
                <StatusDot ok={status.link_import.installed && !status.link_import.outdated} />
                <strong>Import from a link</strong>
                <span className="setup-sub">download a video from YouTube or another site with yt-dlp</span>
              </div>
              {!status.link_import.supported ? (
                <p className="setup-detail">yt-dlp and Deno have no builds for this machine.</p>
              ) : status.link_import.installed ? (
                <div className={`setup-detail${status.link_import.outdated ? " warn" : ""}`}>
                  On: yt-dlp {status.link_import.ytdlp ?? "?"}, Deno {status.link_import.deno ?? "not found"}
                  {status.link_import.outdated && " (yt-dlp may be out of date: sites change, so update it)"}.
                  You and any client you connect (Claude Code, scripts using <code>roughcut</code>) can import
                  links.
                  <button
                    className="primary-btn tier-btn"
                    disabled={!!llmBusy}
                    onClick={() => void llmAction("links", installLinkImport)}
                  >
                    {llmBusy === "links" ? "Updating…" : "Update"}
                  </button>
                  <button
                    className="ghost-btn"
                    disabled={!!llmBusy}
                    onClick={() => void llmAction("links-off", removeLinkImport)}
                  >
                    Remove
                  </button>
                </div>
              ) : (
                <div className="setup-detail">
                  Off. Install downloads yt-dlp and Deno (about 80 MB) from their GitHub releases, checks each
                  against its published SHA-256, and turns link import on, for you and for any client you
                  connect (Claude Code, scripts using <code>roughcut</code>).
                  <button
                    className="primary-btn tier-btn"
                    disabled={!!llmBusy}
                    onClick={() => void llmAction("links", installLinkImport)}
                  >
                    {llmBusy === "links" ? "Installing…" : "Install"}
                  </button>
                </div>
              )}
              <p className="setup-detail">
                Saved to <code>{status.link_import.download_dir}</code>
                {isTauri && (
                  <button
                    className="ghost-btn"
                    disabled={!!llmBusy}
                    onClick={() =>
                      void llmAction("links-dir", async () => {
                        const picked = await openFolderDialog({ directory: true, multiple: false });
                        if (typeof picked !== "string") return;
                        await callTool("set_preferences", { preferences: { download_dir: picked } });
                      })
                    }
                  >
                    Choose…
                  </button>
                )}
              </p>
            </section>

            {/* ---- DaVinci Resolve hand-off ---- */}
            <section className="setup-row">
              <div className="setup-row-head">
                <StatusDot ok={status.resolve.app_installed && status.resolve.plugin_installed && !status.resolve.plugin_outdated} />
                <strong>DaVinci Resolve</strong>
                <span className="setup-sub">one-click rough draft from Resolve's Scripts menu</span>
              </div>
              {!status.resolve.app_installed ? (
                <p className="setup-detail">
                  Resolve not detected. Install{" "}
                  <ExternalLink href="https://www.blackmagicdesign.com/products/davinciresolve">
                    DaVinci Resolve
                  </ExternalLink>{" "}
                  (the free version works) and this lights up.
                </p>
              ) : status.resolve.plugin_installed && !status.resolve.plugin_outdated ? (
                <p className="setup-detail">
                  Plugins installed. In Resolve (Workspace ▸ Scripts ▸ Utility): “RoughCut AI
                  Draft” makes a fresh rough cut from the selected media-pool clip; “RoughCut
                  Import Cut” pulls your current RoughCut edit into Resolve — works on the free
                  version.
                </p>
              ) : (
                <div className="setup-detail warn">
                  {status.resolve.plugin_outdated
                    ? "An older plugin version is installed."
                    : "Resolve found — the RoughCut script isn't installed yet."}
                  <button
                    className="primary-btn tier-btn"
                    disabled={!!llmBusy}
                    onClick={() => void llmAction("resolve", installResolvePlugin)}
                  >
                    {llmBusy === "resolve"
                      ? "Installing…"
                      : status.resolve.plugin_outdated
                        ? "Update plugin"
                        : "Install plugin"}
                  </button>
                </div>
              )}
            </section>

            {/* ---- app log ---- */}
            {logFile && (
              <section className="setup-row">
                <div className="setup-row-head">
                  <StatusDot ok />
                  <strong>Logs</strong>
                  <span className="setup-sub">AI steps, fallbacks and ffmpeg errors — stays on this Mac</span>
                </div>
                <p className="setup-detail">
                  <code>{logFile}</code>
                  <button className="primary-btn tier-btn" onClick={() => void revealPath(logFile)}>
                    Reveal logs
                  </button>
                </p>
              </section>
            )}

            {error && <p className="setup-detail warn">{error}</p>}
            <p className="setup-footer">
              {hosted
                ? `Hosted: AI steps send text to Anthropic (${model?.hosted_model_name ?? "Claude"}) — transcripts, your chat, and the app's prompts and results. Video, audio, frames, transcription and search stay on this machine. Other network use: these user-triggered model downloads.`
                : "Everything runs on this machine. The only network use is these user-triggered model downloads, the API-key check (Save / Test), and link imports once you install yt-dlp."}
            </p>
          </>
        )}
      </div>
    </div>
  );
}
