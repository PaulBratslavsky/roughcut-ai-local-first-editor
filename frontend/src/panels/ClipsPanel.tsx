// Clips tab: suggested moments that stand alone as Shorts / Reels / TikToks,
// saved from here or from pieces picked in the transcript ("Add to clip").
// Each clip is 16:9 or 9:16, is its own timeline over the same source (the
// current cut inside its pieces), and survives later main-cut edits. The
// saved clips themselves live on the Clips screen (play, crop, export); a
// scan for many at once on the Shorts screen.

import { useEffect, useState } from "react";
import { useIsMutating } from "@tanstack/react-query";
import { onAppEvent } from "../ipc/api";
import {
  clipRunKeys,
  useClipCandidates,
  useClips,
  useCreateClip,
  useFindClips,
  useTranscript,
} from "../ipc/queries";
import type { ClipCandidate, ClipFormat, ProgressEvent } from "../ipc/types";
import { previewRanges, seekTo, setScreen } from "../state/viewStore";
import { clock, errText, scoredBy, useElapsed } from "./clipUtil";
import { modelLabel, useModelStatus } from "../state/modelStore";

const LENGTHS: { label: string; min_s: number; max_s: number }[] = [
  { label: "15–30s", min_s: 15, max_s: 30 },
  { label: "15–60s", min_s: 15, max_s: 60 },
  { label: "30–90s", min_s: 30, max_s: 90 },
];

export function ClipsPanel({ projectId }: { projectId: string }) {
  const { data: transcript } = useTranscript(projectId);
  const { data: candidates = [] } = useClipCandidates(projectId);
  const { data: clips = [] } = useClips(projectId);
  const find = useFindClips(projectId);
  // A find or scan for this project, from here or the Shorts screen: one at a time.
  const finding = useIsMutating({ mutationKey: clipRunKeys.find(projectId) }) > 0;
  const create = useCreateClip();
  const model = useModelStatus();
  const [lengthIdx, setLengthIdx] = useState(1);
  const [theme, setTheme] = useState("");
  const [error, setError] = useState<string | null>(null);
  const elapsed = useElapsed(finding);
  const [progress, setProgress] = useState<string | null>(null);

  // The core narrates a find (outline → picks, topic by topic); a long local-model
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
    if (finding) return;
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

  // Saved clips are listed, played, cropped and exported on the Clips screen.
  const savedLink = (
    <div className="clips-find">
      <span className="clips-note">
        {clips.length === 0
          ? "No saved clips yet."
          : `${clips.length} saved ${clips.length === 1 ? "clip" : "clips"}: play, crop and export them on the Clips screen.`}
      </span>
      <button className="ghost-btn" onClick={() => setScreen("clips")} title="Open the Clips screen">
        ▸ open clips
      </button>
    </div>
  );

  // Always reachable: the Shorts screen says "transcribe first" itself.
  const openReview = (
    <div className="clips-find">
      <span className="clips-note">Many at once: scan the whole video and pick shorts side by side.</span>
      <button className="ghost-btn" onClick={() => setScreen("shorts")} title="Open the Shorts screen">
        ▸ open review
      </button>
    </div>
  );

  if (!hasSpeech) {
    return (
      <div className="clips-panel">
        <p className="clips-note">Transcribe the video first — clips are found from what you say.</p>
        {openReview}
        {savedLink}
      </div>
    );
  }

  const scorers = scoredBy(candidates);

  return (
    <div className="clips-panel">
      <p className="clips-note">
        Save clips from the transcript (select text → right-click → Add to clip), or let the
        finder suggest moments that stand alone as Shorts / Reels / TikToks. Each clip keeps your
        cut's removed silences and fillers.
      </p>

      {openReview}

      <input
        className="clips-theme"
        value={theme}
        onChange={(e) => setTheme(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !finding) runFind();
        }}
        placeholder="about… (optional) — e.g. AI tools, pricing, my guitar plugin"
        aria-label="Only find clips about this topic"
        disabled={finding}
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
              disabled={finding}
            >
              {l.label}
            </button>
          ))}
        </div>
        <button className="ghost-btn" onClick={runFind} disabled={finding}>
          {finding ? `finding… ${elapsed}s` : candidates.length ? "▸ find again" : "▸ find clips"}
        </button>
      </div>
      {finding && (
        <p className="clips-note">
          {progress ?? "Reading the transcript…"} Scoring with {modelLabel(model)} can take a minute or two.
        </p>
      )}
      {error && <p className="empty-error">{error}</p>}

      {savedLink}

      {candidates.length > 0 && (
        <>
          <div className="clips-label">
            <span>
              suggestions
              {candidates[0]?.theme && <span className="clips-method"> · about “{candidates[0].theme}”</span>}
            </span>
            <span className="clips-method" title={scorers.raw ? `Scored by: ${scorers.raw}` : undefined}>
              {scorers.text ? `scored by ${scorers.text}` : "rule-based — no model scored these"}
            </span>
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
                    onClick={() => previewRanges(c.pieces.length ? c.pieces : [{ start: c.start, end: c.end }])}
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
