// Inspector card: global padding (matching the Gling reference panel),
// target length, cut aggressiveness, and custom filler words.

import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { callTool } from "../ipc/api";
import {
  useApplyDurationCut,
  useProject,
  usePreferences,
  useSetGlobalPadding,
  useSetPreferences,
  useTimeline,
} from "../ipc/queries";
import type { DurationCutPlan } from "../ipc/types";

const fmtLen = (s: number) => {
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
};

function PaddingSlider({
  label,
  value,
  onChange,
}: {
  label: string;
  value: number;
  onChange: (v: number) => void;
}) {
  return (
    <div className="padding-row">
      <span className="padding-label">{label}</span>
      <input
        type="range"
        min={0}
        max={1}
        step={0.01}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
      />
      <span className="padding-value">{value.toFixed(2)}s</span>
    </div>
  );
}

/** "Make it N minutes" as a deterministic control: type a target, see the
 *  plan ("cut 38 segments → 19:58"), Apply lands it as ONE undoable action —
 *  the same plan_duration_cut engine the chat and MCP clients use. */
function AudioSyncSection({ projectId }: { projectId: string }) {
  const project = useProject(projectId);
  const queryClient = useQueryClient();
  const saved = project.data?.audio_offset_s ?? 0;
  const [ms, setMs] = useState<number | null>(null);
  const value = ms ?? Math.round(saved * 1000);
  const commit = async (v: number) => {
    setMs(v);
    await callTool("set_audio_offset", { project_id: projectId, offset_s: v / 1000 });
    await queryClient.invalidateQueries({ queryKey: ["project", projectId] });
  };
  return (
    <section className="inspector-section">
      <h3>Audio sync</h3>
      <p className="section-note">
        Hearing words before lips move? Drag right to delay the audio.
        Applies to playback and export — the file is untouched.
      </p>
      <div className="audio-sync-row">
        <input
          type="range"
          min={-500}
          max={500}
          step={10}
          value={value}
          onChange={(e) => setMs(Number(e.target.value))}
          onMouseUp={() => void commit(value)}
          onTouchEnd={() => void commit(value)}
        />
        <span className="audio-sync-value">
          {value > 0 ? "+" : ""}{value} ms
        </span>
        {value !== 0 && (
          <button className="link-btn" onClick={() => void commit(0)}>reset</button>
        )}
      </div>
    </section>
  );
}

function TargetLengthSection({ projectId }: { projectId: string }) {
  const { data: timeline } = useTimeline(projectId);
  const applyCut = useApplyDurationCut();
  const [minutes, setMinutes] = useState("");
  const [plan, setPlan] = useState<DurationCutPlan | null>(null);
  const [planError, setPlanError] = useState<string | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const included =
    timeline?.clips
      .filter((c) => c.included)
      .reduce((acc, c) => acc + (c.source_out - c.source_in), 0) ?? 0;

  const targetS = Number(minutes) * 60;
  const validTarget = Number.isFinite(targetS) && targetS > 0;

  // Preview the plan (apply=false) as the target changes, debounced.
  useEffect(() => {
    setPlan(null);
    setPlanError(null);
    if (!validTarget) return;
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      callTool<DurationCutPlan>("plan_duration_cut", {
        project_id: projectId,
        target_duration_s: targetS,
      })
        .then(setPlan)
        .catch((err) => setPlanError(String((err as { message?: string })?.message ?? err)));
    }, 400);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId, targetS, included]);

  const preview = (() => {
    if (planError) return planError;
    if (!validTarget) return `current cut: ${fmtLen(included)}`;
    if (!plan) return "planning…";
    if (plan.segment_ids.length === 0) return `already at ${fmtLen(plan.before_s)} — under the target`;
    return `cut ${plan.segment_ids.length} segments → ${fmtLen(plan.projected_after_s)} (now ${fmtLen(plan.before_s)})`;
  })();

  const apply = () => {
    if (!validTarget) return;
    applyCut.mutate(
      { project_id: projectId, target_duration_s: targetS, apply: true },
      { onSuccess: () => setPlan(null) },
    );
  };

  return (
    <div className="inspector-section">
      <h3 className="inspector-title">Target length</h3>
      <p className="inspector-desc">
        Cuts the least-important material (semantic ranking) until the video fits — one undoable step
      </p>
      <label className="field-row">
        <span>Minutes</span>
        <input
          className="text-input target-minutes"
          type="number"
          min={1}
          step={1}
          placeholder={String(Math.max(1, Math.round(included / 60)))}
          value={minutes}
          onChange={(e) => setMinutes(e.target.value)}
        />
      </label>
      <p className="target-preview">{preview}</p>
      <div className="inspector-actions">
        <button
          className="primary-btn"
          onClick={apply}
          disabled={!validTarget || !plan || plan.segment_ids.length === 0 || applyCut.isPending}
        >
          {applyCut.isPending ? "Cutting…" : "Apply"}
        </button>
        {applyCut.isSuccess && !applyCut.isPending && (
          <span className="apply-ok">
            now {fmtLen(applyCut.data.included_duration_s)} — undo in the top bar
          </span>
        )}
      </div>
    </div>
  );
}

