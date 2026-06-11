// First-run flow: drop a video (or pick one), then
// create_project -> import_media -> transcribe with a progress bar.

import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { downloadWhisperModel, getSetupStatus, isTauri, onAppEvent } from "./ipc/api";
import { ingestFile } from "./ingest";
import { setScreen } from "./state/viewStore";
import { useProjects, useRestoreProject } from "./ipc/queries";
import type { WhisperTier, ProgressEvent, SetupStatus } from "./ipc/types";

/** The empty state is also where you land after trashing your LAST project —
 *  the trash must be reachable from here, not just the project switcher. */
function TrashList() {
  const projects = useProjects();
  const restore = useRestoreProject();
  const trash = projects.data?.trash ?? [];
  if (trash.length === 0) return null;
  return (
    <div className="empty-trash">
      <span className="empty-trash-label">trash</span>
      {trash.map((p) => (
        <button
          key={p.id}
          className="empty-trash-item"
          disabled={restore.isPending}
          onClick={() => restore.mutate({ project_id: p.id })}
          title="Restore this project"
        >
          ↩ {p.name}
        </button>
      ))}
    </div>
  );
}

type Phase = "idle" | "working";

/** First-run checklist: shown until ffmpeg is found and (when the native
 *  whisper engine is compiled in) a speech model is downloaded. */
function SetupCard({ status, onChanged }: { status: SetupStatus; onChanged: () => void }) {
  const [downloading, setDownloading] = useState<WhisperTier | null>(null);
  const [progress, setProgress] = useState(0);
  const [message, setMessage] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    return onAppEvent<ProgressEvent>("progress", (p) => {
      if (p.task === "model_download") {
        setProgress(p.fraction);
        setMessage(p.message);
      }
    });
  }, []);

  const download = async (tier: WhisperTier) => {
    setError(null);
    setDownloading(tier);
    setProgress(0);
    try {
      await downloadWhisperModel(tier);
      onChanged();
    } catch (err) {
      setError(String((err as { message?: string })?.message ?? err));
    } finally {
      setDownloading(null);
    }
  };

  const needsModel = status.whisper_native && !status.whisper_model;
  const modelName = status.whisper_model?.split(/[\\/]/).pop();

  return (
    <div className="setup-card">
      <h3>Set up local AI</h3>
      <p className="setup-note">One-time setup. Everything runs on this machine.</p>
      <div className="setup-row">
        <span className={`setup-dot${status.ffmpeg ? " ok" : ""}`} />
        <div className="setup-row-body">
          <strong>Video toolchain (ffmpeg)</strong>
          {status.ffmpeg ? (
            <span className="setup-detail">{status.ffmpeg_path}</span>
          ) : (
            <span className="setup-detail">
              Not found — install with <code>brew install ffmpeg</code>, then{" "}
              <button className="link-btn" onClick={onChanged}>re-check</button>
            </span>
          )}
        </div>
      </div>
      <div className="setup-row">
        <span className={`setup-dot${!needsModel ? " ok" : ""}`} />
        <div className="setup-row-body">
          <strong>Speech model (whisper)</strong>
          {!needsModel ? (
            <span className="setup-detail">{modelName ?? "ready"}</span>
          ) : downloading ? (
            <>
              <div className="progress-track">
                <div className="progress-fill" style={{ width: `${Math.round(progress * 100)}%` }} />
              </div>
              <span className="setup-detail">{message || "downloading…"}</span>
            </>
          ) : (
            <span className="setup-actions">
              <button className="primary-btn" onClick={() => void download("accurate")}>
                Best quality · 550 MB
              </button>
              <button className="ghost-btn" onClick={() => void download("compact")}>
                Smaller · 190 MB
              </button>
            </span>
          )}
        </div>
      </div>
      {error && <p className="empty-error">{error}</p>}
    </div>
  );
}

function baseName(path: string): string {
  const last = path.split(/[\\/]/).pop() ?? path;
  return last.replace(/\.[^.]+$/, "") || "Untitled project";
}

