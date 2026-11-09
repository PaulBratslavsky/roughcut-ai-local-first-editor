// One scan result on the Shorts screen: a frame from the video, the title,
// the opening line, the short score with its reason, and the playable
// length. Tick it to make it into a short; the picture (or "preview") plays
// its pieces in the player beside the grid.

import { useEffect, useMemo, useState } from "react";
import { useStore } from "@tanstack/react-store";
import { mediaSrc } from "../ipc/api";
import { useMediaAssets } from "../ipc/queries";
import type { ClipCandidate, ThumbnailRef } from "../ipc/types";
import { previewRanges, viewStore } from "../state/viewStore";
import { clock } from "./clipUtil";

/** The filmstrip frame nearest `t` (about one every 10 s on a long video). */
export function nearestThumb(thumbs: ThumbnailRef[], t: number): ThumbnailRef | null {
  let best: ThumbnailRef | null = null;
  for (const th of thumbs) {
    if (!best || Math.abs(th.time - t) < Math.abs(best.time - t)) best = th;
  }
  return best;
}

export function CandidateCard({
  projectId,
  candidate: c,
  checked,
  onCheck,
}: {
  projectId: string;
  candidate: ClipCandidate;
  checked: boolean;
  onCheck: (checked: boolean) => void;
}) {
  const pieces = c.pieces.length > 0 ? c.pieces : [{ start: c.start, end: c.end }];
  const at = pieces[0]!.start;
  // The filmstrip get_media_assets already caches. Empty in demo mode and
  // the browser mock: the card then shows a plain block with the timecode.
  const { data: assets } = useMediaAssets(projectId);
  const thumbUrl = useMemo(() => {
    const th = nearestThumb(assets?.thumbnails ?? [], at);
    return th ? mediaSrc(th.path) : null;
  }, [assets?.thumbnails, at]);
  const [broken, setBroken] = useState(false);
  useEffect(() => setBroken(false), [thumbUrl]);
  const picture = !!thumbUrl && !broken;
  // Only while it actually plays: a paused audition is not "playing".
  const playing = useStore(
    viewStore,
    (s) => s.playing && s.auditionIsCandidate && s.audition !== null && s.audition[0]?.start === at,
  );

  const preview = () => previewRanges(pieces, { candidate: c.id });
  const score = Math.round(c.score);

  return (
    <div className={`short-card${checked ? " checked" : ""}${playing ? " playing" : ""}`}>
      <button
        className={`short-thumb${picture ? " picture" : ""}`}
        onClick={preview}
        title="Play this moment in the player"
        aria-label={`Play “${c.title}”`}
      >
        {picture ? (
          <img src={thumbUrl!} alt="" draggable={false} onError={() => setBroken(true)} />
        ) : (
          <span className="short-thumb-time">{clock(at)}</span>
        )}
        <span className="short-thumb-len" aria-hidden>
          {Math.round(c.duration)}s
        </span>
        {playing && (
          <span className="short-thumb-now" aria-hidden>
            ▶ playing
          </span>
        )}
        <span className="short-thumb-play" aria-hidden>
          ▶ play
        </span>
      </button>
      <div className="short-card-body">
        <label className="short-card-head">
          <input
            type="checkbox"
            className="tick-box"
            checked={checked}
            onChange={(e) => onCheck(e.target.checked)}
            aria-label={`Make “${c.title}” into a short`}
          />
          <span className="short-card-title">{c.title}</span>
          <span
            className="short-score"
            title="Short score, 0–100: a hook, making sense alone, a payoff — judged from the transcript"
          >
            {score}
            <span className="short-score-label">short score</span>
          </span>
        </label>
        <span className="clip-hook">“{c.hook}”</span>
        <span className="short-reason">{c.reason}</span>
        <div className="short-card-foot">
          <span className="clip-meta">
            {clock(c.start)}–{clock(c.end)} · {Math.round(c.duration)}s
            {pieces.length > 1 && ` · ${pieces.length} pieces`}
            {c.framing && (
              <span title="You framed this short's 9:16 crop; making it keeps that framing">
                {" "}
                · framed{c.framing.crop_zoom > 1.005 && ` ${c.framing.crop_zoom.toFixed(1)}×`}
              </span>
            )}
          </span>
          <button className="ghost-btn" onClick={preview} aria-label={`Preview “${c.title}”`}>
            ▶ preview
          </button>
        </div>
      </div>
    </div>
  );
}
