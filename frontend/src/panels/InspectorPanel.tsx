// Inspector card: global padding (matching the Gling reference panel),
// cut aggressiveness, and custom filler words.

import { useEffect, useState } from "react";
import {
  usePreferences,
  useSetGlobalPadding,
  useSetPreferences,
  useTimeline,
} from "../ipc/queries";

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
