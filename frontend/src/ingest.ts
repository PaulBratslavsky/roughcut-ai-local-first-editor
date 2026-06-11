// THE ingest seam: create → attach media → transcribe, one implementation
// for every entry point (drop zone, file dialog, demo button — and the
// recorder, whose output lands through this exact path). EmptyState and
// newProject each had a copy; EmptyState's double-probed the file
// (create_project(file_path) probes, then import_media probed AGAIN and
// replaced the media) and newProject's swallowed transcription errors.

import { callTool } from "./ipc/api";
import type { Project } from "./ipc/types";

export function baseName(path: string): string {
  const last = path.split(/[\\/]/).pop() ?? path;
  return last.replace(/\.[^.]+$/, "") || "Untitled project";
}

export interface IngestCallbacks {
  /** Coarse phase labels; fine-grained progress arrives via `progress` events. */
  onPhase?: (message: string) => void;
}

/** Create a project around `filePath` and start transcription. Resolves to
 *  the project id once transcription COMPLETES (callers that don't want to
 *  wait can ignore the promise after the id is known via onCreated). */
export async function ingestFile(
  name: string,
  filePath: string,
  cb: IngestCallbacks & { onCreated?: (projectId: string) => void } = {},
): Promise<string> {
  cb.onPhase?.("Creating project…");
  // create_project(file_path) probes and attaches in one step — do NOT call
  // import_media after this; that probes a second time and replaces media.
  const project = await callTool<Project>("create_project", {
    name,
    file_path: filePath,
  });
  cb.onCreated?.(project.id);
  cb.onPhase?.("Starting transcription…");
  await callTool("transcribe", { project_id: project.id });
  return project.id;
}
