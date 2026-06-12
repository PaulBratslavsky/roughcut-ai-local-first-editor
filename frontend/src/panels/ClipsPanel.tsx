// Clips tab: extend the CURRENT project with more footage — recordings from
// the library or any video file — appended to the end of the source.
// Existing cuts keep their timestamps; the appended span gets transcribed in.

import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { callTool, isTauri, listRecordings, type RecordingFile } from "../ipc/api";
import { setScreen } from "../state/viewStore";

function fmtDur(s: number): string {
  if (s <= 0) return "";
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
}

export function ClipsPanel({ projectId }: { projectId: string }) {
  const [files, setFiles] = useState<RecordingFile[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const queryClient = useQueryClient();

  useEffect(() => {
    void listRecordings().then(setFiles).catch(() => setFiles([]));
  }, []);

  const append = async (label: string, path: string) => {
    setBusy(path);
    setError(null);
    try {
      await callTool("append_media", { project_id: projectId, file_path: path });
      // Caption the appended span; cuts keep their timestamps.
      await callTool("transcribe", { project_id: projectId });
      await queryClient.invalidateQueries();
    } catch (e) {
      setError(`${label}: ${((x: unknown) => {
        const o = x as { error?: { message?: string }; message?: string };
        return o?.error?.message ?? o?.message ?? String(x);
      })(e)}`);
    } finally {
      setBusy(null);
    }
  };

  const browse = async () => {
    const path = await openFileDialog({
      multiple: false,
      directory: false,
      filters: [{ name: "Video", extensions: ["mp4", "mov", "m4v"] }],
    });
    if (typeof path === "string") {
      const name = path.split("/").pop() ?? path;
      void append(name, path);
    }
  };

  if (!isTauri) {
    return <p className="clips-note">Adding footage needs the desktop app.</p>;
  }

  return (
    <div className="clips-panel">
      <p className="clips-note">
        Add footage to the END of this video. Your cuts stay put; the new part
        gets transcribed. Same codec &amp; resolution required (recordings made
        here always match each other).
      </p>
      <div className="clips-actions">
        <button className="ghost-btn" onClick={() => void browse()} disabled={!!busy}>
          ▸ add a video file…
        </button>
        <button className="ghost-btn" onClick={() => setScreen("recorder")} disabled={!!busy}>
          ● record a new clip
        </button>
      </div>
      {files.length > 0 && (
        <>
          <div className="clips-lib-label">recordings</div>
          <div className="rec-library-list">
            {files.map((f) => (
              <div key={f.path} className="rec-library-row">
                <span className="rec-library-name">{f.name}</span>
                <span className="rec-library-meta">
                  {fmtDur(f.duration_s)} · {f.size_mb.toFixed(0)} MB
                </span>
                <button
                  className="ghost-btn"
                  disabled={!!busy}
                  onClick={() => void append(f.name, f.path)}
                >
                  {busy === f.path ? "adding…" : "+ add"}
                </button>
              </div>
            ))}
          </div>
        </>
      )}
      {error && <p className="empty-error">{error}</p>}
    </div>
  );
}
