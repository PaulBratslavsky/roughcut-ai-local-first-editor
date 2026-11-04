// TanStack Query bindings for the tool surface. Reads are cached and
// invalidated by `timeline-changed` / `transcript-changed` core events
// (wired up in useCoreEventInvalidation).

import { useEffect } from "react";
import {
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
} from "@tanstack/react-query";
import { callTool, onAppEvent } from "./api";
import type {
  DurationCutReceipt,
  EditResult,
  ExportResult,
  ExportTarget,
  ListProjectsResult,
  Media,
  Preferences,
  Project,
  RoughCutResult,
  CutSuggestion,
  SplitResult,
  Timeline,
  Transcript,
  UndoResult,
  MediaAssets,
  ClipBatchError,
  ClipCandidate,
  ClipExportResult,
  ClipFormat,
  SavedClipSummary,
  SourceRange,
} from "./types";

export const queryKeys = {
  projects: ["projects"] as const,
  project: (id: string) => ["project", id] as const,
  timeline: (id: string) => ["timeline", id] as const,
  transcript: (id: string) => ["transcript", id] as const,
  preferences: ["preferences"] as const,
};

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

export function useProjects() {
  return useQuery({
    queryKey: queryKeys.projects,
    queryFn: () => callTool<ListProjectsResult>("list_projects", {}),
  });
}

export function useProject(projectId: string | null) {
  return useQuery({
    queryKey: queryKeys.project(projectId ?? "none"),
    queryFn: () => callTool<Project>("open_project", { project_id: projectId }),
    enabled: projectId !== null,
  });
}

export function useMediaAssets(projectId: string | null) {
  return useQuery({
    queryKey: ["media-assets", projectId],
    queryFn: () => callTool<MediaAssets>("get_media_assets", { project_id: projectId }),
    enabled: !!projectId,
    // Cheap once the cache exists (file-existence checks); a refetch on
    // focus picks up assets that finished generating in the background.
    staleTime: 30_000,
    retry: false,
  });
}

export function useTimeline(projectId: string | null) {
  return useQuery({
    queryKey: queryKeys.timeline(projectId ?? "none"),
    queryFn: () => callTool<Timeline>("get_timeline", { project_id: projectId }),
    enabled: projectId !== null,
  });
}

export function useTranscript(projectId: string | null) {
  return useQuery({
    queryKey: queryKeys.transcript(projectId ?? "none"),
    queryFn: () => callTool<Transcript | null>("get_transcript", { project_id: projectId }),
    enabled: projectId !== null,
  });
}

export function usePreferences() {
  return useQuery({
    queryKey: queryKeys.preferences,
    queryFn: () => callTool<Preferences>("get_preferences", {}),
  });
}

// ---------------------------------------------------------------------------
// Event-driven invalidation
// ---------------------------------------------------------------------------

export function invalidateOnCoreEvents(queryClient: QueryClient): () => void {
  const offTimeline = onAppEvent("timeline-changed", () => {
    void queryClient.invalidateQueries({ queryKey: ["timeline"] });
    void queryClient.invalidateQueries({ queryKey: ["project"] });
    void queryClient.invalidateQueries({ queryKey: queryKeys.projects });
    // Suggestion mutations (accept/dismiss/accept-all/rough cut) all emit
    // timeline-changed — refresh the review list so an agent/MCP-driven
    // change doesn't leave the panel showing already-applied suggestions.
    void queryClient.invalidateQueries({ queryKey: ["suggestions"] });
    void queryClient.invalidateQueries({ queryKey: ["history"] });
  });
  const offTranscript = onAppEvent("transcript-changed", () => {
    void queryClient.invalidateQueries({ queryKey: ["transcript"] });
    // Derived media (peaks/thumbnails/playable remux) lands around the same
    // time as the transcript; refresh so the player picks up playback_path.
    void queryClient.invalidateQueries({ queryKey: ["media-assets"] });
  });
  // Projects created/duplicated/deleted by ANY caller (incl. MCP clients)
  // refresh the library dropdown live.
  const offProjects = onAppEvent("projects-changed", () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.projects });
  });
  const offAssets = onAppEvent("media-assets-changed", () => {
    void queryClient.invalidateQueries({ queryKey: ["media-assets"] });
  });
  // Saved clips change from the panel, the chat agent, or an MCP client alike.
  const offClips = onAppEvent("clips-changed", () => {
    void queryClient.invalidateQueries({ queryKey: ["clips"] });
    void queryClient.invalidateQueries({ queryKey: ["clip-candidates"] });
  });
  return () => {
    offTimeline();
    offTranscript();
    offProjects();
    offAssets();
    offClips();
  };
}

