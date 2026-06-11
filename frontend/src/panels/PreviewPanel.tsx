// Video preview card: the <video> element (or mock placeholder), hover
// controls, and the Cut/Original readouts. Everything that moves the
// playhead lives in playback/engine.ts — this component only renders.

import { useMemo, useRef } from "react";
import { useStore } from "@tanstack/react-store";
import { isTauri, mediaSrc } from "../ipc/api";
import { useProject, useTimeline } from "../ipc/queries";
import { usePlaybackEngine } from "../playback/engine";
import { excludedRanges, excludedTotal, toEditedTime, type Range } from "../playback/time";
import {
  seekTo,
  setPlaybackRate,
  setPlaying,
  setSkipCuts,
  togglePlaying,
  viewStore,
} from "../state/viewStore";

function fmt(t: number): string {
  const m = Math.floor(t / 60);
  const s = Math.floor(t % 60);
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

export function PreviewPanel({ projectId }: { projectId: string }) {
  const { data: project } = useProject(projectId);
  const { data: timeline } = useTimeline(projectId);
  const videoRef = useRef<HTMLVideoElement | null>(null);

  const playing = useStore(viewStore, (s) => s.playing);
  const rate = useStore(viewStore, (s) => s.playbackRate);
  const playhead = useStore(viewStore, (s) => s.playhead);
  const skipCuts = useStore(viewStore, (s) => s.skipCuts);

  const duration = timeline?.duration ?? project?.media?.duration ?? 0;
  const src = useMemo(
    () => (project?.media && isTauri ? mediaSrc(project.media.file_path) : null),
    [project?.media],
  );
  const ranges = useMemo<Range[]>(() => excludedRanges(timeline?.clips ?? []), [timeline]);

  const { onTimeUpdate } = usePlaybackEngine({ videoRef, src, ranges, duration });

  const nudge = (delta: number) => {
    seekTo(Math.min(Math.max(0, viewStore.state.playhead + delta), duration));
  };

  // Cut-aware time: when watching the cut, both the clock and the total are
  // in EDITED time (source time minus everything excluded) — so a 42-min
  // source with 7 min cut reads "… / 35:06", matching what export produces.
  const editedTotal = Math.max(0, duration - excludedTotal(ranges));
  const shownCurrent = skipCuts
    ? toEditedTime(Math.min(playhead, duration), ranges)
    : Math.min(playhead, duration);
  const shownTotal = skipCuts ? editedTotal : duration;

  return (
    <div className="preview-card card">
      <div className="preview-frame" onClick={() => togglePlaying()}>
        {src ? (
          <video
            ref={videoRef}
            src={src}
            className="preview-video"
            onTimeUpdate={onTimeUpdate}
            onEnded={() => setPlaying(false)}
            playsInline
          />
        ) : (
          <div className="preview-placeholder">
            <svg width="44" height="44" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.4">
              <rect x="2" y="4" width="20" height="16" rx="2.5" />
              <path d="M2 8h20M2 16h20M7 4v16M17 4v16M7 8v4M17 8v4M7 16v4M17 16v4" opacity="0.55" />
              <path d="M10.5 10.2v3.6l3.2-1.8z" fill="currentColor" stroke="none" />
            </svg>
            <span>Demo footage (mock mode)</span>
          </div>
        )}

        {/* Big center affordance while paused */}
        {!playing && (
          <div className="preview-center-play" aria-hidden>
            <svg width="56" height="56" viewBox="0 0 56 56">
              <circle cx="28" cy="28" r="27" fill="rgba(0,0,0,0.45)" />
              <path d="M22 17v22l18-11z" fill="white" />
            </svg>
          </div>
        )}

        {/* Hover controls overlay */}
        <div className="preview-overlay" onClick={(e) => e.stopPropagation()}>
          <input
            className="preview-scrubber"
            type="range"
            min={0}
            max={Math.max(0.01, duration)}
            step={0.01}
            value={Math.min(playhead, duration)}
            onChange={(e) => seekTo(Number(e.target.value))}
          />
          <div className="preview-controls">
            <button className="overlay-btn" title="Back 5s" onClick={() => nudge(-5)}>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                <path d="M11 18V6l-8.5 6zM11.5 12l8.5 6V6z" transform="scale(-1,1) translate(-24,0)" />
              </svg>
            </button>
            <button className="overlay-btn play" title={playing ? "Pause (Space)" : "Play (Space)"} onClick={() => togglePlaying()}>
              {playing ? (
                <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                  <path d="M7 5h4v14H7zM13 5h4v14h-4z" />
                </svg>
              ) : (
                <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                  <path d="M8 5v14l11-7z" />
                </svg>
              )}
            </button>
            <button className="overlay-btn" title="Forward 5s" onClick={() => nudge(5)}>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                <path d="M11 18V6l-8.5 6zM11.5 12l8.5 6V6z" />
              </svg>
            </button>
            <span className="overlay-time">
              {fmt(Math.min(playhead, duration))} / {fmt(duration)}
            </span>
            <select
              className="overlay-rate"
              value={rate}
              title="Playback speed"
              onChange={(e) => setPlaybackRate(Number(e.target.value))}
            >
              <option value={1}>1x</option>
              <option value={1.5}>1.5x</option>
              <option value={2}>2x</option>
            </select>
          </div>
        </div>
      </div>
      <div className="preview-meta">
        <span className="cut-counter">{timeline?.cut_count ?? 0} Cuts</span>
        <div className="preview-version-toggle" role="group" aria-label="Preview version">
          <button
            className={skipCuts ? "active" : ""}
            title="Watch the edited cut (skips removed sections)"
            onClick={() => setSkipCuts(true)}
          >
            Cut · {fmt(editedTotal)}
          </button>
          <button
            className={!skipCuts ? "active" : ""}
            title="Watch the original footage (plays through cuts)"
            onClick={() => setSkipCuts(false)}
          >
            Original · {fmt(duration)}
          </button>
        </div>
        <span className="time-readout">
          {fmt(shownCurrent)} <span className="time-sep">/</span> {fmt(shownTotal)}
        </span>
      </div>
    </div>
  );
}
