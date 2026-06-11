// Chat panel: conversational editing. Sends instructions to apply_instruction
// and renders streaming agent-step events (tool calls, results, final summary).

import { useEffect, useRef, useState } from "react";
import { onAppEvent } from "../ipc/api";
import { useApplyInstruction, useUndo } from "../ipc/queries";
import type { AgentStepEvent } from "../ipc/types";

interface AgentMessage {
  id: string;
  role: "user" | "agent";
  text: string; // user text or final summary
  steps: AgentStepEvent[];
  pending: boolean;
  actionCount: number;
  undone: boolean;
}

let msgCounter = 0;
const nextId = () => `msg-${++msgCounter}`;

// ---- friendly tool-call rendering (Claude-Desktop-style action cards) ----

const fmtTime = (s: number) => {
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
};

function friendlyToolName(tool: string): string {
  return tool.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
}

/** One human sentence about what a call is doing, from its args. */
function summarizeCall(tool: string, args?: Record<string, unknown>): string {
  const a = args ?? {};
  switch (tool) {
    case "find_segments":
      return `searching for “${String(a.query ?? "")}”`;
    case "detect_fillers":
      return "scanning for filler words";
    case "detect_silences":
      return "scanning for silences";
    case "detect_takes":
      return "looking for repeated takes";
    case "plan_duration_cut":
      return `planning cuts to reach ${fmtTime(Number(a.target_duration_s ?? 0))}`;
    case "cut_by_transcript":
    case "restore_by_transcript": {
      const n = Array.isArray(a.segment_ids) ? a.segment_ids.length : 0;
      return `${tool.startsWith("cut") ? "cutting" : "restoring"} ${n} transcript segment${n === 1 ? "" : "s"}`;
    }
    case "apply_edits": {
      const edits = Array.isArray(a.edits) ? (a.edits as { type?: string }[]) : [];
      const counts = new Map<string, number>();
      for (const e of edits) counts.set(e.type ?? "edit", (counts.get(e.type ?? "edit") ?? 0) + 1);
      const parts = [...counts].map(([t, n]) => `${n}× ${t.replace(/_/g, " ")}`);
      return `${edits.length} operation${edits.length === 1 ? "" : "s"} (${parts.join(", ")})`;
    }
    case "cut_range":
    case "restore_range":
      return `${tool === "cut_range" ? "cutting" : "restoring"} ${fmtTime(Number(a.start ?? 0))}–${fmtTime(Number(a.end ?? 0))}`;
    case "set_global_padding":
      return `padding ${Number(a.start_s ?? 0)}s around every cut`;
    case "generate_rough_cut":
      return `aggressiveness: ${String(a.aggressiveness ?? "default")}`;
    case "read_transcript":
      return `reading segments ${Number(a.offset ?? 0)}–${Number(a.offset ?? 0) + Number(a.limit ?? 50)}`;
    default: {
      const keys = Object.keys(a).filter((k) => k !== "project_id");
      return keys.length ? keys.join(", ") : "";
    }
  }
}

/** One human sentence about what a call DID, from its result. */
function summarizeResult(result?: Record<string, unknown>): string | null {
  if (!result) return null;
  const r = result as Record<string, any>;
  if (r.error) return `failed: ${String(r.error.message ?? r.error)}`;
  // Batch receipts and single-action results both carry descriptions.
  if (Array.isArray(r.actions) && r.actions.length > 0) {
    const dur = typeof r.included_duration_s === "number" ? ` — cut runs ${fmtTime(r.included_duration_s)}` : "";
    return `${r.actions.length} edit${r.actions.length === 1 ? "" : "s"} applied${dur}`;
  }
  if (typeof r.applied === "number" && typeof r.target_s === "number")
    return `cut ${r.applied} segments → ${fmtTime(Number(r.included_duration_s ?? 0))} (target ${fmtTime(r.target_s)})`;
  if (r.action?.description) return String(r.action.description);
  if (typeof r.projected_after_s === "number" && Array.isArray(r.segment_ids))
    return `plan: cut ${r.segment_ids.length} segments → ${fmtTime(r.projected_after_s)}`;
  if (Array.isArray(r.segments)) return `${r.segments.length} match${r.segments.length === 1 ? "" : "es"}`;
  if (typeof r.cut_count === "number" && r.timeline === undefined)
    return `${r.cut_count} cuts on the timeline`;
  if (r.timeline?.cut_count !== undefined) return `${r.timeline.cut_count} cuts on the timeline`;
  return "done";
}

