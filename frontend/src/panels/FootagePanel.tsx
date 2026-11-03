// Footage tab: extend the CURRENT project with another video file, appended
// to the end of the source. Existing cuts keep their timestamps; the
// appended span gets transcribed in.

import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { callTool, isTauri } from "../ipc/api";

export function FootagePanel({ projectId }: { projectId: string }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const queryClient = useQueryClient();

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
      filters: [{ name: "Video", extensions: ["mp4", "mov", "m4v", "mkv"] }],
    });
    if (typeof path === "string") {
      const name = path.split("/").pop() ?? path;
      void append(name, path);
    }
  };

  if (!isTauri) {
    return <p className="footage-note">Adding footage needs the desktop app.</p>;
  }

  return (
    <div className="footage-panel">
      <p className="footage-note">
        Add footage to the END of this video. Your cuts stay put; the new part
        gets transcribed. Same codec &amp; resolution required (two recordings
        made with the same OBS settings match).
      </p>
      <div className="footage-actions">
        <button className="ghost-btn" onClick={() => void browse()} disabled={!!busy}>
          {busy ? "adding…" : "▸ add a video file…"}
        </button>
      </div>
      {error && <p className="empty-error">{error}</p>}
    </div>
  );
}
