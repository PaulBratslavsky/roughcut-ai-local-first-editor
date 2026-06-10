// Video preview card. Plays real media via convertFileSrc inside Tauri;
// renders a placeholder in mock mode and drives the playhead with a rAF
// clock. When "Skip cuts" is on, playback jumps over excluded ranges.

import { useEffect, useMemo, useRef } from "react";
import { useStore } from "@tanstack/react-store";
import { isTauri, mediaSrc } from "../ipc/api";
import { useProject, useTimeline } from "../ipc/queries";
import {
  excludedRanges,
  seekTo,
  setPlaybackRate,
  setPlayhead,
  setPlaying,
  setSkipCuts,
  skipTarget,
  togglePlaying,
  viewStore,
  type Range,
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
  const seekNonce = useStore(viewStore, (s) => s.seekNonce);
  const auditionNonce = useStore(viewStore, (s) => s.auditionNonce);
  const playhead = useStore(viewStore, (s) => s.playhead);
  const skipCuts = useStore(viewStore, (s) => s.skipCuts);

  const duration = timeline?.duration ?? project?.media?.duration ?? 0;
  const src = useMemo(
    () => (project?.media && isTauri ? mediaSrc(project.media.file_path) : null),
    [project?.media],
  );

  const ranges = useMemo<Range[]>(() => excludedRanges(timeline?.clips ?? []), [timeline]);
  const rangesRef = useRef(ranges);
  rangesRef.current = ranges;
  const durationRef = useRef(duration);
  durationRef.current = duration;

  // Follow seek requests.
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const t = viewStore.state.playhead;
    if (Math.abs(v.currentTime - t) > 0.05) v.currentTime = t;
  }, [seekNonce]);

  // Scrub audition: each arrow-key jog plays a short audio burst at the new
  // position (the audible cue for finding edit points). Repeated steps keep
  // the sound rolling like a jog wheel; irrelevant while already playing.
  const auditionTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const auditioningRef = useRef(false);
  useEffect(() => {
    if (auditionNonce === 0) return;
    const v = videoRef.current;
    if (!v || viewStore.state.playing) return;
    // While the burst plays, the playhead must NOT follow the video —
    // otherwise the forward drift of the audio outruns a backward frame step.
    auditioningRef.current = true;
    void v.play().catch(() => {});
    if (auditionTimer.current) clearTimeout(auditionTimer.current);
    auditionTimer.current = setTimeout(() => {
      auditioningRef.current = false;
      if (!viewStore.state.playing) {
        v.pause();
        // Snap the frame back to the scrub point the user is parked on.
        v.currentTime = viewStore.state.playhead;
      }
    }, 180);
    return () => {
      if (auditionTimer.current) clearTimeout(auditionTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [auditionNonce]);

  // Play / pause / rate on the real video element.
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    v.playbackRate = rate;
    if (playing) void v.play().catch(() => setPlaying(false));
    else v.pause();
  }, [playing, rate, src]);

  // Mock-mode rAF clock when there's no playable video.
  useEffect(() => {
    if (src || !playing) return;
    let raf = 0;
    let last = performance.now();
    const tick = (now: number) => {
      const dt = (now - last) / 1000;
      last = now;
      let t = viewStore.state.playhead + dt * viewStore.state.playbackRate;
      if (viewStore.state.skipCuts) {
        const jump = skipTarget(t, rangesRef.current);
        if (jump !== null) t = jump;
      }
      if (t >= durationRef.current && durationRef.current > 0) {
        setPlayhead(durationRef.current);
        setPlaying(false);
        return;
      }
      setPlayhead(t);
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [src, playing]);

  // Drive the playhead from the video element; skip excluded ranges.
  const onTimeUpdate = () => {
    const v = videoRef.current;
    if (!v) return;
    if (auditioningRef.current) return; // scrub audition: playhead stays put
    let t = v.currentTime;
    if (viewStore.state.skipCuts) {
      const jump = skipTarget(t, rangesRef.current);
      if (jump !== null) {
        v.currentTime = jump;
        t = jump;
      }
    }
    setPlayhead(t);
  };

  const nudge = (delta: number) => {
    seekTo(Math.min(Math.max(0, viewStore.state.playhead + delta), duration));
  };

  // Cut-aware time: when watching the cut, both the clock and the total are
  // in EDITED time (source time minus everything excluded) — so a 42-min
  // source with 7 min cut reads "… / 35:06", matching what export produces.
  const excludedTotal = ranges.reduce((acc, r) => acc + (r.end - r.start), 0);
  const editedTotal = Math.max(0, duration - excludedTotal);
  const toEditedTime = (t: number) => {
    let cut = 0;
    for (const r of ranges) {
      if (t >= r.end) cut += r.end - r.start;
      else if (t > r.start) cut += t - r.start;
      else break;
    }
    return Math.max(0, t - cut);
  };
  const shownCurrent = skipCuts ? toEditedTime(Math.min(playhead, duration)) : Math.min(playhead, duration);
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
