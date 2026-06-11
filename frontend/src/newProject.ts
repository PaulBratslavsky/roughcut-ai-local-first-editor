// Start a new project from anywhere in the app: native file dialog (Tauri)
// or a fresh demo project (browser/mock), then kick off transcription.

import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { isTauri } from "./ipc/api";
import { baseName, ingestFile } from "./ingest";

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
  // Resolve as soon as the project exists; transcription continues in the
  // background and the UI fills in via transcript-changed events.
  return await new Promise<string>((resolve, reject) => {
    ingestFile(name, path, { onCreated: resolve }).catch(reject);
  });
}
