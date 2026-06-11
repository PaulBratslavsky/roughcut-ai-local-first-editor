// View state (TanStack Store). The Rust core owns project/timeline state;
// this is only ephemeral UI state: playhead, zoom, selection, toggles.
// A few toggles persist across launches (localStorage) — they're workflow
// preferences, not session state.

import { Store } from "@tanstack/react-store";

const PERSIST_KEY = "rc-view-prefs";

interface PersistedPrefs {
  showCuts: boolean;
  skipCuts: boolean;
  previewCollapsed: boolean;
}

function loadPrefs(): PersistedPrefs {
  const defaults: PersistedPrefs = { showCuts: true, skipCuts: true, previewCollapsed: false };
  try {
    const raw = localStorage.getItem(PERSIST_KEY);
    if (raw) return { ...defaults, ...(JSON.parse(raw) as Partial<PersistedPrefs>) };
  } catch {
    /* first run / private mode */
  }
  return defaults;
}

function savePrefs(s: PersistedPrefs): void {
  try {
    localStorage.setItem(
      PERSIST_KEY,
      JSON.stringify({
        showCuts: s.showCuts,
        skipCuts: s.skipCuts,
        previewCollapsed: s.previewCollapsed,
      }),
    );
  } catch {
    /* best-effort */
  }
}

export type ActiveTab = "tools" | "chat";
export type AppScreen = "editor" | "recorder";

export interface ViewState {
  /** App-level screen: the editor (default) or the full-screen recorder. */
  screen: AppScreen;
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
  screen: "editor",
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
  activeTab: "tools",
  ...loadPrefs(),
});

function patch(p: Partial<ViewState>): void {
  viewStore.setState((s) => ({ ...s, ...p }));
}

/** Switching projects resets the viewport — scroll, playhead, and selection
 *  belong to the project you were just looking at. (A 2:50 short opened
 *  after a 40-minute edit otherwise lands on an empty timeline, scrolled to
 *  minute 7 of footage that doesn't exist.) Zoom is owned by the timeline's
 *  auto-fit, which runs when the new project's duration is known. */
export function setProjectId(projectId: string | null): void {
  viewStore.setState((s) => {
    if (s.projectId === projectId) return s;
    return {
      ...s,
      projectId,
      playhead: 0,
      seekNonce: s.seekNonce + 1,
      playing: false,
      scrollX: 0,
      selectedClipId: null,
      selectedSegmentIds: [],
    };
  });
}
export const setScreen = (screen: AppScreen) => patch({ screen });
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
export const setShowCuts = (showCuts: boolean) => {
  patch({ showCuts });
  savePrefs(viewStore.state);
};
export const setSkipCuts = (skipCuts: boolean) => {
  patch({ skipCuts });
  savePrefs(viewStore.state);
};
export const setActiveTab = (activeTab: ActiveTab) => patch({ activeTab });
export const togglePreviewCollapsed = () => {
  patch({ previewCollapsed: !viewStore.state.previewCollapsed });
  savePrefs(viewStore.state);
};
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

export const ZOOM_MIN = 0.5; // low enough to fit ~45 min on a wide screen
export const ZOOM_MAX = 240;

/** Fit the whole video into the visible timeline width (project open). */
export function fitTimelineToWidth(durationS: number, widthPx: number): void {
  if (durationS <= 0 || widthPx <= 0) return;
  const zoom = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, (widthPx - 12) / durationS));
  patch({ zoom, scrollX: 0 });
}

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
