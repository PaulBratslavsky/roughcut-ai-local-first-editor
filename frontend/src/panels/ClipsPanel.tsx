// Clips tab: suggested moments that stand alone as Shorts / Reels / TikToks,
// and the clips you saved — from a suggestion or from pieces picked in the
// transcript ("Add to clip"). Each clip is 16:9 or 9:16, is its own timeline
// over the same source (the current cut inside its pieces), and survives
// later main-cut edits.

import { useEffect, useRef, useState } from "react";
import { onAppEvent, revealPath } from "../ipc/api";
import {
  useClipCandidates,
  useClips,
  useCreateClip,
  useDeleteClip,
  useExportClip,
  useFindClips,
  useProject,
  useTranscript,
  useUpdateClip,
} from "../ipc/queries";
import type { ClipCandidate, ClipFormat, ExportTarget, ProgressEvent, SavedClipSummary } from "../ipc/types";
import { previewRanges, seekTo } from "../state/viewStore";
import { EXPORT_TARGETS } from "./ExportMenu";

const LENGTHS: { label: string; min_s: number; max_s: number }[] = [
  { label: "15–30s", min_s: 15, max_s: 30 },
  { label: "15–60s", min_s: 15, max_s: 60 },
  { label: "30–90s", min_s: 30, max_s: 90 },
];

export const FORMAT_LABEL: Record<ClipFormat, string> = { landscape: "16:9", vertical: "9:16" };

export function clock(s: number): string {
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
}

export function errText(e: unknown): string {
  const o = e as { error?: { message?: string }; message?: string };
  return o?.error?.message ?? o?.message ?? String(e);
}

/** Room the export menu needs below its button (8 targets). */
const MENU_HEIGHT_PX = 280;

function slugify(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 48) || "clip";
}