export function EmptyState() {
  const [phase, setPhase] = useState<Phase>("idle");
  const [progress, setProgress] = useState(0);
  const [message, setMessage] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [dragOver, setDragOver] = useState(false);
  const [setup, setSetup] = useState<SetupStatus | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const workingRef = useRef(false);
  const queryClient = useQueryClient();

  const refreshSetup = useCallback(() => {
    void getSetupStatus().then(setSetup).catch(() => setSetup(null));
  }, []);
  useEffect(refreshSetup, [refreshSetup]);

  useEffect(() => {
    return onAppEvent<ProgressEvent>("progress", (p) => {
      if (p.task === "transcribe") {
        setProgress(p.fraction);
        setMessage(p.message);
      }
    });
  }, []);

  const setupIncomplete =
    isTauri && !!setup && (!setup.ffmpeg || (setup.whisper_native && !setup.whisper_model));

  const start = async (name: string, filePath: string) => {
    if (workingRef.current) return;
    workingRef.current = true;
    setError(null);
    setPhase("working");
    setProgress(0);
    setMessage("Creating project…");
    try {
      await ingestFile(name, filePath, { onPhase: setMessage });
      await queryClient.invalidateQueries();
    } catch (err) {
      setError(String((err as { message?: string })?.message ?? err));
      setPhase("idle");
    } finally {
      workingRef.current = false;
    }
  };

  // Tauri native file drop carries real paths.
  useEffect(() => {
    if (!isTauri) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<{ paths: string[] }>("tauri://drag-drop", (e) => {
      const path = e.payload.paths?.[0];
      if (path) void start(baseName(path), path);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onBrowserDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    if (isTauri) return; // handled by the native drag-drop event
    const file = e.dataTransfer.files?.[0];
    if (file) void start(baseName(file.name), file.name);
  };

  const onPick = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (file) void start(baseName(file.name), file.name);
    e.target.value = "";
  };

  // In Tauri the HTML file input only exposes a bare file NAME; the native
  // dialog returns the real absolute path the asset protocol can play.
  const choose = async () => {
    if (!isTauri) {
      fileInputRef.current?.click();
      return;
    }
    const path = await openFileDialog({
      multiple: false,
      directory: false,
      filters: [{ name: "Video", extensions: ["mp4", "mov", "mkv", "avi", "m4v", "webm"] }],
    });
    if (typeof path === "string") void start(baseName(path), path);
  };

  return (
    <div className="empty-state">
      {phase === "idle" ? (
        <>
          {setupIncomplete && setup && <SetupCard status={setup} onChanged={refreshSetup} />}
          <div
            className={`drop-zone${dragOver ? " over" : ""}`}
            onClick={() => void choose()}
            onDragOver={(e) => {
              e.preventDefault();
              setDragOver(true);
            }}
            onDragLeave={() => setDragOver(false)}
            onDrop={onBrowserDrop}
          >
            <svg width="42" height="42" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.4">
              <rect x="2" y="4" width="20" height="16" rx="2.5" />
              <path d="M10.5 9.5v5l4-2.5z" fill="currentColor" stroke="none" />
            </svg>
            <h2>Drop a video or click to choose</h2>
            <p>Your footage never leaves this machine.</p>
            <input
              ref={fileInputRef}
              type="file"
              accept="video/*"
              style={{ display: "none" }}
              onChange={onPick}
            />
          </div>
          {isTauri ? (
            <button className="ghost-btn demo-btn" onClick={() => setScreen("recorder")}>
              ● or record something new
            </button>
          ) : (
            <button
              className="primary-btn demo-btn"
              onClick={() => void start("i-quit-rough-draft", "/demo/talking-head.mp4")}
            >
              Use demo footage
            </button>
          )}
          {error && <p className="empty-error">{error}</p>}
          <TrashList />
        </>
      ) : (
        <div className="import-progress">
          <h2>Preparing your project</h2>
          <div className="progress-track">
            <div className="progress-fill" style={{ width: `${Math.round(progress * 100)}%` }} />
          </div>
          <p className="progress-message">{message}</p>
        </div>
      )}
    </div>
  );
}
