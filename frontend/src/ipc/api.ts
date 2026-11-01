// Single IPC entry point. Inside Tauri this proxies to the Rust core's
// generic `call_tool` command; in a plain browser it falls back to the
// in-memory mock so the whole app is drivable for testing.

import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  mockCallTool,
  mockCheckHostedKey,
  mockModelStatus,
  mockOn,
  mockSetHostedKey,
  mockSetHostedModel,
  mockSetModelMode,
  mockSwitch,
} from "./mockApi";
import type { AppEventName, WhisperTier, SetupStatus } from "./types";

export const isTauri: boolean =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function callTool<T = unknown>(
  name: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  if (isTauri) {
    return invoke<T>("call_tool", { name, args });
  }
  return mockCallTool(name, args) as Promise<T>;
}

/** Subscribe to a core event. Returns an unsubscribe function. `onReady`
 *  runs once the listener is live (Tauri registers it asynchronously): read
 *  initial state there, so no event can fall between the read and the
 *  subscription. */
export function onAppEvent<T>(
  event: AppEventName,
  handler: (payload: T) => void,
  onReady?: () => void,
): () => void {
  if (isTauri) {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<T>(event, (e) => handler(e.payload)).then((fn) => {
      if (cancelled) fn();
      else {
        unlisten = fn;
        onReady?.();
      }
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }
  const off = mockOn(event, handler as (payload: unknown) => void);
  onReady?.();
  return off;
}

/** Open one of the app's known sites (Anthropic Console, Ollama, DaVinci
 *  Resolve) in the system browser: the desktop webview doesn't follow
 *  target=_blank links. The shell allow-lists the sites. */
export async function openExternal(url: string): Promise<void> {
  if (isTauri) return invoke("open_external", { url });
  window.open(url, "_blank", "noopener,noreferrer");
}

/** True when the backend is running on fixture adapters. In a plain browser
 *  everything is the mock, which is its own kind of demo. */
export async function getDemoMode(): Promise<boolean> {
  if (isTauri) return invoke<boolean>("demo_mode");
  return true;
}

/** First-run toolchain status. The browser mock reports a complete setup so
 *  the setup card never shows outside the real app. */
export async function getSetupStatus(): Promise<SetupStatus> {
  if (isTauri) return invoke<SetupStatus>("setup_status");
  return {
    ffmpeg: true,
    ffmpeg_path: "(browser mock)",
    whisper_model: "(browser mock)",
    whisper_native: false,
    whisper_cli: false,
    transcription_ready: true,
    models_dir: "",
    tiers: [
      { id: "accurate", file: "ggml-large-v3-turbo-q5_0.bin", approx_mb: 547, downloaded: true, recommended: true },
      { id: "compact", file: "ggml-small-q5_1.bin", approx_mb: 190, downloaded: false, recommended: false },
    ],
    ram_gb: 24,
    inference_reachable: !mockSwitch("nomodel"),
    inference_endpoint: "http://localhost:11434/v1",
    inference_model: "(browser mock)",
    embedding_model: "nomic-embed-text",
    runtime: {
      ollama_cli: true,
      models: ["(browser mock)"],
      chat_model_present: true,
      embedding_model_present: true,
      llama_server_installed: false,
      managed_running: false,
      ggufs: [],
    },
    resolve: {
      app_installed: false,
      plugin_installed: false,
      plugin_outdated: false,
      scripts_dir: "(browser mock)",
    },
    demo: true,
    demo_reason: "browser mock",
  };
}

// ---- The LOCAL / HOSTED switch ----
// Shell commands, not registry tools (ADR-0004): only the user, in the app,
// turns Hosted on or handles the key. Errors reject with
// `{ error: { code, message } }` (codes include hosted_no_key,
// hosted_not_consented, hosted_key_rejected, invalid_argument).
import type { HostedKeyResult, KeyCheck, ModelMode, ModelStatus } from "./types";

/** In memory: no network, no key-store read. */
export async function getModelStatus(): Promise<ModelStatus> {
  if (isTauri) return invoke<ModelStatus>("model_status");
  return mockModelStatus();
}

/** Hosted needs a stored key, and `consent: true` the first time. */
export async function setModelMode(mode: ModelMode, consent: boolean): Promise<ModelStatus> {
  if (isTauri) return invoke<ModelStatus>("set_model_mode", { mode, consent });
  return mockSetModelMode(mode, consent);
}

/** One of claude-opus-5-5 | claude-sonnet-5-5 | claude-haiku-5-5. */
export async function setHostedModel(model: string): Promise<ModelStatus> {
  if (isTauri) return invoke<ModelStatus>("set_hosted_model", { model });
  return mockSetHostedModel(model);
}

/** Save (checked first, no tokens spent) or forget (null; also switches to
 *  Local) the Anthropic API key. */
export async function setHostedKey(key: string | null): Promise<HostedKeyResult> {
  if (isTauri) return invoke<HostedKeyResult>("set_hosted_key", { key });
  return mockSetHostedKey(key);
}

/** Check the stored key against Anthropic again (Setup's "Test"). */
export async function checkHostedKey(): Promise<KeyCheck> {
  if (isTauri) return invoke<KeyCheck>("check_hosted_key");
  return mockCheckHostedKey();
}

/** Download a whisper model; progress arrives via `progress` events with
 *  task "model_download". Resolves to the installed model path. */
export async function downloadWhisperModel(tier: WhisperTier): Promise<string> {
  if (!isTauri) throw new Error("model download is only available in the desktop app");
  return invoke<string>("download_whisper_model", { tier });
}

/** Install/refresh the DaVinci Resolve script (desktop only). */
export async function installResolvePlugin(): Promise<string> {
  return invoke<string>("install_resolve_plugin");
}

import type { ResolveStatus } from "./types";

/** Cheap filesystem-only Resolve detection (no server probes). */
export async function getResolveStatus(): Promise<ResolveStatus> {
  if (!isTauri) {
    return { app_installed: false, plugin_installed: false, plugin_outdated: false, scripts_dir: "" };
  }
  return invoke<ResolveStatus>("resolve_status");
}

import type { SendToResolveOutcome } from "./types";

/** Export the current cut as Resolve XML and push it into the running
 *  Resolve. Resolves with ok=false (never rejects) when Resolve can't take
 *  it, so the caller can reveal the XML for a manual drag. */
export async function sendToResolve(projectId: string): Promise<SendToResolveOutcome> {
  return invoke<SendToResolveOutcome>("send_to_resolve", { projectId });
}

export async function ollamaPullModel(model: string): Promise<void> {
  return invoke("ollama_pull_model", { model });
}

export async function installLlamaServer(): Promise<string> {
  return invoke<string>("install_llama_server");
}

export async function downloadGguf(url: string): Promise<string> {
  return invoke<string>("download_gguf", { url });
}

export async function startManagedLlm(ggufPath: string): Promise<string> {
  return invoke<string>("start_managed_llm", { ggufPath });
}

// ---- Recording (desktop only; shapes generated from core) ----
import type { CaptureDevices } from "./generated/CaptureDevices";
import type { RecordingStatus } from "./generated/RecordingStatus";
export type { CaptureDevices, RecordingStatus };

export async function recordDevices(): Promise<CaptureDevices> {
  if (!isTauri) return { cameras: [], screens: [], microphones: [] };
  return invoke<CaptureDevices>("record_devices");
}

/** Starts capture; resolves with the output path. First runs trigger the
 *  macOS camera+mic (or Screen Recording) permission prompts. */
export async function recordStart(
  video: number,
  microphone: number,
  screen: boolean,
  dualScreen?: number,
): Promise<string> {
  return invoke<string>("record_start", { video, microphone, screen, dualScreen: dualScreen ?? null });
}

import type { RecordingOutcome } from "./generated/RecordingOutcome";
export type { RecordingOutcome };

/** Attach a dual-capture screen file to a project (probed; persisted). */
export async function attachScreenMedia(projectId: string, filePath: string): Promise<void> {
  return invoke("attach_screen_media", { projectId, filePath });
}

export async function recordStatus(): Promise<RecordingStatus> {
  if (!isTauri) return { recording: false, paused: false, take: 0, elapsed_s: 0, total_s: 0, out_path: null };
  return invoke<RecordingStatus>("record_status");
}

/** Finish the session: finalizes + stitches every take into one file per
 *  stream (camera always; screen too for dual sessions). */
export async function recordStop(): Promise<RecordingOutcome> {
  return invoke<RecordingOutcome>("record_stop");
}

export async function recordPause(): Promise<RecordingStatus> {
  return invoke<RecordingStatus>("record_pause");
}

export async function recordResume(): Promise<RecordingStatus> {
  return invoke<RecordingStatus>("record_resume");
}

import type { RecordingFile } from "./generated/RecordingFile";
export type { RecordingFile };

/** The recordings library (~/Movies/RoughCut), newest first. */
export async function listRecordings(): Promise<RecordingFile[]> {
  if (!isTauri) return [];
  return invoke<RecordingFile[]>("list_recordings");
}

/** Screen Recording TCC status (true off-macOS). */
export async function screenPermissionStatus(): Promise<boolean> {
  if (!isTauri) return true;
  return invoke<boolean>("screen_permission_status");
}

/** Trigger the ONE-TIME macOS prompt; returns the (possibly new) status. */
export async function requestScreenPermission(): Promise<boolean> {
  return invoke<boolean>("request_screen_permission");
}

/** Open System Settings at a privacy pane: "screen" | "camera" | "microphone". */
export async function openPrivacySettings(pane: string): Promise<void> {
  return invoke("open_privacy_settings", { pane });
}

/** Delete a recording from the library (the file itself). */
export async function deleteRecording(path: string): Promise<void> {
  return invoke("delete_recording", { path });
}

/** Stitch library recordings (same codec/resolution) into one new file. */
export async function combineRecordings(paths: string[]): Promise<string> {
  return invoke<string>("combine_recordings", { paths });
}

/** Reveal a file in Finder (desktop only). */
export async function revealPath(path: string): Promise<void> {
  if (!isTauri) return;
  return invoke("reveal_path", { path });
}

// ---- In-app terminal (desktop only) ----

/** Open a PTY session bound to `id`. Output arrives via `terminal-output`
 *  events; the shell exiting emits `terminal-exit`. Desktop-only — there is no
 *  PTY in the browser harness. */
export async function ptySpawn(id: string, cols: number, rows: number): Promise<void> {
  if (!isTauri) return;
  return invoke("pty_spawn", { id, cols, rows });
}

/** Forward keystrokes (xterm `onData`) to the shell. */
export async function ptyWrite(id: string, data: string): Promise<void> {
  if (!isTauri) return;
  return invoke("pty_write", { id, data });
}

/** Tell the PTY the new grid size (cols/rows, not pixels). */
export async function ptyResize(id: string, cols: number, rows: number): Promise<void> {
  if (!isTauri) return;
  return invoke("pty_resize", { id, cols, rows });
}

/** Kill the session's shell (also happens automatically on app exit). */
export async function ptyKill(id: string): Promise<void> {
  if (!isTauri) return;
  return invoke("pty_kill", { id });
}

/** Resolve a local media file path to something a <video> tag can play. */
export function mediaSrc(filePath: string): string | null {
  // Browser harness: http sources play directly (lets Playwright exercise
  // the REAL <video> engine paths the mock clock can't).
  if (filePath.startsWith("http")) return filePath;
  if (!isTauri) return null;
  // A bare file name (no directory) can't be resolved by the asset protocol —
  // happens for projects imported in demo/browser flows. Show the placeholder.
  if (!filePath.includes("/") && !filePath.includes("\\")) return null;
  return convertFileSrc(filePath);
}
