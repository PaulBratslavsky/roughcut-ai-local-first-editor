// Video preview card: the <video> element (or mock placeholder), hover
// controls, and the Cut/Original readouts. Everything that moves the
// playhead lives in playback/engine.ts — this component only renders.

import { useEffect, useMemo, useRef, useState } from "react";
import { useStore } from "@tanstack/react-store";
import { isTauri, mediaSrc } from "../ipc/api";
import { useMediaAssets, useProject, useTimeline } from "../ipc/queries";
import { registerAudibleElement } from "../playback/meter";
import { usePlaybackEngine } from "../playback/engine";
import { excludedRanges, excludedTotal, toEditedTime, type Range } from "../playback/time";
import {
  seekTo,
  setPlaybackRate,
  setPlaying,
  setSkipCuts,
  togglePlaying,
  togglePreviewCollapsed,
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
  const collapsed = useStore(viewStore, (s) => s.previewCollapsed);

  const duration = timeline?.duration ?? project?.media?.duration ?? 0;
  // Some sources can't stream in a <video> tag (mp4 index at the end); the
  // core then provides a remuxed playable copy alongside peaks/thumbnails.
  const { data: assets } = useMediaAssets(projectId);
  const src = useMemo(() => {
    if (!project?.media) return null;
    const path = assets?.playback_path ?? project.media.file_path;
    if (!isTauri && !path.startsWith("http")) return null;
    return mediaSrc(path);
  }, [project?.media, assets?.playback_path]);
  const ranges = useMemo<Range[]>(() => excludedRanges(timeline?.clips ?? []), [timeline]);

  const { onTimeUpdate } = usePlaybackEngine({ videoRef, src, ranges, duration });
  const [videoError, setVideoError] = useState<string | null>(null);

  // A/V sync nudge: with a non-zero offset the <video> is muted and a shadow
  // <audio> of the same source carries the sound, shifted by the offset and
  // re-synced whenever drift exceeds 50ms. Offset 0 = no follower at all.
  const audioOffset = project?.audio_offset_s ?? 0;
  const audioRef = useRef<HTMLAudioElement | null>(null);

  // Dual-capture compositing: the screen rides as a synced, muted follower
  // layer; the camera (the MASTER element — clock + audio) floats as the
  // PiP. All CSS — sources untouched, layout switchable any time.
  const layout = project?.layout ?? { mode: "pip", shape: "rounded", corner: "br", size: 0.25 };
  const screenSrc = useMemo(
    () => (project?.screen_media && isTauri ? mediaSrc(project.screen_media.file_path) : null),
    [project?.screen_media],
  );
  const compositing = !!screenSrc && layout.mode !== "camera";
  const screenRef = useRef<HTMLVideoElement | null>(null);
  useEffect(() => {
    const master = videoRef.current;
    const screen = screenRef.current;
    if (!master || !screen || !compositing) return;
    const resync = () => {
      if (Math.abs(screen.currentTime - master.currentTime) > 0.08) {
        screen.currentTime = master.currentTime;
      }
      screen.playbackRate = master.playbackRate;
    };
    const onPlay = () => {
      resync();
      void screen.play().catch(() => {});
    };
    const onPause = () => screen.pause();
    master.addEventListener("play", onPlay);
    master.addEventListener("pause", onPause);
    master.addEventListener("seeked", resync);
    master.addEventListener("timeupdate", resync);
    master.addEventListener("ratechange", resync);
    if (!master.paused) onPlay();
    return () => {
      master.removeEventListener("play", onPlay);
      master.removeEventListener("pause", onPause);
      master.removeEventListener("seeked", resync);
      master.removeEventListener("timeupdate", resync);
      master.removeEventListener("ratechange", resync);
      screen.pause();
    };
  }, [compositing, screenSrc, src]);

  const pipStyle = useMemo(() => {
    if (!compositing) return undefined;
    if (layout.mode === "screen") {
      // Keep the master mounted (clock + audio) but invisible.
      return { position: "absolute" as const, width: 1, height: 1, opacity: 0, pointerEvents: "none" as const };
    }
    const inset = "3%";
    const pos: Record<string, string> = {};
    if (layout.corner.includes("t")) pos.top = inset;
    else pos.bottom = inset;
    if (layout.corner.includes("l")) pos.left = inset;
    else pos.right = inset;
    return {
      position: "absolute" as const,
      width: `${Math.round(layout.size * 100)}%`,
      height: "auto", // .preview-video sets 100%; the bubble must not
      ...(layout.shape === "round"
        ? { aspectRatio: "1 / 1", borderRadius: "50%", objectFit: "cover" as const }
        : { borderRadius: "6px" }),
      ...pos,
      zIndex: 2,
      boxShadow: "0 2px 14px rgba(0,0,0,0.55)",
    };
  }, [compositing, layout]);
  useEffect(() => {
    registerAudibleElement(
      audioOffset !== 0 ? audioRef.current : videoRef.current,
    );
    return () => registerAudibleElement(null);
  }, [audioOffset, src]);
  useEffect(() => {
    const video = videoRef.current;
    const audio = audioRef.current;
    if (!video || !audio || audioOffset === 0) return;
    const target = () => Math.max(0, video.currentTime - audioOffset);
    const resync = () => {
      if (Math.abs(audio.currentTime - target()) > 0.05) audio.currentTime = target();
      audio.playbackRate = video.playbackRate;
    };
    const onPlay = () => {
      resync();
      void audio.play().catch(() => {});
    };
    const onPause = () => audio.pause();
    video.addEventListener("play", onPlay);
    video.addEventListener("pause", onPause);
    video.addEventListener("seeked", resync);
    video.addEventListener("timeupdate", resync);
    video.addEventListener("ratechange", resync);
    if (!video.paused) onPlay();
    return () => {
      video.removeEventListener("play", onPlay);
      video.removeEventListener("pause", onPause);
      video.removeEventListener("seeked", resync);
      video.removeEventListener("timeupdate", resync);
      video.removeEventListener("ratechange", resync);
      audio.pause();
    };
  }, [audioOffset, src]);

  // The frame takes the media's real shape (vertical shorts get a tall,
  // centered frame instead of drowning in a 16:9 letterbox), capped so the
  // tools below stay reachable.
  const w = (compositing ? project?.screen_media?.width : project?.media?.width) || 16;
  const h = (compositing ? project?.screen_media?.height : project?.media?.height) || 9;
  const frameStyle = {
    aspectRatio: `${w} / ${h}`,
    width: `min(100%, calc(46vh * ${(w / h).toFixed(4)}))`,
    margin: "0 auto",
  };

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
      {/* Collapsed: the picture hides but the <video> stays MOUNTED, so
          playback state (and audio, if playing) survives the toggle. */}
      {audioOffset !== 0 && src && <audio ref={audioRef} src={src} preload="auto" />}
      <div className="preview-stage" style={collapsed ? { display: "none" } : undefined}>
      <div className="preview-frame" style={frameStyle} onClick={() => togglePlaying()}>
        {compositing && screenSrc && (
          <video
            key={screenSrc}
            ref={screenRef}
            src={screenSrc}
            className="screen-layer"
            muted
            playsInline
            onError={() =>
              setVideoError(
                "screen track missing or unplayable (was the file deleted from the library?) — Tools ▸ Layout ▸ camera only",
              )
            }
          />
        )}
        {src ? (
          <video
            key={src}
            ref={videoRef}
            src={src}
            style={pipStyle}
            className="preview-video"
            onTimeUpdate={onTimeUpdate}
            onEnded={() => setPlaying(false)}
            onLoadedData={() => setVideoError(null)}
            onError={() => {
              const err = videoRef.current?.error;
              setVideoError(
                `player error ${err?.code ?? "?"}: ${err?.message || "source could not be decoded"}`,
              );
            }}
            playsInline
            muted={audioOffset !== 0}
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

        {videoError && (
          <div className="preview-error" role="alert">
            {videoError}
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
        <button
          className="preview-hide-btn"
          title={collapsed ? "Show the video picture" : "Hide the video picture (audio keeps playing) — more room for tools & chat"}
          onClick={() => togglePreviewCollapsed()}
        >
          {collapsed ? "▸ show video" : "▾ hide video"}
        </button>
      </div>
    </div>
  );
}
