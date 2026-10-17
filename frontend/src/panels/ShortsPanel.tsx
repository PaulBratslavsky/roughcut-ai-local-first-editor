// Shorts tab: find the moments of a long-form video that stand alone as
// Shorts / Reels / TikToks, audition them, turn the good ones into shorts,
// and export each one. A short is its own timeline over the same source
// (the current cut inside its range), so it survives later main-cut edits.

import { useEffect, useRef, useState } from "react";
import { onAppEvent, revealPath } from "../ipc/api";
import {
  useCreateShort,
  useDeleteShort,
  useExportShort,
  useFindShorts,
  useProject,
  useShortCandidates,
  useShorts,
  useTranscript,
  useUpdateShort,
} from "../ipc/queries";
import type { ExportTarget, ProgressEvent, ShortCandidate, ShortSummary } from "../ipc/types";
import { previewRange, seekTo } from "../state/viewStore";
import { EXPORT_TARGETS } from "./ExportMenu";

const LENGTHS: { label: string; min_s: number; max_s: number }[] = [
  { label: "15–30s", min_s: 15, max_s: 30 },
  { label: "15–60s", min_s: 15, max_s: 60 },
  { label: "30–90s", min_s: 30, max_s: 90 },
];

function clock(s: number): string {
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
}

function errText(e: unknown): string {
  const o = e as { error?: { message?: string }; message?: string };
  return o?.error?.message ?? o?.message ?? String(e);
}

/** Room the export menu needs below its button (8 targets). */
const MENU_HEIGHT_PX = 280;

function slugify(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 48) || "short";
}

export function ShortsPanel({ projectId }: { projectId: string }) {
  const { data: transcript } = useTranscript(projectId);
  const { data: project } = useProject(projectId);
  const { data: candidates = [] } = useShortCandidates(projectId);
  const { data: shorts = [] } = useShorts(projectId);
  const find = useFindShorts();
  const create = useCreateShort();
  const [lengthIdx, setLengthIdx] = useState(1);
  const [method, setMethod] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const elapsed = useElapsed(find.isPending);
  const [progress, setProgress] = useState<string | null>(null);

  // The core narrates a find (outline → scoring batches); a long local-model
  // run should say where it is, not just spin.
  useEffect(
    () =>
      onAppEvent<ProgressEvent>("progress", (p) => {
        if (p.task === "shorts" && p.project_id === projectId) setProgress(p.message);
      }),
    [projectId],
  );

  const hasSpeech = !!transcript?.segments.some((s) => !s.is_silence);

  const runFind = () => {
    setError(null);
    setProgress(null);
    const len = LENGTHS[lengthIdx];
    find.mutate(
      { project_id: projectId, count: 8, min_s: len.min_s, max_s: len.max_s },
      {
        onSuccess: (r) => setMethod(r.method),
        onError: (e) => setError(errText(e)),
      },
    );
  };

  const makeShort = (c: ShortCandidate) => {
    setError(null);
    create.mutate(
      { project_id: projectId, candidate_id: c.id },
      { onError: (e) => setError(errText(e)) },
    );
  };

  if (!hasSpeech) {
    return (
      <div className="shorts-panel">
        <p className="shorts-note">Transcribe the video first — shorts are found from what you say.</p>
      </div>
    );
  }

  const shownMethod = method ?? candidates[0]?.method ?? null;

  return (
    <div className="shorts-panel">
      <p className="shorts-note">
        Moments that stand alone as Shorts / Reels / TikToks, ranked on the hook, whether it
        makes sense without context, and the payoff. Each short keeps your cut's removed
        silences and fillers.
      </p>

      <div className="shorts-find">
        <div className="shorts-lengths" role="radiogroup" aria-label="Short length">
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
          {find.isPending ? `finding… ${elapsed}s` : candidates.length ? "▸ find again" : "▸ find shorts"}
        </button>
      </div>
      {find.isPending && (
        <p className="shorts-note">
          {progress ?? "Reading the transcript…"} Scoring on-device can take a minute or two.
        </p>
      )}
      {error && <p className="empty-error">{error}</p>}

      {candidates.length > 0 && (
        <>
          <div className="shorts-label">
            suggestions
            {shownMethod && (
              <span className="shorts-method">
                {shownMethod === "llm" ? "scored by the on-device model" : "heuristic — no model reachable"}
              </span>
            )}
          </div>
          <div className="shorts-list">
            {candidates.map((c) => (
              <div key={c.id} className="short-row">
                <span className="short-score" title="short-form potential, 0–100">
                  {Math.round(c.score)}
                </span>
                <div className="short-body">
                  <button className="short-title" onClick={() => seekTo(c.start)} title="Jump to this moment">
                    {c.title}
                  </button>
                  <span className="short-meta">
                    {clock(c.start)}–{clock(c.end)} · {Math.round(c.duration)}s
                  </span>
                  <span className="short-hook">“{c.hook}”</span>
                  <span className="short-reason">{c.reason}</span>
                </div>
                <div className="short-actions">
                  <button className="ghost-btn" onClick={() => previewRange(c.start, c.end)} title="Play this moment">
                    ▶
                  </button>
                  <button className="ghost-btn" onClick={() => makeShort(c)} disabled={create.isPending}>
                    + short
                  </button>
                </div>
              </div>
            ))}
          </div>
        </>
      )}

      {shorts.length > 0 && (
        <>
          <div className="shorts-label">shorts ({shorts.length})</div>
          <div className="shorts-list">
            {shorts.map((s) => (
              <ShortRow
                key={s.id}
                projectId={projectId}
                projectName={project?.name ?? "project"}
                short={s}
              />
            ))}
          </div>
        </>
      )}
    </div>
  );
}

