// View state (TanStack Store). The Rust core owns project/timeline state;
// this is only ephemeral UI state: playhead, zoom, selection, toggles.
// A few toggles persist across launches (localStorage) — they're workflow
// preferences, not session state.

import { Store } from "@tanstack/react-store";
import type { Range } from "../playback/time";

const PERSIST_KEY = "rc-view-prefs";

interface PersistedPrefs {
  showCuts: boolean;
  skipCuts: boolean;
  previewCollapsed: boolean;
  shortsDrawerOpen: boolean;
  shortsDrawerTab: ShortsDrawerTab;
}

function loadPrefs(): PersistedPrefs {
  const defaults: PersistedPrefs = {
    showCuts: true,
    skipCuts: true,
    previewCollapsed: false,
    shortsDrawerOpen: false,
    shortsDrawerTab: "chat",
  };
  try {
    const raw = localStorage.getItem(PERSIST_KEY);
    if (raw) {
      // Before the drawer had a chat tab, its open flag was shortsTerminalOpen.
      const { shortsTerminalOpen, ...saved } = JSON.parse(raw) as Partial<PersistedPrefs> & {
        shortsTerminalOpen?: boolean;
      };
      return {
        ...defaults,
        ...saved,
        shortsDrawerOpen: saved.shortsDrawerOpen ?? shortsTerminalOpen ?? defaults.shortsDrawerOpen,
        shortsDrawerTab: saved.shortsDrawerTab === "terminal" ? "terminal" : "chat",
      };
    }
  } catch {
    /* first run / private mode */
  }
  return defaults;
}

function savePrefs(s: PersistedPrefs & { clipEditShowCuts?: boolean | null }): void {
  try {
    localStorage.setItem(
      PERSIST_KEY,
      JSON.stringify({
        // While a clip is open, cuts are hidden for it: keep the user's own setting.
        showCuts: s.clipEditShowCuts ?? s.showCuts,
        skipCuts: s.skipCuts,
        previewCollapsed: s.previewCollapsed,
        shortsDrawerOpen: s.shortsDrawerOpen,
        shortsDrawerTab: s.shortsDrawerTab,
      }),
    );
  } catch {
    /* best-effort */
  }
}

/** The drawer under the player on the Shorts and Clips screens: the chat or
 *  the terminal. */
export type ShortsDrawerTab = "chat" | "terminal";

export type ActiveTab = "tools" | "chat" | "footage" | "clips" | "history" | "terminal";
/** The editor (default), the Shorts review, or every saved clip. */
export type AppScreen = "editor" | "shorts" | "clips";

export interface ViewState {
  /** App-level screen: the editor (default), Shorts or Clips. */
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
  /** The Shorts screen's drawer (under the player) is open, and which tab
   *  it shows. Chat is the default: it needs no setup. */
  shortsDrawerOpen: boolean;
  shortsDrawerTab: ShortsDrawerTab;
  /** Auditioning a clip: play these pieces back to back, then pause. Gaps
   *  between pieces are skipped. Cleared by any seek. */
  audition: Range[] | null;
  /** The audition is a scan candidate (Shorts screen). */
  auditionIsCandidate: boolean;
  /** The clip being assembled in the transcript ("Add to clip"): pieces in
   *  source order, not yet saved. */
  clipDraft: Range[];
  /** The saved 9:16 clip whose crop the preview is framing (drag to move). */
  cropClipId: string | null;
  /** Shorts screen: the scan candidate whose 9:16 crop the preview shows
   *  (the last one played), until "done". */
  cropCandidateId: string | null;
  /** A saved clip is open in the Editor and the view follows it (cut
   *  material hidden): the "show cuts" setting from before, to put back
   *  when it closes. Null when no clip is open. */
  clipEditShowCuts: boolean | null;
  /** Bumped to ask the timeline to fit its width again (a clip opened or
   *  closed: what plays changed completely). */
  fitNonce: number;
  /** The proposed cut the review bar is on (its number), shown in the
   *  transcript and timeline. */
  focusedCut: number | null;
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
  audition: null,
  auditionIsCandidate: false,
  clipDraft: [],
  cropClipId: null,
  cropCandidateId: null,
  clipEditShowCuts: null,
  fitNonce: 0,
  focusedCut: null,
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
      audition: null,
      clipDraft: [],
      cropClipId: null,
      cropCandidateId: null,
    };
  });
}
/** Show (or hide, with null) a saved clip's 9:16 crop over the preview. */
export const setCropClip = (cropClipId: string | null) => patch({ cropClipId });
/** A saved clip opened in the Editor: hide cut material (so the transcript
 *  and timeline show just the clip) and jump to its start. Not saved as a
 *  preference: `leaveClipEditView` puts the setting back. */
