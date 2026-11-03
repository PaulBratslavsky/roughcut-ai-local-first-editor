// THE ingest seam: create → attach media → transcribe, one implementation
// for every entry point (drop zone, file dialog, demo button). EmptyState and
// newProject each had a copy; EmptyState's double-probed the file
// (create_project(file_path) probes, then import_media probed AGAIN and
// replaced the media) and newProject's swallowed transcription errors.
// A transcription error is kept on the project (transcribeStore), because
// creating the project switches the app to it and the caller is often gone.

import { callTool } from "./ipc/api";
import type { Project } from "./ipc/types";
import { errText } from "./panels/clipUtil";
import { setTranscribeError } from "./state/transcribeStore";

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
 *  wait can ignore the promise after the id is known via onCreated). A
 *  transcription failure still rejects, and is also kept on the project for
 *  its transcript panel to show. */
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
  setTranscribeError(project.id, null);
  try {
    await callTool("transcribe", { project_id: project.id });
  } catch (err) {
    setTranscribeError(project.id, `Transcription failed: ${errText(err)}`);
    throw err;
  }
  return project.id;
}