function ShortRow({
  projectId,
  projectName,
  short,
}: {
  projectId: string;
  projectName: string;
  short: ShortSummary;
}) {
  const update = useUpdateShort();
  const del = useDeleteShort();
  const exportShort = useExportShort();
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(short.title);
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
    if (t && t !== short.title) update.mutate({ project_id: projectId, short_id: short.id, title: t });
    else setTitle(short.title);
  };

  const runExport = (target: ExportTarget, ext: string, label: string) => {
    setMenuOpen(false);
    setStatus({ text: `exporting ${label}…` });
    exportShort.mutate(
      {
        project_id: projectId,
        short_id: short.id,
        target,
        out_path: `~/Downloads/${slugify(projectName)}-${slugify(short.title)}.${ext}`,
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

  return (
    <div className="short-row">
      <span className="short-score" title={short.score === null ? "hand-picked" : "short-form potential, 0–100"}>
        {short.score === null ? "—" : Math.round(short.score)}
      </span>
      <div className="short-body">
        {editing ? (
          <input
            className="short-title-input"
            value={title}
            autoFocus
            onChange={(e) => setTitle(e.target.value)}
            onBlur={saveTitle}
            onKeyDown={(e) => {
              if (e.key === "Enter") saveTitle();
              if (e.key === "Escape") {
                setTitle(short.title);
                setEditing(false);
              }
            }}
          />
        ) : (
          <button
            className="short-title"
            onClick={() => seekTo(short.start)}
            onDoubleClick={() => setEditing(true)}
            title="Jump to the short · double-click to rename"
          >
            {short.title}
          </button>
        )}
        <span className="short-meta">
          {clock(short.start)}–{clock(short.end)} · {Math.round(short.duration)}s
        </span>
        {status && (
          <span className="short-reason">
            {status.text}
            {status.path && (
              <button className="reveal-btn" onClick={() => void revealPath(status.path!)}>
                reveal
              </button>
            )}
          </span>
        )}
      </div>
      <div className="short-actions" ref={menuRef}>
        <button className="ghost-btn" onClick={() => previewRange(short.start, short.end)} title="Play the short">
          ▶
        </button>
        <button
          className="ghost-btn"
          onClick={() => {
            const row = menuRef.current?.getBoundingClientRect();
            const panel = menuRef.current?.closest(".shorts-panel")?.getBoundingClientRect();
            setMenuUp(!!row && !!panel && row.bottom + MENU_HEIGHT_PX > panel.bottom);
            setMenuOpen((v) => !v);
          }}
          disabled={exportShort.isPending}
        >
          export
        </button>
        <button
          className="ghost-btn short-delete"
          onClick={() => del.mutate({ project_id: projectId, short_id: short.id })}
          disabled={del.isPending}
          title="Delete this short"
        >
          ×
        </button>
        {menuOpen && (
          <div className={`export-dropdown${menuUp ? " up" : ""}`}>
            {EXPORT_TARGETS.map((t) => (
              <button key={t.target} className="export-item" onClick={() => runExport(t.target, t.ext, t.label)}>
                {t.label}
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