export function enterClipEditView(start: number | null): void {
  viewStore.setState((s) =>
    s.clipEditShowCuts !== null
      ? s
      : { ...s, clipEditShowCuts: s.showCuts, showCuts: false, fitNonce: s.fitNonce + 1 },
  );
  if (start !== null) seekTo(start);
}

/** The clip closed: the view as it was before it opened. */
export function leaveClipEditView(): void {
  viewStore.setState((s) =>
    s.clipEditShowCuts === null
      ? s
      : {
          ...s,
          showCuts: s.clipEditShowCuts,
          clipEditShowCuts: null,
          cropClipId: null,
          focusedCut: null,
          fitNonce: s.fitNonce + 1,
        },
  );
}

/** Show (or hide, with null) a scan candidate's 9:16 crop over the preview. */
export const setCropCandidate = (cropCandidateId: string | null) => patch({ cropCandidateId });
export const setScreen = (screen: AppScreen) => patch({ screen });
export const setPlayhead = (playhead: number) => patch({ playhead: Math.max(0, playhead) });

/** Seek: moves the playhead AND asks the video element to jump there. */
export function seekTo(t: number): void {
  viewStore.setState((s) => ({
    ...s,
    playhead: Math.max(0, t),
    seekNonce: s.seekNonce + 1,
    audition: null,
  }));
}

/** Play these pieces back to back once and stop — auditioning a clip (or,
 *  with `candidate`, a scan candidate on the Shorts screen: its id, whose
 *  9:16 crop the preview then shows). */
export function previewRanges(ranges: Range[], opts: { candidate?: string } = {}): void {
  const pieces = [...ranges].sort((a, b) => a.start - b.start);
  if (pieces.length === 0) return;
  viewStore.setState((s) => ({
    ...s,
    playhead: Math.max(0, pieces[0]!.start),
    seekNonce: s.seekNonce + 1,
    audition: pieces,
    auditionIsCandidate: !!opts.candidate,
    cropCandidateId: opts.candidate ?? s.cropCandidateId,
    playing: true,
  }));
}

/** The playback clocks call this as time advances: true = the audition is over. */
export function reachedAuditionEnd(t: number): boolean {
  const pieces = viewStore.state.audition;
  if (pieces === null) return false;
  const end = pieces[pieces.length - 1]!.end;
  if (t < end) return false;
  viewStore.setState((s) => ({ ...s, playhead: end, playing: false, audition: null }));
  return true;
}

/** The gaps an audition skips: everything between its pieces. */
export function auditionGaps(): Range[] {
  const pieces = viewStore.state.audition;
  if (!pieces) return [];
  const gaps: Range[] = [];
  for (let i = 1; i < pieces.length; i++) {
    if (pieces[i]!.start > pieces[i - 1]!.end) gaps.push({ start: pieces[i - 1]!.end, end: pieces[i]!.start });
  }
  return gaps;
}

/** Add a piece to the clip being assembled; overlapping pieces merge. */
export function addToClipDraft(piece: Range): void {
  viewStore.setState((s) => {
    const all = [...s.clipDraft, piece].sort((a, b) => a.start - b.start);
    const merged: Range[] = [];
    for (const r of all) {
      const last = merged[merged.length - 1];
      if (last && r.start <= last.end) last.end = Math.max(last.end, r.end);
      else merged.push({ ...r });
    }
    return { ...s, clipDraft: merged };
  });
}

/** Take back every draft piece overlapping `piece`. */
export function removeFromClipDraft(piece: Range): void {
  viewStore.setState((s) => ({
    ...s,
    clipDraft: s.clipDraft.filter((r) => r.end <= piece.start || r.start >= piece.end),
  }));
}

export const clearClipDraft = () => patch({ clipDraft: [] });

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
export const setFocusedCut = (focusedCut: number | null) => patch({ focusedCut });
export const togglePlaying = () => patch({ playing: !viewStore.state.playing });
export const setPlaybackRate = (playbackRate: number) => patch({ playbackRate });
export const setShowCuts = (showCuts: boolean) => {
  // Toggled by hand while a clip is open: that's the setting to keep after.
  patch(viewStore.state.clipEditShowCuts === null ? { showCuts } : { showCuts, clipEditShowCuts: showCuts });
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
export const toggleShortsDrawer = () => {
  patch({ shortsDrawerOpen: !viewStore.state.shortsDrawerOpen });
  savePrefs(viewStore.state);
};
/** Show a drawer tab, opening the drawer if it is closed. */
export const showShortsDrawerTab = (shortsDrawerTab: ShortsDrawerTab) => {
  patch({ shortsDrawerOpen: true, shortsDrawerTab });
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
