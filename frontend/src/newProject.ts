// Start a new project from anywhere in the app: native file dialog (Tauri)
// or a fresh demo project (browser/mock), then kick off transcription.

import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { callTool, isTauri } from "./ipc/api";
import type { Project } from "./ipc/types";

function baseName(path: string): string {
  const last = path.split(/[\\/]/).pop() ?? path;
  return last.replace(/\.[^.]+$/, "") || "Untitled project";
}

/** Returns the new project id, or null if the user cancelled the dialog. */
export async function newProjectFromDialog(): Promise<string | null> {
  let name: string;
  let path: string;
  if (isTauri) {
    const picked = await openFileDialog({
      multiple: false,
      directory: false,
      filters: [{ name: "Video", extensions: ["mp4", "mov", "mkv", "avi", "m4v", "webm"] }],
    });
    if (typeof picked !== "string") return null;
    path = picked;
    name = baseName(picked);
  } else {
    name = `demo-${Date.now().toString(36)}`;
    path = "/demo/talking-head.mp4";
  }
  const project = await callTool<Project>("create_project", { name, file_path: path });
  // Fire-and-forget: the transcript panel fills in via transcript-changed.
  void callTool("transcribe", { project_id: project.id }).catch(() => {});
  return project.id;
}