export function useCoreEventInvalidation(): void {
  const queryClient = useQueryClient();
  useEffect(() => invalidateOnCoreEvents(queryClient), [queryClient]);
}

// ---------------------------------------------------------------------------
// Mutations (editing tools)
// ---------------------------------------------------------------------------

/** Options for a long run whose outcome must survive its component: the
 *  key lets any screen see it is running (`useIsMutating`), and these
 *  callbacks — unlike ones passed to `mutate()` — still fire after the
 *  component that started the run has unmounted. */
export interface RunOptions<TArgs, TResult> {
  onSuccess?: (data: TResult, args: TArgs) => void;
  onError?: (error: unknown, args: TArgs) => void;
}

function useEditMutation<TArgs extends Record<string, unknown>, TResult>(
  toolName: string,
  invalidate: (
    | "timeline" | "transcript" | "project" | "projects" | "preferences" | "suggestions" | "history"
    | "clips" | "clip-candidates"
  )[],
  run?: RunOptions<TArgs, TResult> & { mutationKey: readonly unknown[] },
) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationKey: run?.mutationKey,
    mutationFn: (args: TArgs) => callTool<TResult>(toolName, args),
    onSuccess: (data: TResult, args: TArgs) => {
      for (const key of invalidate) {
        void queryClient.invalidateQueries({ queryKey: [key] });
      }
      run?.onSuccess?.(data, args);
    },
    onError: (error: unknown, args: TArgs) => run?.onError?.(error, args),
  });
}

export interface CutByTranscriptArgs extends Record<string, unknown> {
  project_id: string;
  segment_ids: string[];
}

export function useCutByTranscript() {
  return useEditMutation<CutByTranscriptArgs, EditResult>("cut_by_transcript", ["timeline", "project"]);
}

export function useRestoreByTranscript() {
  return useEditMutation<CutByTranscriptArgs, EditResult>("restore_by_transcript", ["timeline", "project"]);
}

export interface RangeArgs extends Record<string, unknown> {
  project_id: string;
  start: number;
  end: number;
}

export function useCutRange() {
  return useEditMutation<RangeArgs, EditResult>("cut_range", ["timeline", "project"]);
}

export function useRestoreRange() {
  return useEditMutation<RangeArgs, EditResult>("restore_range", ["timeline", "project"]);
}

export interface TrimClipArgs extends Record<string, unknown> {
  project_id: string;
  clip_id: string;
  new_source_in: number;
  new_source_out: number;
}

export function useTrimClip() {
  return useEditMutation<TrimClipArgs, EditResult>("trim_clip", ["timeline", "project"]);
}

export interface SplitClipArgs extends Record<string, unknown> {
  project_id: string;
  clip_id: string;
  at_time: number;
}

export function useSplitClip() {
  return useEditMutation<SplitClipArgs, SplitResult>("split_clip", ["timeline", "project"]);
}

export interface SetGlobalPaddingArgs extends Record<string, unknown> {
  project_id: string;
  start_s: number;
  end_s: number;
  linked: boolean;
}

export function useSetGlobalPadding() {
  return useEditMutation<SetGlobalPaddingArgs, EditResult>("set_global_padding", ["timeline", "project"]);
}

export interface PlanDurationCutArgs extends Record<string, unknown> {
  project_id: string;
  target_duration_s: number;
  apply?: boolean;
}

export interface RestoreProjectArgs extends Record<string, unknown> {
  project_id: string;
}

/** Bring a trashed project back (see ListProjectsResult.trash). */
export function useRestoreProject() {
  return useEditMutation<RestoreProjectArgs, Project>("restore_project", ["projects", "project", "timeline", "transcript"]);
}

/** plan_duration_cut with apply=true: one undoable batch cut to a target. */
export function useApplyDurationCut() {
  return useEditMutation<PlanDurationCutArgs, DurationCutReceipt>("plan_duration_cut", [
    "timeline",
    "project",
  ]);
}

