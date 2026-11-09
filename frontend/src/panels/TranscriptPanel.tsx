// Script panel: virtualized transcript as flowing paragraphs. Words seek the
// playhead; selected segments can be cut with Delete/Backspace; cut segments
// show struck-through (with a restore button) when "Show cuts" is on.

import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useStore } from "@tanstack/react-store";
import {
  useAcceptAllSuggestions,
  useAcceptSuggestion,
  useCutByTranscript,
  useCutRange,
  useDismissSuggestion,
  useGenerateRoughCut,
  usePreferences,
  useProject,
  useRestoreByTranscript,
  useRestoreRange,
  useRetimeWords,
  useSuggestions,
  useTimeline,
  useTranscript,
} from "../ipc/queries";
import {
  addToClipDraft,
  removeFromClipDraft,
  seekTo,
  selectSegment,
  setFocusedCut,
  setSelectedSegmentIds,
  viewStore,
} from "../state/viewStore";
import { CutReview, hearCut } from "./CutReview";
import type { Range } from "../playback/time";
import { ClipDraftBar } from "./ClipDraftBar";
import { callTool, onAppEvent } from "../ipc/api";
import type { CutSuggestion, ProgressEvent, ProposedCut, TranscriptSegment, Word } from "../ipc/types";
import { errText } from "./clipUtil";
import { setTranscribeError, transcribeErrors } from "../state/transcribeStore";