export function InspectorPanel({ projectId }: { projectId: string }) {
  const { data: timeline } = useTimeline(projectId);
  const { data: prefs } = usePreferences();
  const setGlobalPadding = useSetGlobalPadding();
  const setPreferences = useSetPreferences();

  const [start, setStart] = useState(0.15);
  const [end, setEnd] = useState(0.15);
  const [linked, setLinked] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [fillerText, setFillerText] = useState("");
  const [fillerLoaded, setFillerLoaded] = useState(false);

  useEffect(() => {
    if (timeline && !loaded) {
      setStart(timeline.global_padding.start_s);
      setEnd(timeline.global_padding.end_s);
      setLinked(timeline.global_padding.linked);
      setLoaded(true);
    }
  }, [timeline, loaded]);

  useEffect(() => {
    if (prefs && !fillerLoaded) {
      setFillerText(prefs.custom_filler_words.join(", "));
      setFillerLoaded(true);
    }
  }, [prefs, fillerLoaded]);

  const onStart = (v: number) => {
    setStart(v);
    if (linked) setEnd(v);
  };
  const onEnd = (v: number) => {
    setEnd(v);
    if (linked) setStart(v);
  };

  const apply = () => {
    setGlobalPadding.mutate({
      project_id: projectId,
      start_s: Number(start.toFixed(2)),
      end_s: Number(end.toFixed(2)),
      linked,
    });
  };

  const saveAggressiveness = (v: "natural" | "aggressive") => {
    setPreferences.mutate({ preferences: { cut_aggressiveness: v } });
  };

  const saveFillerWords = () => {
    const words = fillerText
      .split(",")
      .map((w) => w.trim())
      .filter(Boolean);
    setPreferences.mutate({ preferences: { custom_filler_words: words } });
  };

  return (
    <div className="inspector-card card">
      <div className="inspector-section">
        <h3 className="inspector-title">Padding</h3>
        <p className="inspector-desc">Set padding to add or remove time from all talking clips</p>
        <div className="padding-grid">
          <div className="padding-sliders">
            <PaddingSlider label="Start" value={start} onChange={onStart} />
            <PaddingSlider label="End" value={end} onChange={onEnd} />
          </div>
          <button
            type="button"
            className={`link-toggle${linked ? " on" : ""}`}
            onClick={() => setLinked(!linked)}
            title={linked ? "Unlink start/end" : "Link start/end"}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7" />
              <path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7" />
            </svg>
          </button>
        </div>
        <div className="inspector-actions">
          <button className="primary-btn" onClick={apply} disabled={setGlobalPadding.isPending}>
            {setGlobalPadding.isPending ? "Applying…" : "Apply"}
          </button>
          {setGlobalPadding.isSuccess && !setGlobalPadding.isPending && (
            <span className="apply-ok">Applied</span>
          )}
        </div>
      </div>

      <div className="inspector-divider" />

      <TargetLengthSection projectId={projectId} />
      <AudioSyncSection projectId={projectId} />

      <div className="inspector-divider" />

      <div className="inspector-section">
        <h3 className="inspector-title">Cutting</h3>
        <label className="field-row">
          <span>Aggressiveness</span>
          <select
            className="select"
            value={prefs?.cut_aggressiveness ?? "natural"}
            onChange={(e) => saveAggressiveness(e.target.value as "natural" | "aggressive")}
          >
            <option value="natural">Natural</option>
            <option value="aggressive">Aggressive</option>
          </select>
        </label>
        <label className="field-col">
          <span>Custom filler words</span>
          <input
            className="text-input"
            placeholder="like, you know, basically"
            value={fillerText}
            onChange={(e) => setFillerText(e.target.value)}
            onBlur={saveFillerWords}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            }}
          />
        </label>
      </div>
    </div>
  );
}