export interface RoughCutArgs extends Record<string, unknown> {
  project_id: string;
  aggressiveness?: "natural" | "aggressive";
}

export function useGenerateRoughCut() {
  return useEditMutation<RoughCutArgs, RoughCutResult>("generate_rough_cut", ["timeline", "project", "suggestions"]);
}

import type { History } from "./generated/History";

export function useHistory(projectId: string | null) {
  return useQuery({
    queryKey: ["history", projectId ?? "none"],
    queryFn: () => callTool<History>("get_history", { project_id: projectId }),
    enabled: projectId !== null,
  });
}

export function useJumpTo() {
  return useEditMutation<{ project_id: string; action_id: string | null }, unknown>(
    "jump_to",
    ["timeline", "project", "history", "suggestions"],
  );
}

export function useSuggestions(projectId: string | null) {
  return useQuery({
    queryKey: ["suggestions", projectId ?? "none"],
    queryFn: () => callTool<{ suggestions: CutSuggestion[] }>("get_suggestions", { project_id: projectId }),
    enabled: projectId !== null,
    select: (r) => r.suggestions,
  });
}

export function useAcceptSuggestion() {
  return useEditMutation<{ project_id: string; suggestion_id: string }, unknown>(
    "accept_suggestion",
    ["timeline", "project", "suggestions"],
  );
}

export function useAcceptAllSuggestions() {
  return useEditMutation<{ project_id: string }, unknown>(
    "accept_all_suggestions",
    ["timeline", "project", "suggestions"],
  );
}

export function useDismissSuggestion() {
  return useEditMutation<{ project_id: string; suggestion_id: string }, unknown>(
    "dismiss_suggestion",
    ["project", "suggestions"],
  );
}

export interface ApplyInstructionArgs extends Record<string, unknown> {
  project_id: string;
  instruction: string;
  /** Recent chat turns (oldest first) so follow-ups like "apply the edits" resolve. */
  history?: { role: "user" | "agent"; text: string }[];
}


export function useUndo() {
  return useEditMutation<{ project_id: string }, UndoResult>("undo", ["timeline", "transcript", "project"]);
}

export function useRedo() {
  return useEditMutation<{ project_id: string }, UndoResult>("redo", ["timeline", "transcript", "project"]);
}

export interface ExportArgs extends Record<string, unknown> {
  project_id: string;
  target: ExportTarget;
  out_path: string;
}

export function useExport() {
  return useEditMutation<ExportArgs, ExportResult>("export", []);
}

export function useSetPreferences() {
  return useEditMutation<{ preferences: Partial<Preferences> }, Preferences>("set_preferences", ["preferences"]);
}

export function useCreateProject() {
  return useEditMutation<{ name: string; file_path?: string }, Project>("create_project", ["projects", "project"]);
}

export function useImportMedia() {
  return useEditMutation<{ file_path: string }, Media>("import_media", ["project"]);
}

export function useTranscribe() {
  return useEditMutation<{ project_id: string; language?: string }, Transcript>("transcribe", [
    "transcript",
    "timeline",
    "project",
    "projects",
  ]);
}

// ---------------------------------------------------------------------------
// Saved clips (16:9 cut-downs and 9:16 shorts)
// ---------------------------------------------------------------------------

export function useClips(projectId: string | null) {
  return useQuery({
    queryKey: ["clips", projectId ?? "none"],
    queryFn: () => callTool<{ clips: SavedClipSummary[] }>("list_clips", { project_id: projectId }),
    enabled: projectId !== null,
    select: (r) => r.clips,
  });
}

export function useClipCandidates(projectId: string | null) {
  return useQuery({
    queryKey: ["clip-candidates", projectId ?? "none"],
    queryFn: () =>
      callTool<{ candidates: ClipCandidate[] }>("get_clip_candidates", { project_id: projectId }),
    enabled: projectId !== null,
    select: (r) => r.candidates,
  });
}

