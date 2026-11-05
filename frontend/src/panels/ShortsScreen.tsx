// The Shorts screen: scan the whole video for moments that stand alone as
// shorts, review them as cards beside the player, and make the ticked ones
// into 9:16 saved clips (framed on the speaker before "make" finishes). The
// saved clips, wherever they came from, live on the Clips screen, which
// exports them. App renders one screen at a time, so this mounts the same
// PreviewPanel (and player) the editor does.
//
// The runs take minutes and the user may leave mid-run, so nothing about a
// run lives in this component: "running" comes from the mutation keys
// (useIsMutating), and ticks, progress lines and outcomes from shortsStore,
// written by the run hooks' own callbacks (they fire after an unmount too).

import { useMemo } from "react";
import { useIsMutating } from "@tanstack/react-query";
import { useStore } from "@tanstack/react-store";
import {
  clipRunKeys,
  useClipCandidates,
  useCreateClips,
  useFindClips,
  useProject,
  useTranscript,
  type CreateClipsArgs,
  type CreateClipsResult,
  type FindClipsArgs,
  type FindClipsResult,
  type RunOptions,
} from "../ipc/queries";
import { useEditorKeyboard } from "../hooks/useEditorKeyboard";
import {
  EMPTY_SHORTS,
  patchShorts,
  setPicked,
  setShortsProgress,
  shortsOf,
  shortsStore,
  togglePicked,
} from "../state/shortsStore";
import { CandidateCard } from "./CandidateCard";
import { errText, scoredBy, useSecondsSince } from "./clipUtil";
import { modelLabel, useModelStatus } from "../state/modelStore";
import { PlayerDrawer } from "./PlayerDrawer";
import { PreviewPanel } from "./PreviewPanel";
import { TaskLine } from "./TaskLine";
import { TopBar } from "./TopBar";
import { setScreen } from "../state/viewStore";
import { transcribeErrors } from "../state/transcribeStore";

/** "Scan for shorts" (count and threshold are starting values to tune). */
const SCAN = { scan: true, count: 40, min_score: 40, multi_moment: true } as const;

// Run outcomes go to the store, keyed by the project the run was for. These
// are hook-level callbacks: they still run if the screen is gone by then.
const SCAN_RUN: RunOptions<FindClipsArgs, FindClipsResult> = {
  onSuccess: (r, a) =>
    patchShorts(a.project_id, { picked: [], lastScanFound: r.candidates.length, scanStartedAt: null }),
  onError: (e, a) => patchShorts(a.project_id, { error: errText(e), scanStartedAt: null }),
};

const MAKE_RUN: RunOptions<CreateClipsArgs, CreateClipsResult> = {
  onSuccess: (r, a) => {
    const s = shortsOf(a.project_id);
    const sent = new Set(a.candidate_ids);
    patchShorts(a.project_id, {
      picked: s.picked.filter((id) => !sent.has(id)),
      // The new shorts start ticked on the Clips screen, ready for "export".
      ticked: [...new Set([...s.ticked, ...r.clips.map((c) => c.id)])],
      made: {
        count: r.clips.length,
        failed: r.errors.map((e) => ({ title: s.makeTitles[e.candidate_id] ?? e.candidate_id, message: e.message })),
      },
      makeTitles: {},
    });
  },
  onError: (e, a) => patchShorts(a.project_id, { error: errText(e), makeTitles: {} }),
};

const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

