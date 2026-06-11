// Editor-wide keyboard transport: Space toggles playback; ←/→ walk the
// transcript word by word (auditioning each word), ⌥ steps a frame, ⇧ a
// second. Lives outside App so the shell stays layout-only.

import { useEffect, useRef } from "react";
import type { Project, Transcript } from "../ipc/types";
import { scrubBy, scrubTo, togglePlaying, viewStore } from "../state/viewStore";

export function useEditorKeyboard(project: Project | undefined, transcript: Transcript | null | undefined): void {
  // Flat, time-ordered word list for ←/→ word navigation.
  const wordsRef = useRef<{ start: number; end: number }[]>([]);
  useEffect(() => {
    wordsRef.current = (transcript?.segments ?? [])
      .filter((seg) => !seg.is_silence)
      .flatMap((seg) => seg.words.map((w) => ({ start: w.start, end: w.end })))
      .sort((a, b) => a.start - b.start);
  }, [transcript]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      if (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.tagName === "SELECT" || target.isContentEditable) return;
      if (e.code === "Space") {
        e.preventDefault();
        togglePlaying();
      } else if (e.code === "ArrowLeft" || e.code === "ArrowRight") {
        e.preventDefault();
        const max = project?.timeline.duration ?? Number.MAX_SAFE_INTEGER;
        const back = e.code === "ArrowLeft";
        const words = wordsRef.current;
        if (e.shiftKey) {
          scrubBy(back ? -1 : 1, max); // coarse: 1 second
        } else if (e.altKey || words.length === 0) {
          const fps = project?.media?.frame_rate || 30;
          scrubBy((back ? -1 : 1) / fps, max); // fine: 1 frame (⌥)
        } else {
          // Default: walk the transcript word by word, auditioning the word.
          const t = viewStore.state.playhead;
          const target = back
            ? [...words].reverse().find((w) => w.start < t - 0.02)
            : words.find((w) => w.start > t + 0.02);
          if (target) {
            const ms = Math.min(700, Math.max(160, (target.end - target.start) * 1000));
            scrubTo(target.start, max, ms);
          }
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project?.media?.frame_rate, project?.timeline.duration]);
}
