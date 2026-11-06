// The review bar for proposed cuts: what the Rough cut button or the chat
// worked out, before anything is cut. Step through the cuts (each is marked
// in the transcript and timeline), hear one (a few seconds either side, with
// it taken out), leave cuts or whole kinds out, then Apply as one undoable
// edit — or Discard. The player already skips proposed cuts, so pressing
// play previews the edit as it would be.

import { useEffect, useMemo, useState } from "react";
import { useStore } from "@tanstack/react-store";
import {
  useApplyCutProposal,
  useDiscardCutProposal,
  useSkipProposedCuts,
  useTimeline,
} from "../ipc/queries";
import type { CutProposal, ProposedCut } from "../ipc/types";
import { previewRanges, seekTo, setFocusedCut, setPlaying, setSkipCuts, viewStore } from "../state/viewStore";
import { errText } from "./clipUtil";

/** Seconds of context played either side of a cut. */
const CONTEXT_S = 2.5;

const KINDS: { kind: "pause" | "filler" | "words"; label: string }[] = [
  { kind: "pause", label: "pauses" },
  { kind: "filler", label: "filler words" },
  { kind: "words", label: "other words" },
];

export function clock(s: number): string {
  const t = Math.max(0, s);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const sec = Math.floor(t % 60);
  return h > 0
    ? `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}`
    : `${m}:${String(sec).padStart(2, "0")}`;
}

function span(s: number): string {
  return s >= 60 ? clock(s) : `${s.toFixed(1)}s`;
}

function what(c: ProposedCut): string {
  if (c.kind === "pause") return `pause · ${span(c.end - c.start)}`;
  return `“${c.text}” · ${span(c.end - c.start)}`;
}

/** Hear a cut as it would be: a few seconds before, then straight on after. */
export function hearCut(c: ProposedCut): void {
  setSkipCuts(true);
  previewRanges(
    c.skipped
      ? [{ start: Math.max(0, c.start - CONTEXT_S), end: c.end + CONTEXT_S }]
      : [
          { start: Math.max(0, c.start - CONTEXT_S), end: c.start },
          { start: c.end, end: c.end + CONTEXT_S },
        ],
  );
}

