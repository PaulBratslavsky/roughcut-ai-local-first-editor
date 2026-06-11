// Single IPC entry point. Inside Tauri this proxies to the Rust core's
// generic `call_tool` command; in a plain browser it falls back to the
// in-memory mock so the whole app is drivable for testing.

import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { mockCallTool, mockOn } from "./mockApi";
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

/** Subscribe to a core event. Returns an unsubscribe function. */
export function onAppEvent<T>(event: AppEventName, handler: (payload: T) => void): () => void {
  if (isTauri) {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<T>(event, (e) => handler(e.payload)).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }
  return mockOn(event, handler as (payload: unknown) => void);
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
    inference_reachable: true,
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

/** Reveal a file in Finder (desktop only). */
export async function revealPath(path: string): Promise<void> {
  if (!isTauri) return;
  return invoke("reveal_path", { path });
}

/** Resolve a local media file path to something a <video> tag can play. */
export function mediaSrc(filePath: string): string | null {
  if (!isTauri) return null;
  // A bare file name (no directory) can't be resolved by the asset protocol —
  // happens for projects imported in demo/browser flows. Show the placeholder.
  if (!filePath.includes("/") && !filePath.includes("\\")) return null;
  return convertFileSrc(filePath);
}
