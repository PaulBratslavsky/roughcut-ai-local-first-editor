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
  InstructionResult,
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
  return () => {
    offTimeline();
    offTranscript();
    offProjects();
    offAssets();
  };
}

export function useCoreEventInvalidation(): void {
  const queryClient = useQueryClient();
  useEffect(() => invalidateOnCoreEvents(queryClient), [queryClient]);
}

// ---------------------------------------------------------------------------
// Mutations (editing tools)
// ---------------------------------------------------------------------------

function useEditMutation<TArgs extends Record<string, unknown>, TResult>(
  toolName: string,
  invalidate: ("timeline" | "transcript" | "project" | "projects" | "preferences" | "suggestions")[],
) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (args: TArgs) => callTool<TResult>(toolName, args),
    onSuccess: () => {
      for (const key of invalidate) {
        void queryClient.invalidateQueries({ queryKey: [key] });
      }
    },
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

export function useApplyInstruction() {
  return useEditMutation<ApplyInstructionArgs, InstructionResult>("apply_instruction", [
    "timeline",
    "transcript",
    "project",
  ]);
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
