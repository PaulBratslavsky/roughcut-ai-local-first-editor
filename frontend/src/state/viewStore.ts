// View state (TanStack Store). The Rust core owns project/timeline state;
// this is only ephemeral UI state: playhead, zoom, selection, toggles.

import { Store } from "@tanstack/react-store";

export type ActiveTab = "tools" | "chat";

export interface ViewState {
  projectId: string | null;
  playhead: number; // seconds, source time
  /** Bumped whenever a seek is requested so the <video> element follows. */
  seekNonce: number;
  /** Bumped by arrow-key scrubbing: the preview auditions a short audio burst. */
  auditionNonce: number;
  /** Length of the audition burst (ms) — word jumps audition the whole word. */
  auditionMs: number;
  playing: boolean;
  playbackRate: number;
  zoom: number; // timeline px per second
  scrollX: number; // timeline scroll, in display seconds
  selectedClipId: string | null;
  selectedSegmentIds: string[];
  showCuts: boolean;
  skipCuts: boolean;
  activeTab: ActiveTab;
  /** Collapse the video picture to give the tabs room (audio keeps playing). */
  previewCollapsed: boolean;
}

export const viewStore = new Store<ViewState>({
  projectId: null,
  playhead: 0,
  seekNonce: 0,
  auditionNonce: 0,
  auditionMs: 180,
  playing: false,
  playbackRate: 1,
  zoom: 6,
  scrollX: 0,
  selectedClipId: null,
  selectedSegmentIds: [],
  showCuts: true,
  skipCuts: true,
  activeTab: "tools",
  previewCollapsed: false,
});

function patch(p: Partial<ViewState>): void {
  viewStore.setState((s) => ({ ...s, ...p }));
}

/** Switching projects resets the viewport — scroll, zoom, playhead, and
 *  selection belong to the project you were just looking at. (A 2:50 short
 *  opened after a 40-minute edit otherwise lands on an empty timeline,
 *  scrolled to minute 7 of footage that doesn't exist.) */
export function setProjectId(projectId: string | null): void {
  viewStore.setState((s) => {
    if (s.projectId === projectId) return s;
    return {
      ...s,
      projectId,
      playhead: 0,
      seekNonce: s.seekNonce + 1,
      playing: false,
      zoom: 6,
      scrollX: 0,
      selectedClipId: null,
      selectedSegmentIds: [],
    };
  });
}
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
  scrubTo(viewStore.state.playhead + delta, max, 180);
}

/** Jump the playhead to an absolute position with an audition of `ms`. */
export function scrubTo(t: number, max: number, ms: number): void {
  viewStore.setState((s) => ({
    ...s,
    playhead: Math.min(Math.max(0, t), max),
    seekNonce: s.seekNonce + 1,
    auditionNonce: s.auditionNonce + 1,
    auditionMs: ms,
  }));
}

export const setPlaying = (playing: boolean) => patch({ playing });
export const togglePlaying = () => patch({ playing: !viewStore.state.playing });
export const setPlaybackRate = (playbackRate: number) => patch({ playbackRate });
export const setShowCuts = (showCuts: boolean) => patch({ showCuts });
export const setSkipCuts = (skipCuts: boolean) => patch({ skipCuts });
export const setActiveTab = (activeTab: ActiveTab) => patch({ activeTab });
export const togglePreviewCollapsed = () =>
  patch({ previewCollapsed: !viewStore.state.previewCollapsed });
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

// Cut-time math lives in playback/time.ts; re-exported here because the
// timeline and transcript panels reach for it alongside view state.
export { excludedRanges, skipTarget, type Range } from "../playback/time";