export function CutReview({
  projectId,
  proposal,
  onDone,
}: {
  projectId: string;
  proposal: CutProposal;
  /** A one-line result for the toolbar after Apply or Discard. */
  onDone: (message: string) => void;
}) {
  const { data: timeline } = useTimeline(projectId);
  const apply = useApplyCutProposal();
  const discard = useDiscardCutProposal();
  const skip = useSkipProposedCuts();
  const focusedId = useStore(viewStore, (s) => s.focusedCut);
  const [showList, setShowList] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const cuts = proposal.cuts;
  const live = useMemo(() => cuts.filter((c) => !c.skipped), [cuts]);
  const removes = live.reduce((a, c) => a + c.end - c.start, 0);
  const before = (timeline?.clips ?? [])
    .filter((c) => c.included)
    .reduce((a, c) => a + c.source_out - c.source_in, 0);
  const idx = Math.max(0, cuts.findIndex((c) => c.id === focusedId));
  const focused = cuts[idx];

  // Land on the first cut when a proposal arrives; clear on the way out.
  useEffect(() => {
    if (!cuts.some((c) => c.id === focusedId)) setFocusedCut(cuts[0]?.id ?? null);
  }, [cuts, focusedId]);
  useEffect(() => () => setFocusedCut(null), []);

  const go = (i: number) => {
    const c = cuts[(i + cuts.length) % cuts.length];
    if (!c) return;
    setFocusedCut(c.id);
    setPlaying(false);
    seekTo(Math.max(0, c.start - 0.5));
  };

  const run = <T,>(p: Promise<T>) => p.catch((e) => setError(errText(e)));

  const toggleKind = (kind: "pause" | "filler" | "words") => {
    const ofKind = cuts.filter((c) => c.kind === kind);
    const allOn = ofKind.every((c) => !c.skipped);
    void run(skip.mutateAsync({ project_id: projectId, kind, skip: allOn }));
  };
  const toggleCut = (c: ProposedCut) =>
    void run(skip.mutateAsync({ project_id: projectId, cut_ids: [c.id], skip: !c.skipped }));

  const onApply = () => {
    setError(null);
    apply.mutate(
      { project_id: projectId },
      {
        onSuccess: (r) =>
          onDone(
            r.applied > 0
              ? `applied ${r.applied} cut${r.applied === 1 ? "" : "s"} (${span(r.seconds)}) · undo with ⌘Z`
              : "every cut was skipped — nothing cut",
          ),
        onError: (e) => setError(errText(e)),
      },
    );
  };
  const onDiscard = () => {
    setError(null);
    discard.mutate(
      { project_id: projectId },
      { onSuccess: () => onDone("proposal discarded — nothing cut"), onError: (e) => setError(errText(e)) },
    );
  };

  const playEdit = () => {
    setSkipCuts(true);
    seekTo(Math.max(0, (focused?.start ?? 0) - CONTEXT_S));
    setPlaying(true);
  };

  return (
    <div className="cut-review" role="region" aria-label="Proposed cuts">
      <div className="cut-review-head">
        <span className="cut-review-tag">proposed</span>
        <span className="cut-review-title">{proposal.title}</span>
        <span className="cut-review-sum">
          {live.length} cut{live.length === 1 ? "" : "s"} · removes {span(removes)}
          {before > 0 && ` · ${clock(before)} → ${clock(Math.max(0, before - removes))}`}
        </span>
        <span className="cut-review-spacer" />
        <button className="ghost-btn" onClick={onDiscard} disabled={discard.isPending || apply.isPending}>
          Discard
        </button>
        <button className="primary-btn" onClick={onApply} disabled={apply.isPending || live.length === 0}>
          {apply.isPending ? "Applying…" : `Apply ${live.length} cut${live.length === 1 ? "" : "s"}`}
        </button>
      </div>

      <div className="cut-review-kinds">
        {KINDS.map(({ kind, label }) => {
          const ofKind = cuts.filter((c) => c.kind === kind);
          if (ofKind.length === 0) return null;
          const on = ofKind.filter((c) => !c.skipped);
          const secs = on.reduce((a, c) => a + c.end - c.start, 0);
          const state = on.length === ofKind.length ? "on" : on.length === 0 ? "off" : "some";
          return (
            <button
              key={kind}
              className={`cut-kind ${state}`}
              onClick={() => toggleKind(kind)}
              disabled={skip.isPending}
              title={state === "off" ? `Cut the ${label} again` : `Keep the ${label} (leave these cuts out)`}
            >
              <span className="cut-kind-box">{state === "on" ? "■" : state === "some" ? "▪" : " "}</span>
              {label} {on.length}
              {on.length !== ofKind.length && `/${ofKind.length}`} · {span(secs)}
            </button>
          );
        })}
        <span className="cut-review-spacer" />
        <button className="ghost-btn" onClick={playEdit} title="Play from this cut with every proposed cut taken out">
          ▶ play the edit
        </button>
        <button className="ghost-btn" onClick={() => setShowList((v) => !v)}>
          {showList ? "hide list" : "list all"}
        </button>
      </div>

      {focused && (
        <div className={`cut-review-nav${focused.skipped ? " skipped" : ""}`}>
          <button className="ghost-btn" onClick={() => go(idx - 1)} aria-label="Previous cut">
            ‹
          </button>
          <span className="cut-review-pos">
            {idx + 1}/{cuts.length}
          </span>
          <button className="cut-review-at" onClick={() => go(idx)} title="Show it in the transcript and timeline">
            {clock(focused.start)}
          </button>
          <span className="cut-review-what">
            {focused.kind === "filler" ? "filler " : focused.kind === "words" ? "words " : ""}
            {what(focused)}
            {focused.skipped && " · kept"}
          </span>
          <button className="ghost-btn" onClick={() => hearCut(focused)} title="Play a few seconds either side, with this cut taken out">
            ▶ hear it
          </button>
          <button className="ghost-btn" onClick={() => toggleCut(focused)} disabled={skip.isPending}>
            {focused.skipped ? "cut it" : "keep it"}
          </button>
          <button className="ghost-btn" onClick={() => go(idx + 1)} aria-label="Next cut">
            ›
          </button>
        </div>
      )}

      {showList && (
        <ol className="cut-review-list">
          {cuts.map((c) => (
            <li key={c.id} className={`${c.id === focusedId ? "focused" : ""}${c.skipped ? " skipped" : ""}`}>
              <button className="cut-review-at" onClick={() => go(cuts.indexOf(c))}>
                {clock(c.start)}
              </button>
              <span className={`cut-review-kind k-${c.kind}`}>{c.kind}</span>
              <span className="cut-review-what">{what(c)}</span>
              <button className="ghost-btn" onClick={() => hearCut(c)}>
                ▶
              </button>
              <button className="ghost-btn" onClick={() => toggleCut(c)} disabled={skip.isPending}>
                {c.skipped ? "cut" : "keep"}
              </button>
            </li>
          ))}
        </ol>
      )}
      {error && <div className="cut-review-error">{error}</div>}
    </div>
  );
}
