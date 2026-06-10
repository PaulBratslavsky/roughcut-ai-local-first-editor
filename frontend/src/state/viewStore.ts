// View state (TanStack Store). The Rust core owns project/timeline state;
// this is only ephemeral UI state: playhead, zoom, selection, toggles.

import { Store } from "@tanstack/react-store";
import type { Clip } from "../ipc/types";

export type ActiveTab = "script" | "chat";

export interface ViewState {
  projectId: string | null;
  playhead: number; // seconds, source time
  /** Bumped whenever a seek is requested so the <video> element follows. */
  seekNonce: number;
  /** Bumped by arrow-key scrubbing: the preview auditions a short audio burst. */
  auditionNonce: number;
  playing: boolean;
  playbackRate: number;
  zoom: number; // timeline px per second
  scrollX: number; // timeline scroll, in display seconds
  selectedClipId: string | null;
  selectedSegmentIds: string[];
  showCuts: boolean;
  skipCuts: boolean;
  activeTab: ActiveTab;
}

export const viewStore = new Store<ViewState>({
  projectId: null,
  playhead: 0,
  seekNonce: 0,
  auditionNonce: 0,
  playing: false,
  playbackRate: 1,
  zoom: 6,
  scrollX: 0,
  selectedClipId: null,
  selectedSegmentIds: [],
  showCuts: true,
  skipCuts: true,
  activeTab: "script",
});

function patch(p: Partial<ViewState>): void {
  viewStore.setState((s) => ({ ...s, ...p }));
}

export const setProjectId = (projectId: string | null) => patch({ projectId });
export const setPlayhead = (playhead: number) => patch({ playhead: Math.max(0, playhead) });

/** Seek: moves the playhead AND asks the video element to jump there. */
export function seekTo(t: number): void {
  viewStore.setState((s) => ({
    ...s,
    playhead: Math.max(0, t),
    seekNonce: s.seekNonce + 1,
  }));
}

/** Arrow-key jog: step the playhead and audition audio at the new position. */
export function scrubBy(delta: number, max: number): void {
  viewStore.setState((s) => ({
    ...s,
    playhead: Math.min(Math.max(0, s.playhead + delta), max),
    seekNonce: s.seekNonce + 1,
    auditionNonce: s.auditionNonce + 1,
  }));
}

export const setPlaying = (playing: boolean) => patch({ playing });
export const togglePlaying = () => patch({ playing: !viewStore.state.playing });
export const setPlaybackRate = (playbackRate: number) => patch({ playbackRate });
export const setShowCuts = (showCuts: boolean) => patch({ showCuts });
export const setSkipCuts = (skipCuts: boolean) => patch({ skipCuts });
export const setActiveTab = (activeTab: ActiveTab) => patch({ activeTab });
export const setSelectedClipId = (selectedClipId: string | null) => patch({ selectedClipId });
export const setSelectedSegmentIds = (selectedSegmentIds: string[]) => patch({ selectedSegmentIds });
export const setScrollX = (scrollX: number) => patch({ scrollX: Math.max(0, scrollX) });

export function selectSegment(id: string, additive: boolean): void {
  viewStore.setState((s) => {
    if (!additive) return { ...s, selectedSegmentIds: [id] };
    const has = s.selectedSegmentIds.includes(id);
    return {
      ...s,
      selectedSegmentIds: has
        ? s.selectedSegmentIds.filter((x) => x !== id)
        : [...s.selectedSegmentIds, id],
    };
  });
}

export const ZOOM_MIN = 1.5;
export const ZOOM_MAX = 240;

/** Zoom keeping the playhead at the same screen x position. */
export function zoomAroundPlayhead(factor: number, playheadDisplayTime: number): void {
  viewStore.setState((s) => {
    const zoom = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, s.zoom * factor));
    if (zoom === s.zoom) return s;
    const px = (playheadDisplayTime - s.scrollX) * s.zoom;
    const scrollX = Math.max(0, playheadDisplayTime - px / zoom);
    return { ...s, zoom, scrollX };
  });
}

// ---------------------------------------------------------------------------
// Excluded-range helpers (used by playback skip + timeline)
// ---------------------------------------------------------------------------

export interface Range { start: number; end: number }

export function excludedRanges(clips: Clip[]): Range[] {
  return clips
    .filter((c) => !c.included)
    .map((c) => ({ start: c.source_in, end: c.source_out }))
    .sort((a, b) => a.start - b.start);
}

/** If `t` is inside an excluded range, return the time to jump to instead. */
export function skipTarget(t: number, ranges: Range[]): number | null {
  for (const r of ranges) {
    if (t >= r.start && t < r.end - 1e-4) return r.end;
    if (r.start > t) break;
  }
  return null;
}