export function ClipsPanel({ projectId }: { projectId: string }) {
  const { data: transcript } = useTranscript(projectId);
  const { data: project } = useProject(projectId);
  const { data: candidates = [] } = useClipCandidates(projectId);
  const { data: clips = [] } = useClips(projectId);
  const find = useFindClips();
  const create = useCreateClip();
  const [lengthIdx, setLengthIdx] = useState(1);
  const [theme, setTheme] = useState("");
  const [method, setMethod] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const elapsed = useElapsed(find.isPending);
  const [progress, setProgress] = useState<string | null>(null);

  // The core narrates a find (outline → scoring batches); a long local-model
  // run should say where it is, not just spin.
  useEffect(
    () =>
      onAppEvent<ProgressEvent>("progress", (p) => {
        if (p.task === "find_clips" && p.project_id === projectId) setProgress(p.message);
      }),
    [projectId],
  );

  const hasSpeech = !!transcript?.segments.some((s) => !s.is_silence);

  const runFind = () => {
    setError(null);
    setProgress(null);
    const len = LENGTHS[lengthIdx];
    find.mutate(
      {
        project_id: projectId,
        count: 8,
        min_s: len.min_s,
        max_s: len.max_s,
        theme: theme.trim() || undefined,
      },
      {
        onSuccess: (r) => setMethod(r.method),
        onError: (e) => setError(errText(e)),
      },
    );
  };

  const saveCandidate = (c: ClipCandidate, format: ClipFormat) => {
    setError(null);
    create.mutate(
      { project_id: projectId, candidate_id: c.id, format },
      { onError: (e) => setError(errText(e)) },
    );
  };

  if (!hasSpeech) {
    return (
      <div className="clips-panel">
        <p className="clips-note">Transcribe the video first — clips are found from what you say.</p>
      </div>
    );
  }

  const shownMethod = method ?? candidates[0]?.method ?? null;

  return (
    <div className="clips-panel">
      <p className="clips-note">
        Save clips from the transcript (select text → right-click → Add to clip), or let the
        finder suggest moments that stand alone as Shorts / Reels / TikToks. Each clip keeps your
        cut's removed silences and fillers.
      </p>

      <input
        className="clips-theme"
        value={theme}
        onChange={(e) => setTheme(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !find.isPending) runFind();
        }}
        placeholder="about… (optional) — e.g. AI tools, pricing, my guitar plugin"
        aria-label="Only find clips about this topic"
        disabled={find.isPending}
      />
      <div className="clips-find">
        <div className="clips-lengths" role="radiogroup" aria-label="Clip length">
          {LENGTHS.map((l, i) => (
            <button
              key={l.label}
              role="radio"
              aria-checked={i === lengthIdx}
              className={`tab${i === lengthIdx ? " active" : ""}`}
              onClick={() => setLengthIdx(i)}
              disabled={find.isPending}
            >
              {l.label}
            </button>
          ))}
        </div>
        <button className="ghost-btn" onClick={runFind} disabled={find.isPending}>
          {find.isPending ? `finding… ${elapsed}s` : candidates.length ? "▸ find again" : "▸ find clips"}
        </button>
      </div>
      {find.isPending && (
        <p className="clips-note">
          {progress ?? "Reading the transcript…"} Scoring on-device can take a minute or two.
        </p>
      )}
      {error && <p className="empty-error">{error}</p>}

      {clips.length > 0 && (
        <>
          <div className="clips-label">saved clips ({clips.length})</div>
          <div className="clips-list">
            {clips.map((c) => (
              <SavedClipRow key={c.id} projectId={projectId} projectName={project?.name ?? "project"} clip={c} />
            ))}
          </div>
        </>
      )}

      {candidates.length > 0 && (
        <>
          <div className="clips-label">
            <span>
              suggestions
              {candidates[0]?.theme && <span className="clips-method"> · about “{candidates[0].theme}”</span>}
            </span>
            {shownMethod && (
              <span className="clips-method">
                {shownMethod === "llm" ? "scored by the on-device model" : "heuristic — no model reachable"}
              </span>
            )}
          </div>
          <div className="clips-list">
            {candidates.map((c) => (
              <div key={c.id} className="clip-row">
                <span className="clip-score" title="short-form potential, 0–100">
                  {Math.round(c.score)}
                </span>
                <div className="clip-body">
                  <button className="clip-title" onClick={() => seekTo(c.start)} title="Jump to this moment">
                    {c.title}
                  </button>
                  <span className="clip-meta">
                    {clock(c.start)}–{clock(c.end)} · {Math.round(c.duration)}s
                  </span>
                  <span className="clip-hook">“{c.hook}”</span>
                  <span className="clip-reason">{c.reason}</span>
                </div>
                <div className="clip-actions">
                  <button
                    className="ghost-btn"
                    onClick={() => previewRanges([{ start: c.start, end: c.end }])}
                    title="Play this moment"
                  >
                    ▶
                  </button>
                  <button
                    className="ghost-btn"
                    onClick={() => saveCandidate(c, "vertical")}
                    disabled={create.isPending}
                    title="Save as a 9:16 short"
                  >
                    + 9:16
                  </button>
                  <button
                    className="ghost-btn"
                    onClick={() => saveCandidate(c, "landscape")}
                    disabled={create.isPending}
                    title="Save as a 16:9 clip"
                  >
                    + 16:9
                  </button>
                </div>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

function SavedClipRow({
  projectId,
  projectName,
  clip,
}: {
  projectId: string;
  projectName: string;
  clip: SavedClipSummary;
}) {
  const update = useUpdateClip();
  const del = useDeleteClip();
  const exportClip = useExportClip();
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(clip.title);
  const [menuOpen, setMenuOpen] = useState(false);
  // The panel scrolls (clips overflow), so near its bottom the menu opens up.
  const [menuUp, setMenuUp] = useState(false);
  const [status, setStatus] = useState<{ text: string; path?: string } | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const onDocClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [menuOpen]);

  const saveTitle = () => {
    setEditing(false);
    const t = title.trim();
    if (t && t !== clip.title) update.mutate({ project_id: projectId, clip_id: clip.id, title: t });
    else setTitle(clip.title);
  };

  const toggleFormat = () =>
    update.mutate({
      project_id: projectId,
      clip_id: clip.id,
      format: clip.format === "vertical" ? "landscape" : "vertical",
    });

  const runExport = (target: ExportTarget, ext: string, label: string) => {
    setMenuOpen(false);
    setStatus({
      text: target === "mp4" && clip.format === "vertical" ? "rendering 9:16…" : `exporting ${label}…`,
    });
    exportClip.mutate(
      {
        project_id: projectId,
        clip_id: clip.id,
        target,
        out_path: `~/Downloads/${slugify(projectName)}-${slugify(clip.title)}.${ext}`,
      },
      {
        onSuccess: (r) => {
          setStatus({ text: `saved ${r.path.split("/").pop()}`, path: r.path });
          void revealPath(r.path);
        },
        onError: (e) => setStatus({ text: `export failed: ${errText(e)}` }),
      },
    );
  };

  const pieces = clip.ranges.length;

  return (
    <div className="clip-row">
      <button
        className={`clip-format${clip.format === "vertical" ? " vertical" : ""}`}
        onClick={toggleFormat}
        disabled={update.isPending}
        title={`${clip.format === "vertical" ? "9:16 short" : "16:9"} — click to switch`}
      >
        {FORMAT_LABEL[clip.format]}
      </button>
      <div className="clip-body">
        {editing ? (
          <input
            className="clip-title-input"
            value={title}
            autoFocus
            onChange={(e) => setTitle(e.target.value)}
            onBlur={saveTitle}
            onKeyDown={(e) => {
              if (e.key === "Enter") saveTitle();
              if (e.key === "Escape") {
                setTitle(clip.title);
                setEditing(false);
              }
            }}
          />
        ) : (
          <button
            className="clip-title"
            onClick={() => seekTo(clip.start)}
            onDoubleClick={() => setEditing(true)}
            title="Jump to the clip · double-click to rename"
          >
            {clip.title}
          </button>
        )}
        <span className="clip-meta">
          {pieces > 1 ? `${pieces} pieces · ` : `${clock(clip.start)}–${clock(clip.end)} · `}
          {Math.round(clip.duration)}s
          {clip.score !== null && ` · score ${Math.round(clip.score)}`}
        </span>
        {status && (
          <span className="clip-reason">
            {status.text}
            {status.path && (
              <button className="reveal-btn" onClick={() => void revealPath(status.path!)}>
                reveal
              </button>
            )}
          </span>
        )}
      </div>
      <div className="clip-actions" ref={menuRef}>
        <button className="ghost-btn" onClick={() => previewRanges(clip.ranges)} title="Play the clip">
          ▶
        </button>
        <button
          className="ghost-btn"
          onClick={() => {
            const row = menuRef.current?.getBoundingClientRect();
            const panel = menuRef.current?.closest(".clips-panel")?.getBoundingClientRect();
            setMenuUp(!!row && !!panel && row.bottom + MENU_HEIGHT_PX > panel.bottom);
            setMenuOpen((v) => !v);
          }}
          disabled={exportClip.isPending}
        >
          export
        </button>
        <button
          className="ghost-btn clip-delete"
          onClick={() => del.mutate({ project_id: projectId, clip_id: clip.id })}
          disabled={del.isPending}
          title="Delete this clip"
        >
          ×
        </button>
        {menuOpen && (
          <div className={`export-dropdown${menuUp ? " up" : ""}`}>
            {EXPORT_TARGETS.map((t) => (
              <button key={t.target} className="export-item" onClick={() => runExport(t.target, t.ext, t.label)}>
                {t.target === "mp4" ? `MP4 (${FORMAT_LABEL[clip.format]})` : t.label}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

/** Whole seconds since `running` turned true (0 when idle). */
function useElapsed(running: boolean): number {
  const [s, setS] = useState(0);
  useEffect(() => {
    if (!running) {
      setS(0);
      return;
    }
    const started = Date.now();
    const id = setInterval(() => setS(Math.floor((Date.now() - started) / 1000)), 1000);
    return () => clearInterval(id);
  }, [running]);
  return s;
}
