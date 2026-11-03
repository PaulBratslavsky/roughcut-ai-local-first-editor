// The clip being assembled in the transcript: pieces added with right-click
// → "Add to clip" (highlighted in the text), shown here with their playable
// length. Audition them back to back, pick 16:9 or 9:16, save — the clip
// lands in the Clips tab and the draft clears.

import { useState } from "react";
import { useStore } from "@tanstack/react-store";
import { useCreateClip, useTimeline } from "../ipc/queries";
import type { ClipFormat } from "../ipc/types";
import { clearClipDraft, previewRanges, setActiveTab, viewStore } from "../state/viewStore";
import { errText, FORMAT_LABEL } from "./clipUtil";

export function ClipDraftBar({ projectId }: { projectId: string }) {
  const draft = useStore(viewStore, (s) => s.clipDraft);
  const { data: timeline } = useTimeline(projectId);
  const create = useCreateClip();
  const [title, setTitle] = useState("");
  const [format, setFormat] = useState<ClipFormat>("vertical");
  const [saved, setSaved] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (draft.length === 0) {
    if (!saved) return null;
    return (
      <div className="clip-draft">
        <span className="clip-draft-summary">saved “{saved}”</span>
        <button className="ghost-btn" onClick={() => setActiveTab("clips")}>
          open clips
        </button>
        <button className="ghost-btn" onClick={() => setSaved(null)} title="Dismiss">
          ×
        </button>
      </div>
    );
  }

  // What will actually play: the pieces minus anything the main cut removed.
  const playable = draft.reduce((sum, r) => {
    const kept = (timeline?.clips ?? [])
      .filter((c) => c.included)
      .reduce((s, c) => s + Math.max(0, Math.min(c.source_out, r.end) - Math.max(c.source_in, r.start)), 0);
    return sum + (timeline ? kept : r.end - r.start);
  }, 0);

  const save = () => {
    setError(null);
    create.mutate(
      { project_id: projectId, ranges: draft, format, title: title.trim() || undefined },
      {
        onSuccess: (r) => {
          setSaved(r.clip.title);
          setTitle("");
          clearClipDraft();
        },
        onError: (e) => setError(errText(e)),
      },
    );
  };

  return (
    <div className="clip-draft">
      <span className="clip-draft-summary" title="Right-click more text → Add to clip to extend it">
        clip · {draft.length} piece{draft.length === 1 ? "" : "s"} · {Math.round(playable)}s
      </span>
      <input
        className="clip-draft-title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !create.isPending) save();
        }}
        placeholder="title"
        aria-label="Clip title"
      />
      <div className="clip-draft-formats" role="radiogroup" aria-label="Clip format">
        {(["vertical", "landscape"] as const).map((f) => (
          <button
            key={f}
            role="radio"
            aria-checked={format === f}
            className={`tab${format === f ? " active" : ""}`}
            onClick={() => setFormat(f)}
            title={f === "vertical" ? "9:16 short" : "16:9, the source frame"}
          >
            {FORMAT_LABEL[f]}
          </button>
        ))}
      </div>
      <button className="ghost-btn" onClick={() => previewRanges(draft)} title="Play the pieces back to back">
        ▶
      </button>
      <button className="ghost-btn" onClick={save} disabled={create.isPending}>
        {create.isPending ? "saving…" : "save clip"}
      </button>
      <button className="ghost-btn" onClick={clearClipDraft} title="Discard the draft" disabled={create.isPending}>
        ×
      </button>
      {error && <span className="empty-error">{error}</span>}
    </div>
  );
}