function StepView({ step }: { step: AgentStepEvent }) {
  switch (step.kind) {
    case "thinking":
      return <div className="agent-step thinking">{step.text}</div>;
    case "tool_call":
      return (
        <div className="tool-card">
          <div className="tool-card-head">
            <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden>
              <path d="M6 2.5 2.5 6 6 9.5M10 6.5 13.5 10 10 13.5" />
            </svg>
            <span className="tool-card-name">{friendlyToolName(step.tool ?? "tool")}</span>
            <span className="tool-card-sub">{summarizeCall(step.tool ?? "", step.args)}</span>
          </div>
          {step.args && (
            <details className="tool-card-details">
              <summary>arguments</summary>
              <code>{JSON.stringify(step.args, null, 1)}</code>
            </details>
          )}
        </div>
      );
    case "tool_result": {
      const summary = summarizeResult(step.result);
      const failed = !!(step.result as Record<string, unknown> | undefined)?.error;
      return (
        <div className={`tool-card result${failed ? " failed" : ""}`}>
          <div className="tool-card-head">
            <span className="tool-card-tick" aria-hidden>{failed ? "✕" : "✓"}</span>
            <span className="tool-card-sub">{summary ?? "done"}</span>
          </div>
          {step.result && (
            <details className="tool-card-details">
              <summary>raw result</summary>
              <code>{JSON.stringify(step.result, null, 1)}</code>
            </details>
          )}
        </div>
      );
    }
    case "final":
      return null; // shown as the message text
  }
}

export function ChatPanel({ projectId }: { projectId: string }) {
  const [messages, setMessages] = useState<AgentMessage[]>([]);
  const [input, setInput] = useState("");
  const applyInstruction = useApplyInstruction();
  const undo = useUndo();
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const pendingIdRef = useRef<string | null>(null);

  // Stream agent-step events into the in-flight agent message.
  useEffect(() => {
    return onAppEvent<AgentStepEvent>("agent-step", (step) => {
      if (step.project_id !== projectId) return;
      const id = pendingIdRef.current;
      if (!id) return;
      setMessages((msgs) =>
        msgs.map((m) =>
          m.id === id
            ? {
                ...m,
                steps: [...m.steps, step],
                text: step.kind === "final" && step.text ? step.text : m.text,
              }
            : m,
        ),
      );
    });
  }, [projectId]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages]);

  const send = () => {
    const instruction = input.trim();
    if (!instruction || applyInstruction.isPending) return;
    setInput("");
    const agentId = nextId();
    pendingIdRef.current = agentId;
    // Recent turns travel with the instruction so "apply the edits" / "do it"
    // resolve against what the agent proposed last turn.
    const history = messages
      .filter((m) => m.text)
      .slice(-8)
      .map((m) => ({ role: m.role === "user" ? ("user" as const) : ("agent" as const), text: m.text }));
    setMessages((msgs) => [
      ...msgs,
      { id: nextId(), role: "user", text: instruction, steps: [], pending: false, actionCount: 0, undone: false },
      { id: agentId, role: "agent", text: "", steps: [], pending: true, actionCount: 0, undone: false },
    ]);
    applyInstruction.mutate(
      { project_id: projectId, instruction, history },
      {
        onSuccess: (res) => {
          setMessages((msgs) =>
            msgs.map((m) =>
              m.id === agentId
                ? { ...m, pending: false, text: res.summary, actionCount: res.actions.length }
                : m,
            ),
          );
          pendingIdRef.current = null;
        },
        onError: (err) => {
          setMessages((msgs) =>
            msgs.map((m) =>
              m.id === agentId
                ? { ...m, pending: false, text: `Something went wrong: ${String((err as Error)?.message ?? err)}` }
                : m,
            ),
          );
          pendingIdRef.current = null;
        },
      },
    );
  };

  const onUndo = (msgId: string) => {
    undo.mutate(
      { project_id: projectId },
      {
        onSuccess: () =>
          setMessages((msgs) => msgs.map((m) => (m.id === msgId ? { ...m, undone: true } : m))),
      },
    );
  };

  return (
    <div className="chat-panel">
      <div className="chat-scroll" ref={scrollRef}>
        {messages.length === 0 && (
          <div className="chat-empty">
            <p>Edit by chatting with the local model.</p>
            <p className="chat-suggestions">
              Try: <em>“remove all the filler words”</em> · <em>“cut the silences”</em> ·{" "}
              <em>“cut the parts about meetings”</em>
            </p>
          </div>
        )}
        {messages.map((m) =>
          m.role === "user" ? (
            <div key={m.id} className="chat-msg user">
              {m.text}
            </div>
          ) : (
            <div key={m.id} className="chat-msg agent">
              {m.steps.filter((s) => s.kind !== "final").map((s, i) => (
                <StepView key={i} step={s} />
              ))}
              {m.pending && m.steps.length === 0 && <div className="agent-step thinking">Thinking…</div>}
              {m.text && <div className="agent-summary">{m.text}</div>}
              {!m.pending && m.actionCount > 0 && (
                <button className="undo-link" onClick={() => onUndo(m.id)} disabled={m.undone || undo.isPending}>
                  {m.undone ? "Undone" : "Undo this edit"}
                </button>
              )}
            </div>
          ),
        )}
      </div>
      <form
        className="chat-input-row"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <input
          className="chat-input"
          placeholder="Tell the editor what to change…"
          value={input}
          onChange={(e) => setInput(e.target.value)}
        />
        <button className="primary-btn" type="submit" disabled={!input.trim() || applyInstruction.isPending}>
          {applyInstruction.isPending ? "Working…" : "Send"}
        </button>
      </form>
    </div>
  );
}
