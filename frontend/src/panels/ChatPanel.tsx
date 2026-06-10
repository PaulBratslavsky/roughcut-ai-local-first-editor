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

function StepView({ step }: { step: AgentStepEvent }) {
  switch (step.kind) {
    case "thinking":
      return <div className="agent-step thinking">{step.text}</div>;
    case "tool_call":
      return (
        <div className="agent-step tool-call">
          <span className="step-badge">tool</span>
          <code>
            {step.tool}({step.args ? JSON.stringify(step.args) : ""})
          </code>
        </div>
      );
    case "tool_result":
      return (
        <div className="agent-step tool-result">
          <span className="step-badge result">result</span>
          <code>{step.result ? JSON.stringify(step.result) : "ok"}</code>
        </div>
      );
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
    setMessages((msgs) => [
      ...msgs,
      { id: nextId(), role: "user", text: instruction, steps: [], pending: false, actionCount: 0, undone: false },
      { id: agentId, role: "agent", text: "", steps: [], pending: true, actionCount: 0, undone: false },
    ]);
    applyInstruction.mutate(
      { project_id: projectId, instruction },
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
