// Transport row above the canvas timeline: play/pause, speed, show/skip cuts
// toggles, split-at-playhead, zoom buttons.

import { useStore } from "@tanstack/react-store";
import { useSplitClip, useTimeline } from "../ipc/queries";
import {
  setPlaybackRate,
  setShowCuts,
  setSkipCuts,
  togglePlaying,
  viewStore,
} from "../state/viewStore";
import { zoomTimeline } from "./Timeline";

function ToggleSwitch({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="switch-row">
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        className={`switch ${checked ? "on" : ""}`}
        onClick={() => onChange(!checked)}
      >
        <span className="switch-knob" />
      </button>
      <span className="switch-label">{label}</span>
    </label>
  );
}

export function TransportBar({ projectId }: { projectId: string }) {
  const playing = useStore(viewStore, (s) => s.playing);
  const rate = useStore(viewStore, (s) => s.playbackRate);
  const showCuts = useStore(viewStore, (s) => s.showCuts);
  const skipCuts = useStore(viewStore, (s) => s.skipCuts);
  const { data: timeline } = useTimeline(projectId);
  const splitClip = useSplitClip();

  const onSplit = () => {
    if (!timeline) return;
    const t = viewStore.state.playhead;
    const clip = timeline.clips.find(
      (c) => t > c.source_in + 0.05 && t < c.source_out - 0.05,
    );
    if (clip) {
      splitClip.mutate({ project_id: projectId, clip_id: clip.id, at_time: Number(t.toFixed(3)) });
    }
  };

  return (
    <div className="transport">
      <div className="transport-group">
        <button className="icon-btn play-btn" onClick={togglePlaying} title={playing ? "Pause (Space)" : "Play (Space)"}>
          {playing ? (
            <svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor">
              <rect x="2" y="1" width="4" height="12" rx="1" />
              <rect x="8" y="1" width="4" height="12" rx="1" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 14 14" fill="currentColor">
              <path d="M3 1.5v11l9-5.5z" />
            </svg>
          )}
        </button>
        <select
          className="select rate-select"
          value={String(rate)}
          onChange={(e) => setPlaybackRate(Number(e.target.value))}
          title="Playback speed"
        >
          <option value="1">1x</option>
          <option value="1.5">1.5x</option>
          <option value="2">2x</option>
        </select>
        <ToggleSwitch label="Show cuts" checked={showCuts} onChange={setShowCuts} />
        <ToggleSwitch label="Skip cuts" checked={skipCuts} onChange={setSkipCuts} />
      </div>

      <div className="transport-group">
        <button className="chip-btn" onClick={onSplit} disabled={!timeline || splitClip.isPending} title="Split clip at playhead">
          <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
            <circle cx="4" cy="4" r="2.2" />
            <circle cx="4" cy="12" r="2.2" />
            <path d="M6 5.5 14 13M6 10.5 14 3" />
          </svg>
          Split
        </button>
        <div className="zoom-group">
          <button className="icon-btn" onClick={() => zoomTimeline(0.75)} title="Zoom out">
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
              <circle cx="7" cy="7" r="5" />
              <path d="M11 11l3.5 3.5M4.5 7h5" />
            </svg>
          </button>
          <button className="icon-btn" onClick={() => zoomTimeline(1.33)} title="Zoom in">
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6">
              <circle cx="7" cy="7" r="5" />
              <path d="M11 11l3.5 3.5M4.5 7h5M7 4.5v5" />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}