const normalize = (w: string) => w.toLowerCase().replace(/[^a-z']/g, "");
const NO_CUTS: ProposedCut[] = [];

function WordSpan({
  word,
  isFiller,
  highlight,
  cutWord,
  current,
  inClip,
  proposed,
}: {
  word: Word;
  isFiller: boolean;
  highlight: boolean;
  cutWord: boolean;
  current: boolean;
  /** Part of the clip being assembled ("Add to clip"). */
  inClip: boolean;
  /** In a proposed cut: "on", or "focus" when the review bar is on it. */
  proposed: "" | "on" | "focus";
}) {
  return (
    <span
      className={`word${isFiller ? " filler" : ""}${highlight ? " hit" : ""}${cutWord ? " cutw" : ""}${current ? " current" : ""}${inClip ? " inclip" : ""}${proposed ? " proposedw" : ""}${proposed === "focus" ? " focusw" : ""}`}
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
  suggested,
  onAcceptSuggestion,
  onDismissSuggestion,
  silenceAfter,
  onRestoreRange,
  clipDraft,
  proposedCuts,
  pausesBefore,
  focusedCut,
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
  suggested?: CutSuggestion;
  onAcceptSuggestion: (id: string) => void;
  onDismissSuggestion: (id: string) => void;
  silenceAfter?: { start: number; end: number; dur: number };
  onRestoreRange: (start: number, end: number) => void;
  clipDraft: Range[];
  /** Proposed cuts (not skipped) that touch this segment. */
  proposedCuts: ProposedCut[];
  /** Proposed pause cuts, by the index of the word they come before (-1:
   *  after the last word). */
  pausesBefore?: Map<number, ProposedCut[]>;
  focusedCut: number | null;
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

  const pauseChips = (cuts: ProposedCut[] | undefined) =>
    cuts?.map((c) => (
      <button
        key={`p${c.id}`}
        className={`proposed-pause${c.id === focusedCut ? " focus" : ""}`}
        title="A pause the proposal would cut — click to hear it"
        onClick={(e) => {
          e.stopPropagation();
          setFocusedCut(c.id);
          hearCut(c);
        }}
      >
        ⏸ {(c.end - c.start).toFixed(1)}s
      </button>
    ));
  const proposedAt = (mid: number) => proposedCuts.find((c) => mid >= c.start && mid < c.end);

  if (seg.is_silence) {
    const p = proposedAt((seg.start + seg.end) / 2);
    return (
      <div
        className={`segment silence${cut ? " cut" : ""}${selected ? " selected" : ""}${p ? " proposed" : ""}`}
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
      className={`segment${cut ? " cut" : ""}${selected ? " selected" : ""}${active ? " active" : ""}${suggested ? " suggested" : ""}`}
      data-segid={seg.id}
      onClick={(e) => selectSegment(seg.id, e.metaKey || e.ctrlKey)}
    >
      {suggested && !cut && (
        <div className="suggest-ribbon" onClick={(e) => e.stopPropagation()}>
          <span className="suggest-tag">
            {suggested.reason === "repeated_take" ? "repeated take" : "repetitive"}
          </span>
          <span className="suggest-hint">suggested cut</span>
          <button className="suggest-cut" onClick={() => onAcceptSuggestion(suggested.id)}>
            ✂ cut
          </button>
          <button className="suggest-keep" onClick={() => onDismissSuggestion(suggested.id)}>
            keep
          </button>
        </div>
      )}
      <p className="segment-text">
        {seg.words.map((w, i) => {
          const mid = (w.start + w.end) / 2;
          const wordCut = !cut && excludedRanges.some(([a, b]) => mid >= a && mid < b);
          if (wordCut && !showCuts) return null;
          const p = proposedAt(mid);
          return (
            <span key={i}>
              {pauseChips(pausesBefore?.get(i))}
              <WordSpan
                word={w}
                isFiller={fillerSet.has(normalize(w.text))}
                highlight={q.length > 0 && normalize(w.text).includes(q)}
                cutWord={wordCut}
                current={i === currentWordIdx}
                inClip={clipDraft.some((r) => mid >= r.start && mid < r.end)}
                proposed={p ? (p.id === focusedCut ? "focus" : "on") : ""}
              />
            </span>
          );
        })}
        {pauseChips(pausesBefore?.get(-1))}
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
      {silenceAfter && (
        <button
          className="silence-removed"
          title="A silent gap was removed here — click to restore it"
          onClick={(e) => {
            e.stopPropagation();
            onRestoreRange(silenceAfter.start, silenceAfter.end);
          }}
        >
          ⏸ {silenceAfter.dur.toFixed(1)}s pause removed · restore
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
  const retime = useRetimeWords();
  const { data: project } = useProject(projectId);
  const proposal = project?.cut_proposal ?? null;
  const focusedCut = useStore(viewStore, (s) => s.focusedCut);
  // The rough cut's steps, as the core reports them.
  const [roughStep, setRoughStep] = useState<string | null>(null);
  useEffect(
    () =>
      onAppEvent<ProgressEvent>("progress", (p) => {
        if (p.task !== "rough_cut" || (p.project_id && p.project_id !== projectId)) return;
        setRoughStep(p.fraction >= 1 ? null : p.message);
      }),
    [projectId],
  );
  const { data: suggestions } = useSuggestions(projectId);
  const acceptSuggestion = useAcceptSuggestion();
  const acceptAll = useAcceptAllSuggestions();
  const dismissSuggestion = useDismissSuggestion();
  // segment_id → suggestion, for inline highlighting.
  const suggestionBySeg = useMemo(() => {
    const m = new Map<string, CutSuggestion>();
    for (const s of suggestions ?? []) for (const sid of s.segment_ids) m.set(sid, s);
    return m;
  }, [suggestions]);
  const onAcceptSuggestion = (id: string) =>
    acceptSuggestion.mutate({ project_id: projectId, suggestion_id: id });
  const onDismissSuggestion = (id: string) =>
    dismissSuggestion.mutate({ project_id: projectId, suggestion_id: id });

  const showCuts = useStore(viewStore, (s) => s.showCuts);
  const clipDraft = useStore(viewStore, (s) => s.clipDraft);
  const selectedIds = useStore(viewStore, (s) => s.selectedSegmentIds);

  const [query, setQuery] = useState("");
  const [roughCutResult, setRoughCutResult] = useState<string | null>(null);
  const parentRef = useRef<HTMLDivElement | null>(null);

  // Visible transcription state: progress events from the core while whisper
  // works, plus a manual start/retry path that surfaces errors.
  const [transcribeProgress, setTranscribeProgress] = useState<ProgressEvent | null>(null);
  const [transcribeStarting, setTranscribeStarting] = useState(false);
  // Shared per project: an import's background transcription lands its
  // error here too (ingest.ts).
  const transcribeError = useStore(transcribeErrors, (s) => s[projectId] ?? null);

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
    setTranscribeError(projectId, null);
    try {
      await callTool("transcribe", { project_id: projectId });
    } catch (err) {
      setTranscribeError(projectId, `Transcription failed: ${errText(err)}`);
    } finally {
      setTranscribeStarting(false);
    }
  };

  const segments = useMemo(() => transcript?.segments ?? [], [transcript]);

  // A segment is cut when (almost) none of it plays. Not "linked to a cut
  // clip": a cut that was partly restored, or a saved clip's own timeline
  // (everything outside it cut), can link a segment that still mostly plays,
  // which then showed as cut, or vanished with cuts hidden.
  const cutIds = useMemo(() => {
    const ids = new Set<string>();
    const plays = (timeline?.clips ?? [])
      .filter((c) => c.included)
      .map((c) => [c.source_in, c.source_out] as const)
      .sort((a, b) => a[0] - b[0]);
    let k = 0;
    for (const seg of segments) {
      while (k > 0 && plays[k - 1]![1] > seg.start) k--;
      while (k < plays.length && plays[k]![1] <= seg.start) k++;
      let kept = 0;
      for (let j = k; j < plays.length && plays[j]![0] < seg.end; j++) {
        kept += Math.min(seg.end, plays[j]![1]) - Math.max(seg.start, plays[j]![0]);
      }
      if (kept < Math.max(0.05, 0.02 * (seg.end - seg.start))) ids.add(seg.id);
    }
    return ids;
  }, [timeline, segments]);

  // Excluded source ranges, for word-accurate strikethrough (range cuts made
  // via right-click don't link whole segments).
  const excludedRanges = useMemo(
    () =>
      (timeline?.clips ?? [])
        .filter((c) => !c.included)
        .map((c) => [c.source_in, c.source_out] as [number, number]),
    [timeline],
  );

  // Removed dead-air between segments: a gap (≥0.3s) with NO segment in it
  // that the timeline excludes was a removed pause. Marked after the earlier
  // segment, restorable — a breadcrumb shown even with "Show cuts" off.
  // (Gaps whisper emitted as is_silence segments render their own chip, so
  // iterating ALL segments in source order avoids double-marking.)
  const silenceAfter = useMemo(() => {
    const m = new Map<string, { start: number; end: number; dur: number }>();
    const segs = [...(transcript?.segments ?? [])].sort((a, b) => a.start - b.start);
    const clips = timeline?.clips ?? [];
    const includedAt = (t: number) =>
      clips.some((c) => c.included && c.source_in <= t && t < c.source_out);
    for (let i = 0; i < segs.length - 1; i++) {
      const a = segs[i]!;
      const b = segs[i + 1]!;
      const gap = b.start - a.end;
      if (gap >= 0.3 && !includedAt((a.end + b.start) / 2)) {
        m.set(a.id, { start: a.end, end: b.start, dur: gap });
      }
    }
    return m;
  }, [transcript, timeline]);

  // Proposed cuts (not skipped), per segment they touch; and proposed pause
  // cuts placed before the word that follows them (or after a segment's
  // last word when nothing follows), so each shows as a chip in the text.
  const { proposedBySeg, pausesBySeg } = useMemo(() => {
    const bySeg = new Map<string, ProposedCut[]>();
    const pauses = new Map<string, Map<number, ProposedCut[]>>();
    const live = (proposal?.cuts ?? []).filter((c) => !c.skipped);
    if (live.length === 0) return { proposedBySeg: bySeg, pausesBySeg: pauses };
    const segs = [...segments].sort((a, b) => a.start - b.start);
    for (const seg of segs) {
      const touching = live.filter((c) => c.end > seg.start && c.start < seg.end);
      if (touching.length) bySeg.set(seg.id, touching);
    }
    const words = segs
      .filter((s) => !s.is_silence)
      .flatMap((s) => s.words.map((w, i) => ({ seg: s.id, i, start: w.start })));
    let wi = 0;
    for (const c of live.filter((c) => c.kind === "pause").sort((a, b) => a.start - b.start)) {
      while (wi < words.length && words[wi]!.start < c.end - 0.25) wi++;
      const at = words[wi] ?? (words.length ? { ...words[words.length - 1]!, i: -1 } : null);
      if (!at) continue;
      const m = pauses.get(at.seg) ?? new Map<number, ProposedCut[]>();
      m.set(at.i, [...(m.get(at.i) ?? []), c]);
      pauses.set(at.seg, m);
    }
    return { proposedBySeg: bySeg, pausesBySeg: pauses };
  }, [proposal, segments]);

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
  const onRestoreRange = (start: number, end: number) => {
    restoreRange.mutate({ project_id: projectId, start, end });
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
      y: Math.min(e.clientY || 0, window.innerHeight - 230),
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

  // The menu's target as clip pieces: the text selection, or each selected
  // paragraph as its own piece (gaps between them are skipped in the clip).
  const menuPieces = (): Range[] => {
    if (!menu) return [];
    if (menu.range) return [menu.range];
    return menu.segIds
      .map((id) => segments.find((s) => s.id === id))
      .filter((s): s is TranscriptSegment => !!s)
      .map((s) => ({ start: s.start, end: s.end }));
  };
  const menuInClip =
    !!menu && menuPieces().some((p) => clipDraft.some((r) => r.start < p.end && p.start < r.end));

  const menuAddToClip = () => {
    menuPieces().forEach(addToClipDraft);
    finishMenuAction();
  };

  const menuRemoveFromClip = () => {
    menuPieces().forEach(removeFromClipDraft);
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
      {
        onSuccess: (res) => {
          setRoughStep(null);
          setRoughCutResult(
            res.cut_count === 0
              ? "nothing to cut: no pauses or filler words found"
              : `${res.cut_count} cuts proposed — nothing is cut yet; review them below`,
          );
        },
        onError: (err) => {
          setRoughStep(null);
          setRoughCutResult(`rough cut failed: ${errText(err)}`);
        },
      },
    );
  };

  const onRetime = () => {
    setRoughCutResult(null);
    retime.mutate(
      { project_id: projectId },
      {
        onSuccess: (res) =>
          setRoughCutResult(
            res.aligned
              ? `words aligned to the audio (${res.chunks - res.chunks_kept} of ${res.chunks} passages)`
              : `word edges trimmed (${res.trimmed} of ${res.words} words)${res.note ? ` · ${res.note}` : ""}`,
          ),
        onError: (err) => setRoughCutResult(`couldn't fix word timings: ${errText(err)}`),
      },
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
        <button
          className="primary-btn"
          onClick={onRoughCut}
          disabled={roughCut.isPending}
          title="Finds the pauses and filler words to cut and shows them for review. Nothing is cut until you Apply."
        >
          {roughCut.isPending ? "Analyzing…" : "Rough cut"}
        </button>
        {transcript && !transcript.words_aligned && (
          <button
            className="ghost-btn"
            onClick={onRetime}
            disabled={retime.isPending}
            title="This transcript's word times are estimates, so cuts aimed at words can land in the wrong place. Fixing lines every word up with the audio (with the word alignment model from Setup) and trims it to its sound; no text or cuts change."
          >
            {retime.isPending ? "Fixing…" : "Fix word timings"}
          </button>
        )}
        {roughCut.isPending && roughStep ? (
          <span className="rough-cut-result">{roughStep}…</span>
        ) : (
          roughCutResult !== null && <span className="rough-cut-result">{roughCutResult}</span>
        )}
      </div>

      {proposal && proposal.cuts.length > 0 && (
        <CutReview projectId={projectId} proposal={proposal} onDone={setRoughCutResult} />
      )}

      {(suggestions?.length ?? 0) > 0 && (
        <div className="suggest-review">
          <span className="suggest-review-label">
            {suggestions!.length} suggested cut{suggestions!.length === 1 ? "" : "s"} — repeated
            takes &amp; repetition, flagged for you (highlighted below)
          </span>
          <button
            className="primary-btn"
            disabled={acceptAll.isPending}
            onClick={() => acceptAll.mutate({ project_id: projectId })}
          >
            {acceptAll.isPending ? "cutting…" : "Cut all"}
          </button>
        </div>
      )}

      <div
        className="transcript-scroll"
        ref={parentRef}
        onContextMenu={onContextMenu}
        onWheel={() => { userScrollAtRef.current = Date.now(); }}
        onTouchMove={() => { userScrollAtRef.current = Date.now(); }}
      >
        {visible.length === 0 ? (
          segments.length === 0 && !transcribeError && (transcribeProgress || transcribeStarting) ? (
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
                    suggested={suggestionBySeg.get(seg.id)}
                    onAcceptSuggestion={onAcceptSuggestion}
                    onDismissSuggestion={onDismissSuggestion}
                    silenceAfter={silenceAfter.get(seg.id)}
                    onRestoreRange={onRestoreRange}
                    clipDraft={clipDraft}
                    proposedCuts={proposedBySeg.get(seg.id) ?? NO_CUTS}
                    pausesBefore={pausesBySeg.get(seg.id)}
                    focusedCut={focusedCut}
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>
      <ClipDraftBar projectId={projectId} />
      <div className="transcript-hint">
        Click a word to seek · ←/→ step words (⌥ frames, ⇧ seconds) · select text + right-click to
        cut or add to a clip · ⌘-click paragraphs for multi · Delete cuts selection
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
          <button onClick={menuAddToClip}>
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
              <rect x="2.5" y="3.5" width="11" height="9" />
              <path d="M8 6v4M6 8h4" />
            </svg>
            Add to clip
          </button>
          {menuInClip && (
            <button onClick={menuRemoveFromClip}>
              <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
                <rect x="2.5" y="3.5" width="11" height="9" />
                <path d="M6 8h4" />
              </svg>
              Remove from clip
            </button>
          )}
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
