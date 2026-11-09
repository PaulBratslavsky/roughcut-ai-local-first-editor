// One saved clip (a 16:9 cut-down or a 9:16 short): rename, switch format,
// frame the crop over the preview, open it in the Editor to edit it, export,
// delete. Shown on the Clips screen,
// with a checkbox each for batch export.

import { useEffect, useRef, useState } from "react";
import { useStore } from "@tanstack/react-store";
import { revealPath } from "../ipc/api";
import { useBeginClipEdit, useDeleteClip, useExportClip, useProject, useUpdateClip } from "../ipc/queries";
import type { ExportTarget, SavedClipSummary } from "../ipc/types";
import { previewRanges, seekTo, setCropClip, setScreen, togglePreviewCollapsed, viewStore } from "../state/viewStore";
import { clock, errText, FORMAT_LABEL, slugify } from "./clipUtil";
import { EXPORT_TARGETS } from "./ExportMenu";

/** Room the export menu needs below its button (8 targets). */
const MENU_HEIGHT_PX = 280;

export function SavedClipRow({
  projectId,
  projectName,
  clip,
  checked,
  onCheck,
}: {
  projectId: string;
  projectName: string;
  clip: SavedClipSummary;
  /** With `onCheck`: the row shows a checkbox (Clips screen batch export). */
  checked?: boolean;
  onCheck?: (checked: boolean) => void;
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
  const framing = useStore(viewStore, (s) => s.cropClipId === clip.id);
  const { data: project } = useProject(projectId);
  const openClip = project?.clip_session ?? null;
  const begin = useBeginClipEdit();

  // Open it in the Editor: the core swaps its timeline in (ClipEditBar then
  // hides cut material and jumps to it); a 9:16 one shows its crop.
  const editInEditor = () => {
    setStatus(null);
    begin.mutate(
      { project_id: projectId, clip_id: clip.id },
      {
        onSuccess: () => {
          setCropClip(clip.format === "vertical" ? clip.id : null);
          setScreen("editor");
        },
        onError: (e) => setStatus({ text: errText(e) }),
      },
    );
  };

  // Show this short's 9:16 window over the preview, on one of its frames.
  const toggleCrop = () => {
    if (framing) return setCropClip(null);
    if (viewStore.state.previewCollapsed) togglePreviewCollapsed();
    seekTo(clip.start);
    setCropClip(clip.id);
  };

  return (
    <div className="clip-row">
      {onCheck && (
        <input
          type="checkbox"
          className="tick-box"
          checked={!!checked}
          onChange={(e) => onCheck(e.target.checked)}
          aria-label={`Select “${clip.title}” for export`}
        />
      )}
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
          className={`ghost-btn${openClip?.clip_id === clip.id ? " active" : ""}`}
          onClick={editInEditor}
          disabled={begin.isPending || (openClip !== null && openClip.clip_id !== clip.id)}
          title={
            openClip && openClip.clip_id !== clip.id
              ? `“${openClip.title}” is open in the Editor: save or cancel it first`
              : "Open this clip in the Editor: cut words, restore, rough cut, chat — then save it back"
          }
        >
          {openClip?.clip_id === clip.id ? "editing" : "edit"}
        </button>
        {clip.format === "vertical" && (
          <button
            className={`ghost-btn${framing ? " active" : ""}`}
            onClick={toggleCrop}
            title="Frame the 9:16 crop over the preview: drag it, drag a corner to zoom, or centre it on the speaker"
          >
            crop
          </button>
        )}
        <button
          className="ghost-btn"
          onClick={() => {
            const row = menuRef.current?.getBoundingClientRect();
            const panel = menuRef.current?.closest(".clips-panel, .shorts-main")?.getBoundingClientRect();
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
