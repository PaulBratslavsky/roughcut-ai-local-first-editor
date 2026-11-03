// The LOCAL / HOSTED switch as the app shows it: the top-bar chip, the line
// on the import screen, and Setup's Models row. The core owns the state
// (model_status, a shell command — in memory, no network); this store holds
// the last copy: read once the `model-route` listener is live, re-read on
// every `model-route` and by Setup's poll (the event only fires when where
// runs go changes, not when `last_error` does), and replaced by every switch
// command's result. Replies are numbered so an older read never overwrites a
// newer status.

import { useEffect } from "react";
import { Store, useStore } from "@tanstack/react-store";
import { ask } from "@tauri-apps/plugin-dialog";
import { getModelStatus, isTauri, onAppEvent, setModelMode } from "../ipc/api";
import type { ModelMode, ModelRouteEvent, ModelStatus } from "../ipc/types";

/** What Hosted sends, shown once before it is first turned on (the user's
 *  consent is recorded by `set_model_mode { consent: true }`). */
export const HOSTED_CONSENT_TEXT =
  "AI steps will send text to Anthropic: transcripts, your chat messages, and the app's prompts and results (which can include file names). Video, audio and frames stay on this machine. After each transcription, the transcript clean-up runs on Anthropic automatically. Runs started by Claude Code or other MCP clients use Hosted too.";

/** The core's HOSTED_MODELS (core/src/model.rs). */
export const HOSTED_MODEL_CHOICES: { id: string; name: string }[] = [
  { id: "claude-opus-5-5", name: "Claude Opus 5.5" },
  { id: "claude-sonnet-5-5", name: "Claude Sonnet 5.5" },
  { id: "claude-haiku-5-5", name: "Claude Haiku 5.5" },
];

/** Display names of the hosted models; any other scorer is a local model. */
export const HOSTED_MODEL_NAMES = new Set(HOSTED_MODEL_CHOICES.map((m) => m.name));

export const modelStore = new Store<{ status: ModelStatus | null }>({ status: null });

export const useModelStatus = (): ModelStatus | null => useStore(modelStore, (s) => s.status);

/** Each read or command result takes a number; only the newest one lands. */
let latest = 0;

/** Take a command's returned status as the current one (and drop any read
 *  that started before it). */
export function applyModelStatus(status: ModelStatus): void {
  latest += 1;
  modelStore.setState(() => ({ status }));
}

/** Re-read model_status (in memory in the core: cheap). */
export async function refreshModelStatus(): Promise<ModelStatus | null> {
  const mine = ++latest;
  try {
    const status = await getModelStatus();
    if (mine === latest) modelStore.setState(() => ({ status }));
  } catch {
    /* keep what we have */
  }
  return modelStore.state.status;
}

/** Mount once (App): follow `model-route`, and read the status once the
 *  listener is live, so no change can fall between the two. An event only
 *  says "re-read": the core's reply is the truth, so a stale event (for a
 *  mode the user has left) does no harm. */
export function useModelStatusSync(): void {
  useEffect(
    () =>
      onAppEvent<ModelRouteEvent>(
        "model-route",
        () => void refreshModelStatus(),
        () => void refreshModelStatus(),
      ),
    [],
  );
}

/** "Opus 5.5" from "Claude Opus 5.5". */
export const shortModelName = (s: ModelStatus) => s.hosted_model_name.replace(/^Claude\s+/, "");

const FALLBACK_REASON: Record<string, string> = {
  offline: "offline",
  rate_limited: "rate limited",
  server_error: "Anthropic unavailable",
};

export type ChipTone = "local" | "hosted" | "fallback" | "error";

/** The chip: LOCAL · HOSTED · Opus 5.5 · HOSTED → LOCAL (offline) ·
 *  HOSTED · key rejected · HOSTED · no key. `setup`: a click opens Setup's
 *  Models row (no key stored, or a rejected key) instead of switching. */
export function modelChip(s: ModelStatus): { label: string; tone: ChipTone; title: string; setup: boolean } {
  const chip = ((): { label: string; tone: ChipTone; title: string; setup: boolean } => {
    if (s.mode === "local") {
      return s.key_present
        ? { label: "LOCAL", tone: "local", title: "AI steps run on this machine. Click to use Claude (Hosted).", setup: false }
        : {
            label: "LOCAL",
            tone: "local",
            title: "AI steps run on this machine. Click to set up Hosted (Claude with your Anthropic API key).",
            setup: true,
          };
    }
    if (!s.key_present) {
      return {
        label: "HOSTED · no key",
        tone: "error",
        title: "Hosted is on but no Anthropic API key is stored. Click to add one in Setup.",
        setup: true,
      };
    }
    if (s.reason === "key_rejected") {
      return {
        label: "HOSTED · key rejected",
        tone: "error",
        title: "Anthropic rejected the API key. Click to update it in Setup.",
        setup: true,
      };
    }
    if (s.effective === "local") {
      const why = FALLBACK_REASON[s.reason ?? "offline"] ?? "offline";
      return {
        label: `HOSTED → LOCAL (${why})`,
        tone: "fallback",
        title: `Hosted, but the last AI step ran on this machine (${why}). The next run tries ${s.hosted_model_name} again. Click to switch to Local.`,
        setup: false,
      };
    }
    return {
      label: `HOSTED · ${shortModelName(s)}`,
      tone: "hosted",
      title: `AI steps run on ${s.hosted_model_name} with your Anthropic API key (text only). Click to switch to Local.`,
      setup: false,
    };
  })();
  return s.last_error ? { ...chip, title: `${chip.title}\nLast Hosted error: ${s.last_error}` } : chip;
}

/** The model that runs AI steps, for prose: "Claude Opus 5.5" or "the
 *  on-device model" (neutral until the status has loaded). */
export function modelLabel(s: ModelStatus | null): string {
  if (!s) return "the configured model";
  return s.mode === "hosted" ? s.hosted_model_name : "the on-device model";
}

async function confirmHosted(): Promise<boolean> {
  return isTauri
    ? ask(HOSTED_CONSENT_TEXT, {
        title: "Turn on Hosted?",
        kind: "warning",
        okLabel: "Turn on Hosted",
        cancelLabel: "Keep Local",
      })
    : window.confirm(HOSTED_CONSENT_TEXT);
}

/** Switch where AI steps run, with the chip's rules: Hosted needs a key
 *  ("needs_key": open Setup's Models row), and the first time the user must
 *  confirm what it sends ("cancelled" keeps Local). Other errors throw. */
export async function chooseModelMode(target: ModelMode): Promise<"done" | "needs_key" | "cancelled"> {
  const cur = modelStore.state.status ?? (await refreshModelStatus());
  try {
    if (target === "local") {
      applyModelStatus(await setModelMode("local", false));
      return "done";
    }
    if (!cur?.key_present) return "needs_key";
    let consent = false;
    if (!cur.consented) {
      if (!(await confirmHosted())) return "cancelled";
      consent = true;
    }
    applyModelStatus(await setModelMode("hosted", consent));
    return "done";
  } catch (e) {
    const code = (e as { error?: { code?: string } })?.error?.code;
    void refreshModelStatus();
    if (code === "hosted_no_key") return "needs_key";
    throw e;
  }
}
