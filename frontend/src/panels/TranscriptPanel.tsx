// Script panel: virtualized transcript as flowing paragraphs. Words seek the
// playhead; selected segments can be cut with Delete/Backspace; cut segments
// show struck-through (with a restore button) when "Show cuts" is on.

import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useStore } from "@tanstack/react-store";
import {
  useCutByTranscript,
  useCutRange,
  useGenerateRoughCut,
  usePreferences,
  useRestoreByTranscript,
  useRestoreRange,
  useTimeline,
  useTranscript,
} from "../ipc/queries";
import {
  seekTo,
  selectSegment,
  setSelectedSegmentIds,
  viewStore,
} from "../state/viewStore";
import { callTool, onAppEvent } from "../ipc/api";
import type { ProgressEvent, TranscriptSegment, Word } from "../ipc/types";

const normalize = (w: string) => w.toLowerCase().replace(/[^a-z']/g, "");

function WordSpan({
  word,
  isFiller,
  highlight,
  cutWord,
  current,
}: {
  word: Word;
  isFiller: boolean;
  highlight: boolean;
  cutWord: boolean;
  current: boolean;
}) {
  return (
    <span
      className={`word${isFiller ? " filler" : ""}${highlight ? " hit" : ""}${cutWord ? " cutw" : ""}${current ? " current" : ""}`}
      data-wstart={word.start}
      data-wend={word.end}
      onClick={(e) => {
        e.stopPropagation();
        seekTo(word.start);
      }}
    >
      {word.text}{" "}
    </span>
  );
}

function SegmentBlock({
  seg,
  cut,
  selected,
  active,
  fillerSet,
  query,
  onRestore,
  excludedRanges,
  showCuts,
}: {
  seg: TranscriptSegment;
  cut: boolean;
  selected: boolean;
  active: boolean;
  fillerSet: Set<string>;
  query: string;
  onRestore: (id: string) => void;
  excludedRanges: Array<[number, number]>;
  showCuts: boolean;
}) {
  // Word under the playhead — selector returns an index, so this block only
  // re-renders when the spoken word changes (and only while it's active).
  const currentWordIdx = useStore(viewStore, (s) => {
    if (!active) return -1;
    for (let i = 0; i < seg.words.length; i++) {
      const w = seg.words[i]!;
      if (s.playhead >= w.start && s.playhead < w.end) return i;
    }
    return -1;
  });

  if (seg.is_silence) {
    return (
      <div
        className={`segment silence${cut ? " cut" : ""}${selected ? " selected" : ""}`}
        data-segid={seg.id}
        onClick={(e) => selectSegment(seg.id, e.metaKey || e.ctrlKey)}
      >
        <span className="silence-chip">
          silence · {(seg.end - seg.start).toFixed(1)}s
        </span>
        {cut && (
          <button
            className="restore-btn"
            onClick={(e) => {
              e.stopPropagation();
              onRestore(seg.id);
            }}
          >
            Restore
          </button>
        )}
      </div>
    );
  }

  const q = query.toLowerCase();
  return (
    <div
      className={`segment${cut ? " cut" : ""}${selected ? " selected" : ""}${active ? " active" : ""}`}
      data-segid={seg.id}
      onClick={(e) => selectSegment(seg.id, e.metaKey || e.ctrlKey)}
    >
      <p className="segment-text">
        {seg.words.map((w, i) => {
          const mid = (w.start + w.end) / 2;
          const wordCut = !cut && excludedRanges.some(([a, b]) => mid >= a && mid < b);
          if (wordCut && !showCuts) return null;
          return (
            <WordSpan
              key={i}
              word={w}
              isFiller={fillerSet.has(normalize(w.text))}
              highlight={q.length > 0 && normalize(w.text).includes(q)}
              cutWord={wordCut}
              current={i === currentWordIdx}
            />
          );
        })}
      </p>
      {cut && (
        <button
          className="restore-btn"
          onClick={(e) => {
            e.stopPropagation();
            onRestore(seg.id);
          }}
        >
          Restore
        </button>
      )}
    </div>
  );
}

export function TranscriptPanel({ projectId }: { projectId: string }) {
  const { data: transcript } = useTranscript(projectId);
  const { data: timeline } = useTimeline(projectId);
  const { data: prefs } = usePreferences();
  const cutByTranscript = useCutByTranscript();
  const restoreByTranscript = useRestoreByTranscript();
  const roughCut = useGenerateRoughCut();

  const showCuts = useStore(viewStore, (s) => s.showCuts);
  const selectedIds = useStore(viewStore, (s) => s.selectedSegmentIds);

  const [query, setQuery] = useState("");
  const [roughCutResult, setRoughCutResult] = useState<number | null>(null);
  const parentRef = useRef<HTMLDivElement | null>(null);

  // Visible transcription state: progress events from the core while whisper
  // works, plus a manual start/retry path that surfaces errors.
  const [transcribeProgress, setTranscribeProgress] = useState<ProgressEvent | null>(null);
  const [transcribeStarting, setTranscribeStarting] = useState(false);
  const [transcribeError, setTranscribeError] = useState<string | null>(null);

  useEffect(() => {
    setTranscribeProgress(null);
    return onAppEvent<ProgressEvent>("progress", (p) => {
      if (p.task !== "transcribe") return;
      if (p.project_id && p.project_id !== projectId) return;
      setTranscribeProgress(p.fraction >= 1 ? null : p);
    });
  }, [projectId]);

  const startTranscribe = async () => {
    setTranscribeStarting(true);
    setTranscribeError(null);
    try {
      await callTool("transcribe", { project_id: projectId });
    } catch (err) {
      setTranscribeError(String((err as { message?: string })?.message ?? err));
    } finally {
      setTranscribeStarting(false);
    }
  };

  const segments = useMemo(() => transcript?.segments ?? [], [transcript]);

  const cutIds = useMemo(() => {
    const ids = new Set<string>();
    for (const clip of timeline?.clips ?? []) {
      if (!clip.included) for (const id of clip.linked_segment_ids) ids.add(id);
    }
    return ids;
  }, [timeline]);

  // Excluded source ranges, for word-accurate strikethrough (range cuts made
  // via right-click don't link whole segments).
  const excludedRanges = useMemo(
    () =>
      (timeline?.clips ?? [])
        .filter((c) => !c.included)
        .map((c) => [c.source_in, c.source_out] as [number, number]),
    [timeline],
  );

  const fillerSet = useMemo(() => {
    const s = new Set(["um", "uh"]);
    for (const w of prefs?.custom_filler_words ?? []) {
      const n = normalize(w);
      if (n) s.add(n);
    }
    return s;
  }, [prefs]);

  const visible = useMemo(() => {
    const q = query.toLowerCase().trim();
    return segments.filter((seg) => {
      if (!showCuts && cutIds.has(seg.id)) return false;
      if (q && !seg.text.toLowerCase().includes(q) && !seg.is_silence) return false;
      if (q && seg.is_silence) return false;
      return true;
    });
  }, [segments, showCuts, cutIds, query]);

  const virtualizer = useVirtualizer({
    count: visible.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 76,
    overscan: 10,
  });

  // Track the segment under the playhead (re-renders only when the id changes).
  const activeSegId = useStore(viewStore, (s) => {
    const seg = segments.find((x) => s.playhead >= x.start && s.playhead < x.end);
    return seg?.id ?? null;
  });

  // Follow the playhead: scroll the active segment into view while scrubbing
  // or playing — but yield to the user's own scrolling for a moment.
  const userScrollAtRef = useRef(0);
  useEffect(() => {
    if (!activeSegId) return;
    if (Date.now() - userScrollAtRef.current < 2500) return;
    const idx = visible.findIndex((seg) => seg.id === activeSegId);
    if (idx >= 0) virtualizer.scrollToIndex(idx, { align: "auto" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSegId]);

  // Delete/Backspace cuts the selected segments.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable) return;
      if ((e.key === "Delete" || e.key === "Backspace") && viewStore.state.selectedSegmentIds.length > 0) {
        e.preventDefault();
        cutByTranscript.mutate({
          project_id: projectId,
          segment_ids: viewStore.state.selectedSegmentIds,
        });
        setSelectedSegmentIds([]);
      } else if (e.key === "Escape") {
        setSelectedSegmentIds([]);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  const onRestore = (id: string) => {
    restoreByTranscript.mutate({ project_id: projectId, segment_ids: [id] });
  };

  // ---- right-click context menu: cut/restore the selected text's video ----
  const cutRange = useCutRange();
  const restoreRange = useRestoreRange();
  const [menu, setMenu] = useState<{
    x: number;
    y: number;
    label: string;
    range: { start: number; end: number } | null;
    segIds: string[];
  } | null>(null);

  const onContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    // 1st choice: the actual text selection, mapped to a source time range
    // via the word spans' data attributes.
    let range: { start: number; end: number } | null = null;
    let wordCount = 0;
    const sel = window.getSelection();
    if (sel && !sel.isCollapsed && sel.rangeCount > 0 && parentRef.current) {
      const r = sel.getRangeAt(0);
      let start = Infinity;
      let end = -Infinity;
      parentRef.current.querySelectorAll<HTMLElement>("[data-wstart]").forEach((el) => {
        if (r.intersectsNode(el)) {
          start = Math.min(start, Number(el.dataset.wstart));
          end = Math.max(end, Number(el.dataset.wend));
          wordCount++;
        }
      });
      if (wordCount > 0 && end > start) range = { start, end };
    }
    // Fallback: the clicked paragraph (plus any selected paragraphs).
    let segIds: string[] = [];
    if (!range) {
      const segEl = (e.target as HTMLElement).closest<HTMLElement>("[data-segid]");
      const segId = segEl?.dataset.segid;
      if (segId) segIds = selectedIds.includes(segId) ? selectedIds : [segId];
      else if (selectedIds.length > 0) segIds = selectedIds;
    }
    if (!range && segIds.length === 0) return;
    const label = range
      ? `${wordCount} word${wordCount === 1 ? "" : "s"} · ${(range.end - range.start).toFixed(1)}s`
      : `${segIds.length} paragraph${segIds.length === 1 ? "" : "s"}`;
    setMenu({
      x: Math.min(e.clientX || 0, window.innerWidth - 220),
      y: Math.min(e.clientY || 0, window.innerHeight - 160),
      label,
      range,
      segIds,
    });
  };

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && close();
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey);
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", close, true);
    };
  }, [menu]);

  const finishMenuAction = () => {
    window.getSelection()?.removeAllRanges();
    setSelectedSegmentIds([]);
    setMenu(null);
  };

  const menuCut = () => {
    if (!menu) return;
    if (menu.range) {
      cutRange.mutate({ project_id: projectId, start: menu.range.start, end: menu.range.end });
    } else if (menu.segIds.length > 0) {
      cutByTranscript.mutate({ project_id: projectId, segment_ids: menu.segIds });
    }
    finishMenuAction();
  };

  const menuRestore = () => {
    if (!menu) return;
    if (menu.range) {
      restoreRange.mutate({ project_id: projectId, start: menu.range.start, end: menu.range.end });
    } else if (menu.segIds.length > 0) {
      restoreByTranscript.mutate({ project_id: projectId, segment_ids: menu.segIds });
    }
    finishMenuAction();
  };

  const menuPlayHere = () => {
    if (!menu) return;
    const t = menu.range?.start ?? segments.find((s) => s.id === menu.segIds[0])?.start;
    if (t != null) seekTo(t);
    setMenu(null);
  };

  const onRoughCut = () => {
    setRoughCutResult(null);
    roughCut.mutate(
      { project_id: projectId, aggressiveness: prefs?.cut_aggressiveness ?? "natural" },
      { onSuccess: (res) => setRoughCutResult(res.cut_count) },
    );
  };

  return (
    <div className="transcript-panel">
      <div className="transcript-toolbar">
        <div className="search-box">
          <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
            <circle cx="7" cy="7" r="5" />
            <path d="M11 11l3.5 3.5" />
          </svg>
          <input
            type="search"
            placeholder="Search transcript"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <button className="primary-btn" onClick={onRoughCut} disabled={roughCut.isPending}>
          {roughCut.isPending ? "Cutting…" : "Rough cut"}
        </button>
        {roughCutResult !== null && (
          <span className="rough-cut-result">{roughCutResult} cuts made</span>
        )}
      </div>

      <div
        className="transcript-scroll"
        ref={parentRef}
        onContextMenu={onContextMenu}
        onWheel={() => { userScrollAtRef.current = Date.now(); }}
        onTouchMove={() => { userScrollAtRef.current = Date.now(); }}
      >
        {visible.length === 0 ? (
          segments.length === 0 && (transcribeProgress || transcribeStarting) ? (
            <div className="transcribe-progress">
              <div className="spinner" />
              <p className="transcribe-message">
                {transcribeProgress?.message ?? "Transcribing on-device…"}
              </p>
              <div className="progress-track">
                <div
                  className={`progress-fill${(transcribeProgress?.fraction ?? 0) < 0.95 ? " pulsing" : ""}`}
                  style={{
                    width: `${Math.max(8, Math.round((transcribeProgress?.fraction ?? 0.05) * 100))}%`,
                  }}
                />
              </div>
              <p className="transcribe-hint">
                whisper runs locally — a long video can take a few minutes
              </p>
            </div>
          ) : (
            <div className="transcript-empty">
              {segments.length === 0 ? (
                <>
                  <p>No transcript yet.</p>
                  <button className="primary-btn" onClick={() => void startTranscribe()}>
                    Transcribe
                  </button>
                  {transcribeError && <p className="empty-error">{transcribeError}</p>}
                </>
              ) : (
                "No matching segments."
              )}
            </div>
          )
        ) : (
          <div
            className="transcript-inner"
            style={{ height: virtualizer.getTotalSize(), position: "relative" }}
          >
            {virtualizer.getVirtualItems().map((item) => {
              const seg = visible[item.index]!;
              return (
                <div
                  key={seg.id}
                  data-index={item.index}
                  ref={virtualizer.measureElement}
                  style={{
                    position: "absolute",
                    top: 0,
                    left: 0,
                    width: "100%",
                    transform: `translateY(${item.start}px)`,
                  }}
                >
                  <SegmentBlock
                    seg={seg}
                    cut={cutIds.has(seg.id)}
                    selected={selectedIds.includes(seg.id)}
                    active={activeSegId === seg.id}
                    fillerSet={fillerSet}
                    query={query.trim()}
                    onRestore={onRestore}
                    excludedRanges={excludedRanges}
                    showCuts={showCuts}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>
      <div className="transcript-hint">
        Click a word to seek · select text + right-click to cut · ⌘-click paragraphs for multi ·
        Delete cuts selection
      </div>

      {menu && (
        <div
          className="context-menu"
          style={{ left: menu.x, top: menu.y }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div className="context-menu-label">{menu.label}</div>
          <button onClick={menuCut}>
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
              <circle cx="4" cy="4" r="2.2" />
              <circle cx="4" cy="12" r="2.2" />
              <path d="M5.8 5.4 14 13M5.8 10.6 14 3" />
            </svg>
            Cut from video
          </button>
          <button onClick={menuRestore}>
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
              <path d="M2.5 8a5.5 5.5 0 1 1 1.6 3.9" />
              <path d="M2.5 12V8h4" />
            </svg>
            Restore
          </button>
          <button onClick={menuPlayHere}>
            <svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor" stroke="none">
              <path d="M5 3.5v9l7.5-4.5z" />
            </svg>
            Play from here
          </button>
        </div>
      )}
    </div>
  );
}
