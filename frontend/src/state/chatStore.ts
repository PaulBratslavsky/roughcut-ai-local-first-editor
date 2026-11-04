// The chat assistant's conversations, one per project. Every ChatPanel (the
// editor's Chat tab and the Shorts screen's drawer) shows the same one, the
// way both show the one terminal. The run and its streamed steps live here,
// not in a component, so switching tabs or screens mid-run loses nothing.
// Kept for the app session only; nothing is saved to disk.

import { Store } from "@tanstack/react-store";
import type { QueryClient } from "@tanstack/react-query";
import { callTool, onAppEvent } from "../ipc/api";
import type { ApplyInstructionArgs } from "../ipc/queries";
import type { AgentStepEvent, InstructionResult } from "../ipc/types";
import { errText } from "../panels/clipUtil";

export interface AgentMessage {
  id: string;
  role: "user" | "agent";
  text: string; // user text or final summary
  steps: AgentStepEvent[];
  pending: boolean;
  actionCount: number;
  actionIds: string[];
  undone: boolean;
  startedAt: number;
}

export interface ChatState {
  messages: AgentMessage[];
  /** The agent message a run is filling in, while one runs. */
  pendingId: string | null;
}

const EMPTY_CHAT: ChatState = { messages: [], pendingId: null };

export const chatStore = new Store<Record<string, ChatState>>({});

export const chatOf = (all: Record<string, ChatState>, projectId: string): ChatState =>
  all[projectId] ?? EMPTY_CHAT;

let msgCounter = 0;
const nextId = () => `msg-${++msgCounter}`;

function patchChat(projectId: string, fn: (c: ChatState) => ChatState): void {
  chatStore.setState((all) => ({ ...all, [projectId]: fn(chatOf(all, projectId)) }));
}

function patchMessage(projectId: string, id: string, fn: (m: AgentMessage) => AgentMessage): void {
  patchChat(projectId, (c) => ({ ...c, messages: c.messages.map((m) => (m.id === id ? fn(m) : m)) }));
}

let listening = false;

/** Stream agent-step events into the run they belong to. One app-wide
 *  listener; ChatPanel starts it on mount, so it is up before a send. */
export function ensureChatListener(): void {
  if (listening) return;
  listening = true;
  onAppEvent<AgentStepEvent>("agent-step", (step) => {
    const id = chatStore.state[step.project_id]?.pendingId;
    if (!id) return;
    patchMessage(step.project_id, id, (m) => ({
      ...m,
      steps: [...m.steps, step],
      text: step.kind === "final" && step.text ? step.text : m.text,
    }));
  });
}

/** Send an instruction to the chat assistant (apply_instruction). */
export async function sendChat(projectId: string, instruction: string, queryClient: QueryClient): Promise<void> {
  const chat = chatOf(chatStore.state, projectId);
  if (chat.pendingId) return;
  ensureChatListener();
  // Recent turns travel with the instruction so "apply the edits" / "do it"
  // resolve against what the agent proposed last turn.
  const history = chat.messages
    .filter((m) => m.text)
    .slice(-8)
    .map((m) => ({ role: m.role === "user" ? ("user" as const) : ("agent" as const), text: m.text }));
  const agentId = nextId();
  const now = Date.now();
  const base = { steps: [], actionCount: 0, actionIds: [], undone: false, startedAt: now };
  patchChat(projectId, (c) => ({
    pendingId: agentId,
    messages: [
      ...c.messages,
      { ...base, id: nextId(), role: "user", text: instruction, pending: false },
      { ...base, id: agentId, role: "agent", text: "", pending: true },
    ],
  }));
  try {
    const args: ApplyInstructionArgs = { project_id: projectId, instruction, history };
    const res = await callTool<InstructionResult>("apply_instruction", args);
    patchMessage(projectId, agentId, (m) => ({
      ...m,
      pending: false,
      text: res.summary,
      actionCount: res.actions.length,
      actionIds: res.actions.map((a) => a.id),
    }));
    for (const key of ["timeline", "transcript", "project"]) {
      void queryClient.invalidateQueries({ queryKey: [key] });
    }
  } catch (err) {
    patchMessage(projectId, agentId, (m) => ({ ...m, pending: false, text: `Something went wrong: ${errText(err)}` }));
  } finally {
    patchChat(projectId, (c) => ({ ...c, pendingId: null }));
  }
}

/** Undo the WHOLE turn (every action it applied), and only while those
 *  actions are still the newest journal entries — a plain `undo` popped one
 *  global entry, so an old button could revert unrelated later edits. */
export async function undoChatTurn(projectId: string, msgId: string, queryClient: QueryClient): Promise<void> {
  const msg = chatOf(chatStore.state, projectId).messages.find((m) => m.id === msgId);
  if (!msg || msg.actionIds.length === 0) return;
  try {
    await callTool("undo_actions", { project_id: projectId, action_ids: msg.actionIds });
    await queryClient.invalidateQueries();
    patchMessage(projectId, msgId, (m) => ({ ...m, undone: true }));
  } catch (e) {
    const why = errText(e);
    patchMessage(projectId, msgId, (m) => ({ ...m, text: `${m.text}\n\n[undo: ${why}]` }));
  }
}