export function ShortsScreen({ projectId }: { projectId: string }) {
  const { data: project } = useProject(projectId);
  const { data: transcript } = useTranscript(projectId);
  const { data: candidates = [] } = useClipCandidates(projectId);
  useEditorKeyboard(project, transcript);
  const find = useFindClips(projectId, SCAN_RUN);
  const make = useCreateClips(projectId, MAKE_RUN);
  // Running anywhere (this screen before a remount, or the Clips tab).
  const scanning = useIsMutating({ mutationKey: clipRunKeys.find(projectId) }) > 0;
  const making = useIsMutating({ mutationKey: clipRunKeys.make(projectId) }) > 0;

  const s = useStore(shortsStore, (all) => all[projectId] ?? EMPTY_SHORTS);
  const transcribeError = useStore(transcribeErrors, (all) => all[projectId] ?? null);
  const elapsed = useSecondsSince(scanning ? s.scanStartedAt : null);
  const picked = useMemo(() => new Set(s.picked), [s.picked]);

  const hasSpeech = !!transcript?.segments.some((seg) => !seg.is_silence);
  const chosen = candidates.filter((c) => picked.has(c.id));
  const scorers = scoredBy(candidates);
  const model = useModelStatus();
  const theme = candidates[0]?.theme ?? null;

  const runScan = () => {
    if (scanning) return;
    setShortsProgress(projectId, "find_clips", null);
    patchShorts(projectId, { error: null, lastScanFound: null, scanStartedAt: Date.now() });
    find.mutate({ project_id: projectId, ...SCAN });
  };

  const runMake = () => {
    if (chosen.length === 0 || making) return;
    setShortsProgress(projectId, "make_clips", null);
    patchShorts(projectId, {
      error: null,
      made: null,
      makeTitles: Object.fromEntries(chosen.map((c) => [c.id, c.title])),
    });
    make.mutate({ project_id: projectId, candidate_ids: chosen.map((c) => c.id) });
  };

  return (
    <div className="app-shell">
      <TopBar projectId={projectId} projectName={project?.name ?? "…"} />
      <main className="shorts-row">
        <section className="shorts-player">
          <PreviewPanel projectId={projectId} shortsGuide />
          <p className="clips-note shorts-player-note">
            Click a card's picture to play it here · Space plays / pauses. While a card plays, the centred 9:16
            window shows where its short starts; making it frames the crop on the speaker.
          </p>
          <PlayerDrawer projectId={projectId} />
        </section>

        <section className="shorts-main">
          <div className="shorts-bar">
            <button className="ghost-btn" onClick={runScan} disabled={scanning || !hasSpeech}>
              {scanning
                ? `scanning…${s.scanStartedAt ? ` ${elapsed}s` : ""}`
                : candidates.length
                  ? "▸ scan again"
                  : "▸ scan for shorts"}
            </button>
            <span className="shorts-bar-gap" />
            <button
              className="ghost-btn primary"
              onClick={runMake}
              disabled={chosen.length === 0 || making}
              title="Make the ticked cards into 9:16 shorts, framed on the speaker"
            >
              {making
                ? "making…"
                : chosen.length > 0
                  ? `make ${chosen.length} ${plural(chosen.length, "short", "shorts")}`
                  : "make shorts"}
            </button>
          </div>

          {(scanning || making) && (
            <div className="shorts-tasks">
              {scanning && (
                <TaskLine label="scan" event={s.progress.find_clips ?? null} waiting="reading the transcript…" />
              )}
              {making && <TaskLine label="make" event={s.progress.make_clips ?? null} waiting="making the shorts…" />}
            </div>
          )}

          {!hasSpeech &&
            (transcribeError ? (
              <p className="empty-error">{transcribeError} — retry from the editor's transcript.</p>
            ) : (
              <p className="clips-note">Transcribe the video first — shorts are found from what you say.</p>
            ))}
          {scanning && (
            <p className="clips-note">
              A scan judges moments from every part of the video with {modelLabel(model)} — minutes on a long
              video. The rest of the app stays usable.
            </p>
          )}
          {s.error && <p className="empty-error">{s.error}</p>}

          {s.made && (
            <div className="shorts-result">
              {s.made.count > 0 && (
                <span>
                  made {s.made.count} {plural(s.made.count, "short", "shorts")} · ticked on the{" "}
                  <button className="link-btn" onClick={() => setScreen("clips")}>
                    Clips
                  </button>{" "}
                  screen, ready to export
                </span>
              )}
              {s.made.failed.map((f, i) => (
                <span key={i} className="shorts-failed">
                  couldn't make “{f.title}”: {f.message}
                </span>
              ))}
            </div>
          )}

          <div className="clips-label">
            <span>
              candidates ({candidates.length})
              {theme && <span className="clips-method"> · about “{theme}”</span>}
            </span>
            {candidates.length > 0 && (
              <span className="clips-method shorts-select">
                {chosen.length > 0 && `${chosen.length} ticked · `}
                <button className="link-btn" onClick={() => setPicked(projectId, candidates.map((c) => c.id))}>
                  all
                </button>
                {" / "}
                <button className="link-btn" onClick={() => setPicked(projectId, [])}>
                  none
                </button>
              </span>
            )}
          </div>
          {candidates.length > 0 && (
            <p className="clips-note" title={scorers.raw ? `Scored by: ${scorers.raw}` : undefined}>
              {scorers.text
                ? `Scored by ${scorers.text} from the transcript: a hook, making sense alone, a payoff.`
                : "Scored without a model: ranked by the rule-based score, single stretches only. Multi-moment shorts need a model."}
            </p>
          )}
          {candidates.length > 0 ? (
            <div className="shorts-grid">
              {candidates.map((c) => (
                <CandidateCard
                  key={c.id}
                  projectId={projectId}
                  candidate={c}
                  checked={picked.has(c.id)}
                  onCheck={(on) => togglePicked(projectId, c.id, on)}
                />
              ))}
            </div>
          ) : (
            hasSpeech &&
            !scanning && (
              <p className="clips-note">
                {s.lastScanFound === 0
                  ? "No new moments: either everything that fits is already saved as a clip, or nothing scored 40 or more."
                  : "Scan the whole video for moments that stand alone as shorts. Each card gets a short score (0–100) for its hook, making sense alone, and its payoff — judged from the transcript, not the picture."}
              </p>
            )
          )}
        </section>
      </main>
    </div>
  );
}
