// One running task's progress line (scan, make, export) on the Shorts and
// Clips screens: a label, the run's latest message, and a bar that pulses
// until the first progress event arrives.

import type { ProgressEvent } from "../ipc/types";

export function TaskLine({ label, event, waiting }: { label: string; event: ProgressEvent | null; waiting: string }) {
  return (
    <div className="shorts-task">
      <div className="shorts-task-head">
        <span className="shorts-task-label">{label}</span>
        <span className="shorts-task-msg">{event?.message ?? waiting}</span>
      </div>
      <div className="progress-track">
        <div
          className={`progress-fill${event ? "" : " pulsing"}`}
          style={{ width: `${Math.round((event ? event.fraction : 1) * 100)}%` }}
        />
      </div>
    </div>
  );
}