export interface FindClipsArgs extends Record<string, unknown> {
  project_id: string;
  /** How many to return, 1–60 (default 8). */
  count?: number;
  min_s?: number;
  max_s?: number;
  /** Only moments about this topic. */
  theme?: string;
  /** Judge moments from every part of the video, beat by beat (slower). */
  scan?: boolean;
  /** Drop moments the model scored below this (unjudged ones stay). */
  min_score?: number;
  /** Also propose shorts made of several pieces (needs the model). */
  multi_moment?: boolean;
}

export interface FindClipsResult {
  candidates: ClipCandidate[];
  /** "llm" (scored by a model: see each candidate's `scorer`) | "heuristic" (rule-based). */
  method: string;
  theme: string | null;
}

/** Mutation keys of the per-project clip runs (scan, batch make, batch
 *  export): `useIsMutating({ mutationKey })` says one is running wherever it
 *  was started (Clips tab, Shorts screen) and after a remount. */
export const clipRunKeys = {
  find: (projectId: string) => ["find_clips", projectId] as const,
  make: (projectId: string) => ["create_clips", projectId] as const,
  export: (projectId: string) => ["export_clips", projectId] as const,
};

export function useFindClips(projectId: string, run?: RunOptions<FindClipsArgs, FindClipsResult>) {
  return useEditMutation<FindClipsArgs, FindClipsResult>("find_clips", ["clip-candidates"], {
    ...run,
    mutationKey: clipRunKeys.find(projectId),
  });
}

export interface CreateClipArgs extends Record<string, unknown> {
  project_id: string;
  rank?: number;
  candidate_id?: string;
  segment_ids?: string[];
  ranges?: SourceRange[];
  title?: string;
  format?: ClipFormat;
}

export function useCreateClip() {
  return useEditMutation<CreateClipArgs, { clip: SavedClipSummary }>("create_clip", [
    "clips",
    "clip-candidates",
  ]);
}

export interface CreateClipsArgs extends Record<string, unknown> {
  project_id: string;
  candidate_ids: string[];
  /** Default `vertical` (a 9:16 short). */
  format?: ClipFormat;
}

/** Make several candidates at once; vertical ones come back framed on the
 *  speaker. Candidates that no longer validate stay listed, in `errors`. */
export interface CreateClipsResult {
  clips: SavedClipSummary[];
  errors: ClipBatchError[];
}

export function useCreateClips(projectId: string, run?: RunOptions<CreateClipsArgs, CreateClipsResult>) {
  return useEditMutation<CreateClipsArgs, CreateClipsResult>("create_clips", ["clips", "clip-candidates"], {
    ...run,
    mutationKey: clipRunKeys.make(projectId),
  });
}

export function useUpdateClip() {
  return useEditMutation<
    { project_id: string; clip_id: string; title?: string; format?: ClipFormat; crop_x?: number },
    { clip: SavedClipSummary }
  >("update_clip", ["clips"]);
}

/** What a face pass found: `crop_x` null = no confident face, crop unchanged. */
export interface FaceCropResult {
  clip: SavedClipSummary;
  crop_x: number | null;
  frames_sampled: number;
  frames_with_face: number;
}

export function useCenterClipOnFace() {
  return useEditMutation<{ project_id: string; clip_id: string }, FaceCropResult>("center_clip_on_face", [
    "clips",
  ]);
}

export function useDeleteClip() {
  return useEditMutation<{ project_id: string; clip_id: string }, unknown>("delete_clip", ["clips"]);
}

export function useExportClip() {
  return useEditMutation<
    { project_id: string; clip_id: string; target: ExportTarget; out_path: string },
    ExportResult
  >("export_clip", []);
}

export interface ExportClipsArgs extends Record<string, unknown> {
  project_id: string;
  clip_ids: string[];
  /** Folder to write into (created if missing; `~` expands). */
  out_dir: string;
  /** Default `mp4`. */
  target?: ExportTarget;
}

/** One file per clip into one folder, `NN-<slug>.<ext>`; results follow
 *  `clip_ids` order, each with a path or an error. */
export interface ExportClipsResult {
  results: ClipExportResult[];
}

export function useExportClips(projectId: string, run?: RunOptions<ExportClipsArgs, ExportClipsResult>) {
  return useEditMutation<ExportClipsArgs, ExportClipsResult>("export_clips", [], {
    ...run,
    mutationKey: clipRunKeys.export(projectId),
  });
}
