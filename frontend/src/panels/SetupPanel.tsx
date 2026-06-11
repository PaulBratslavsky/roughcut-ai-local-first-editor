// Setup: one row per capability — media engine, speech model, chat model,
// semantic search — with live status, machine-sized recommendations, and
// checksum-verified downloads. The app ships lean; models arrive on demand.

import { useEffect, useState } from "react";
import {
  downloadWhisperModel,
  downloadGguf,
  getSetupStatus,
  installLlamaServer,
  ollamaPullModel,
  onAppEvent,
  startManagedLlm,
} from "../ipc/api";
import type { ModelTier, ProgressEvent, ProgressTask, SetupStatus } from "../ipc/types";

/** Typo-proof: the compiler rejects a name the core can't emit. */
const PROVISIONING_TASKS: ProgressTask[] = ["model_download", "model_pull", "runtime_install"];

function StatusDot({ ok }: { ok: boolean }) {
  return <span className={`status-dot${ok ? " ok" : ""}`} aria-hidden />;
}

export function SetupPanel({ onClose }: { onClose: () => void }) {
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const [downloading, setDownloading] = useState<ModelTier | null>(null);
  const [llmBusy, setLlmBusy] = useState<string | null>(null);
  const [ggufUrl, setGgufUrl] = useState("");
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    void getSetupStatus().then(setStatus).catch(() => setStatus(null));
  };

  const llmAction = async (kind: string, fn: () => Promise<unknown>) => {
    setLlmBusy(kind);
    setError(null);
    try {
      await fn();
      refresh();
    } catch (err) {
      setError(String((err as { message?: string })?.message ?? err));
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

  const download = async (tier: ModelTier) => {
    setDownloading(tier);
    setError(null);
    try {
      await downloadWhisperModel(tier);
      refresh();
    } catch (err) {
      setError(String((err as { message?: string })?.message ?? err));
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
                      onClick={() => void download(tier.id)}
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

            {/* ---- chat editing ---- */}
            <section className="setup-row">
              <div className="setup-row-head">
                <StatusDot ok={status.inference_reachable} />
                <strong>Chat editing</strong>
                <span className="setup-sub">local LLM — “cut the part where…”</span>
              </div>
              {status.inference_reachable && status.runtime.chat_model_present ? (
                <p className="setup-detail">
                  {status.inference_model} via {status.inference_endpoint}
                </p>
              ) : status.inference_reachable && !status.runtime.chat_model_present ? (
                <div className="setup-detail warn">
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
                <p className="setup-detail warn">
                  Ollama is installed but not running — start it (<code>ollama serve</code> or the
                  menu-bar app). Chat falls back to keyword find-and-cut meanwhile.
                </p>
              ) : (
                <div className="setup-detail warn">
                  No local model server. Either install{" "}
                  <a href="https://ollama.com" target="_blank" rel="noreferrer">Ollama</a> — or use
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
                <span className="setup-sub">transcript embeddings — “find the part about…”</span>
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

            {error && <p className="setup-detail warn">{error}</p>}
            <p className="setup-footer">
              Everything runs on this machine. The only network use is these
              user-triggered model downloads.
            </p>
          </>
        )}
      </div>
    </div>
  );
}
