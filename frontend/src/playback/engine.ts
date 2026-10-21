// The playback engine: every clock that can move the playhead, in one place.
//
// Five machines interact here — seek-follow, scrub audition, the transport
// (play/pause/rate), the mock-mode rAF clock, and the cut-skip audio fade —
// and their interplay is the part that breaks (an audition once dragged the
// playhead forward past a backward frame-step). Composing them in one hook
// makes the couplings explicit:
//   - audition SUPPRESSES timeupdate-driven playhead writes (auditioningRef)
//   - the mock clock runs only without a real <video> (src == null)
//   - the fade watcher owns volume; everyone else leaves it alone
//     (except seek, which restores full volume)
//   - a clip audition (previewing a clip's pieces) skips the gaps between
//     them even with skip-cuts off, and is ended by whichever clock reaches
//     its last piece first; any seek cancels it
//
// The UI (PreviewPanel) renders controls and calls `onTimeUpdate`; it never
// touches the video element's clock directly.

import { useEffect, useRef, type RefObject } from "react";
import { useStore } from "@tanstack/react-store";
import { auditionGaps, reachedAuditionEnd, setPlayhead, setPlaying, viewStore } from "../state/viewStore";
import { skipTarget, type Range } from "./time";

export interface PlaybackEngineArgs {
  videoRef: RefObject<HTMLVideoElement | null>;
  /** Playable media URL, or null in mock mode (drives the rAF clock instead). */
  src: string | null;
  ranges: Range[];
  duration: number;
}

export function usePlaybackEngine({ videoRef, src, ranges, duration }: PlaybackEngineArgs): {
  onTimeUpdate: () => void;
} {
  // Effects read these through refs so the rAF loops never capture stale cuts.
  const rangesRef = useRef(ranges);
  rangesRef.current = ranges;
  const durationRef = useRef(duration);
  durationRef.current = duration;

  // What playback skips right now: the cuts (when skipping them) plus,
  // during a clip audition, the gaps between its pieces. Sorted (skipTarget
  // scans in order).
  const skipRanges = (): Range[] => {
    const cuts = viewStore.state.skipCuts ? rangesRef.current : [];
    const gaps = auditionGaps();
    return gaps.length ? [...cuts, ...gaps].sort((a, b) => a.start - b.start) : cuts;
  };

  // --- seek-follow: the <video> element follows seek requests -------------
  const seekNonce = useStore(viewStore, (s) => s.seekNonce);
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const t = viewStore.state.playhead;
    if (Math.abs(v.currentTime - t) > 0.05) v.currentTime = t;
    v.volume = 1; // a manual seek always lands at full volume
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seekNonce]);

  // --- scrub audition: arrow-key jogs play a short audio burst ------------
  // While the burst plays, the playhead must NOT follow the video — otherwise
  // the forward drift of the audio outruns a backward frame step.
  const auditionNonce = useStore(viewStore, (s) => s.auditionNonce);
  const auditionTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const auditioningRef = useRef(false);
  useEffect(() => {
    if (auditionNonce === 0) return;
    const v = videoRef.current;
    if (!v || viewStore.state.playing) return;
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
    }, viewStore.state.auditionMs || 180);
    return () => {
      if (auditionTimer.current) clearTimeout(auditionTimer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [auditionNonce]);

  // --- transport: play / pause / rate on the real video element -----------
  const playing = useStore(viewStore, (s) => s.playing);
  const rate = useStore(viewStore, (s) => s.playbackRate);
  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    v.playbackRate = rate;
    if (playing) void v.play().catch(() => setPlaying(false));
    else v.pause();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [playing, rate, src]);

  // --- mock-mode rAF clock (no playable video) -----------------------------
  useEffect(() => {
    if (src || !playing) return;
    let raf = 0;
    let last = performance.now();
    const tick = (now: number) => {
      const dt = (now - last) / 1000;
      last = now;
      let t = viewStore.state.playhead + dt * viewStore.state.playbackRate;
      const jump = skipTarget(t, skipRanges());
      if (jump !== null) t = jump;
      if (t >= durationRef.current && durationRef.current > 0) {
        setPlayhead(durationRef.current);
        setPlaying(false);
        return;
      }
      if (reachedAuditionEnd(t)) return;
      setPlayhead(t);
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [src, playing]);

  // --- cut-skip audio fade --------------------------------------------------
  // Fade out over the last ~50ms before an excluded range, jump silently,
  // ramp back in. timeupdate is far too coarse for this; rAF it is.
  useEffect(() => {
    if (!src) return;
    const FADE_S = 0.05;
    let raf = 0;
    const tick = () => {
      raf = requestAnimationFrame(tick);
      const v = videoRef.current;
      if (!v || v.paused) return;
      // A range audition ends on the frame, not on the next ~250ms timeupdate.
      if (reachedAuditionEnd(v.currentTime)) {
        v.pause();
        return;
      }
      const ranges = skipRanges();
      if (ranges.length === 0) {
        if (v.volume < 1) v.volume = Math.min(1, v.volume + 0.15);
        return;
      }
      const t = v.currentTime;
      const upcoming = ranges.find((r) => t >= r.start - FADE_S && t < r.end);
      if (upcoming) {
        if (t < upcoming.start) {
          // Approaching the cut: proportional fade-out.
          v.volume = Math.max(0, (upcoming.start - t) / FADE_S);
        } else {
          // Inside the cut: jump silently to the other side.
          v.volume = 0;
          v.currentTime = upcoming.end;
          setPlayhead(upcoming.end);
        }
      } else if (v.volume < 1) {
        // Past the cut: quick ramp back up (~6 frames).
        v.volume = Math.min(1, v.volume + 0.15);
      }
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [src]);

  // --- timeupdate: the video element drives the playhead -------------------
  const onTimeUpdate = () => {
    const v = videoRef.current;
    if (!v) return;
    if (auditioningRef.current) return; // scrub audition: playhead stays put
    let t = v.currentTime;
    const jump = skipTarget(t, skipRanges());
    if (jump !== null) {
      v.currentTime = jump;
      t = jump;
    }
    if (reachedAuditionEnd(t)) return;
    setPlayhead(t);
  };

  return { onTimeUpdate };
}
