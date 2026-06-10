// Setup: one row per capability — media engine, speech model, chat model,
// semantic search — with live status, machine-sized recommendations, and
// checksum-verified downloads. The app ships lean; models arrive on demand.

import { useEffect, useState } from "react";
import { downloadWhisperModel, getSetupStatus, onAppEvent } from "../ipc/api";
import type { ModelTier, ProgressEvent, SetupStatus } from "../ipc/types";

function StatusDot({ ok }: { ok: boolean }) {
  return <span className={`status-dot${ok ? " ok" : ""}`} aria-hidden />;
}

export function SetupPanel({ onClose }: { onClose: () => void }) {
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const [downloading, setDownloading] = useState<ModelTier | null>(null);
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    void getSetupStatus().then(setStatus).catch(() => setStatus(null));
  };

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 5000); // picks up installs done outside the app
    const off = onAppEvent<ProgressEvent>("progress", (p) => {
      if (p.task === "model_download") setProgress(p.fraction >= 1 ? null : p);
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
              {status.inference_reachable ? (
                <p className="setup-detail">
                  {status.inference_model} via {status.inference_endpoint}
                </p>
              ) : (
                <p className="setup-detail warn">
                  No local model server. Install <a href="https://ollama.com" target="_blank" rel="noreferrer">Ollama</a>,
                  then <code>ollama pull {status.inference_model}</code>. Chat falls back to
                  keyword find-and-cut meanwhile.
                </p>
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
                {status.inference_reachable ? (
                  <>{status.embedding_model} (same local server). Indexes build automatically after transcription.</>
                ) : (
                  <>Needs the local server above, plus <code>ollama pull {status.embedding_model}</code>. BM25 keyword search works without it.</>
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
