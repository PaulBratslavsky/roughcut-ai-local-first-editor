// Shorts screen state that must outlive the screen (TanStack Store, like
// viewStore). A scan, a batch make or a batch export runs for minutes, and the
// user may switch to the editor or another project meanwhile; what they
// ticked and how the last run went must still be there when they come back.
// Per project, ephemeral (not persisted). Written by the run hooks' own
// callbacks, which fire even after the screen unmounts (ShortsScreen), and by
// one always-on progress listener mounted in App.

import { useEffect } from "react";
import { Store } from "@tanstack/react-store";
import { onAppEvent } from "../ipc/api";
import type { ClipExportResult, ProgressEvent } from "../ipc/types";

export type ShortsTask = "find_clips" | "make_clips" | "export_clips";
const TASKS: readonly string[] = ["find_clips", "make_clips", "export_clips"];

export interface ShortsState {
  /** Candidate ids ticked to make. */
  picked: string[];
  /** Saved clip ids ticked to export. */
  ticked: string[];
  /** The latest progress line of each run (null once it reports done). */
  progress: Partial<Record<ShortsTask, ProgressEvent | null>>;
  /** When this screen started the running scan (for "scanning… 12s"). */
  scanStartedAt: number | null;
  /** How many candidates the last scan from this screen found. */
  lastScanFound: number | null;
  error: string | null;
  /** Titles of the candidates being made, for naming the ones that fail. */
  makeTitles: Record<string, string>;
  made: { count: number; failed: { title: string; message: string }[] } | null;
  /** The running export's folder and clip titles. */
  exportJob: { dir: string; titles: Record<string, string> } | null;
  exported: { dir: string; results: ClipExportResult[]; titles: Record<string, string> } | null;
}

export const EMPTY_SHORTS: ShortsState = {
  picked: [],
  ticked: [],
  progress: {},
  scanStartedAt: null,
  lastScanFound: null,
  error: null,
  makeTitles: {},
  made: null,
  exportJob: null,
  exported: null,
};

export const shortsStore = new Store<Record<string, ShortsState>>({});

export const shortsOf = (projectId: string): ShortsState => shortsStore.state[projectId] ?? EMPTY_SHORTS;

export function patchShorts(
  projectId: string,
  p: Partial<ShortsState> | ((s: ShortsState) => Partial<ShortsState>),
): void {
  shortsStore.setState((all) => {
    const cur = all[projectId] ?? EMPTY_SHORTS;
    return { ...all, [projectId]: { ...cur, ...(typeof p === "function" ? p(cur) : p) } };
  });
}

const toggled = (ids: string[], id: string, on: boolean): string[] =>
  on ? (ids.includes(id) ? ids : [...ids, id]) : ids.filter((x) => x !== id);

export const setPicked = (projectId: string, picked: string[]) => patchShorts(projectId, { picked });
export const togglePicked = (projectId: string, id: string, on: boolean) =>
  patchShorts(projectId, (s) => ({ picked: toggled(s.picked, id, on) }));
export const setTicked = (projectId: string, ticked: string[]) => patchShorts(projectId, { ticked });
export const toggleTicked = (projectId: string, id: string, on: boolean) =>
  patchShorts(projectId, (s) => ({ ticked: toggled(s.ticked, id, on) }));

/** Set (or clear, with null) a run's progress line. */
export const setShortsProgress = (projectId: string, task: ShortsTask, event: ProgressEvent | null) =>
  patchShorts(projectId, (s) => ({ progress: { ...s.progress, [task]: event } }));

/** Mount once (App): keeps every project's latest scan / make / export
 *  progress line, whichever screen is showing. A run's final event clears
 *  it, so the next run never opens on the last one's "done". */
export function useShortsProgressListener(): void {
  useEffect(
    () =>
      onAppEvent<ProgressEvent>("progress", (p) => {
        if (!p.project_id || !TASKS.includes(p.task)) return;
        setShortsProgress(p.project_id, p.task as ShortsTask, p.fraction >= 1 ? null : p);
      }),
    [],
  );
}
