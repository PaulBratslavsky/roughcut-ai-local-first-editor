// The Clips screen: every saved clip of the project, wherever it was made —
// in the editor (select transcript text → Add to clip, or the Clips tab's
// finder) or on the Shorts screen ("make shorts"). Play one, frame its 9:16
// crop over the player, rename or export it, or tick several and export them
// into one folder. App renders one screen at a time, so this mounts the same
// PreviewPanel (and player) as the editor and the Shorts screen.
//
// A batch export runs for minutes and the user may leave mid-run, so, as on
// the Shorts screen, "running" comes from the mutation keys and the ticks,
// progress line and outcome from shortsStore.

import { useMemo } from "react";
import { useIsMutating } from "@tanstack/react-query";
import { useStore } from "@tanstack/react-store";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { isTauri, revealPath } from "../ipc/api";
import {
  clipRunKeys,
  useClips,
  useExportClips,
  useProject,
  useTranscript,
  type ExportClipsArgs,
  type ExportClipsResult,
  type RunOptions,
} from "../ipc/queries";
import { useEditorKeyboard } from "../hooks/useEditorKeyboard";
import {
  EMPTY_SHORTS,
  patchShorts,
  setShortsProgress,
  setTicked,
  shortsOf,
  shortsStore,
  toggleTicked,
} from "../state/shortsStore";
import { setScreen } from "../state/viewStore";
import { errText, slugify } from "./clipUtil";
import { PlayerDrawer } from "./PlayerDrawer";
import { PreviewPanel } from "./PreviewPanel";
import { SavedClipRow } from "./SavedClipRow";
import { TaskLine } from "./TaskLine";
import { TopBar } from "./TopBar";

// A hook-level callback: it still runs if the screen is gone by then.
const EXPORT_RUN: RunOptions<ExportClipsArgs, ExportClipsResult> = {
  onSuccess: (r, a) => {
    const s = shortsOf(a.project_id);
    // Exported clips untick, so the next export doesn't render them again.
    const done = new Set(r.results.filter((x) => x.path).map((x) => x.clip_id));
    patchShorts(a.project_id, {
      ticked: s.ticked.filter((id) => !done.has(id)),
      exported: { dir: a.out_dir, results: r.results, titles: s.exportJob?.titles ?? {} },
      exportJob: null,
    });
    const first = r.results.find((x) => x.path)?.path;
    if (first) void revealPath(first);
  },
  onError: (e, a) => patchShorts(a.project_id, { exportError: errText(e), exportJob: null }),
};

const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

export function ClipsScreen({ projectId }: { projectId: string }) {
  const { data: project } = useProject(projectId);
  const { data: transcript } = useTranscript(projectId);
  const { data: clips = [] } = useClips(projectId);
  useEditorKeyboard(project, transcript);
  const exportBatch = useExportClips(projectId, EXPORT_RUN);
  // Running anywhere: a make from the Shorts screen lists its clips here
  // before their crops are framed.
  const making = useIsMutating({ mutationKey: clipRunKeys.make(projectId) }) > 0;
  const exporting = useIsMutating({ mutationKey: clipRunKeys.export(projectId) }) > 0;

  const s = useStore(shortsStore, (all) => all[projectId] ?? EMPTY_SHORTS);
  const ticked = useMemo(() => new Set(s.ticked), [s.ticked]);
  const toExport = clips.filter((c) => ticked.has(c.id));

  const runExport = async () => {
    // While a make is framing, its clips are listed with crops not yet set.
    if (toExport.length === 0 || exporting || making) return;
    let dir: string;
    if (isTauri) {
      let chosenDir: string | string[] | null;
      try {
        chosenDir = await openDialog({
          directory: true,
          multiple: false,
          title: `Export ${toExport.length} ${plural(toExport.length, "clip", "clips")} to…`,
        });
      } catch (e) {
        patchShorts(projectId, { exportError: `couldn't open the folder picker: ${errText(e)}` });
        return;
      }
      if (typeof chosenDir !== "string") return;
      dir = chosenDir;
    } else {
      // Browser mock: no folder picker; the mock only pretends to write.
      dir = `~/Downloads/${slugify(project?.name ?? "project")}-clips`;
    }
    setShortsProgress(projectId, "export_clips", null);
    patchShorts(projectId, {
      exportError: null,
      exported: null,
      exportJob: { dir, titles: Object.fromEntries(toExport.map((c) => [c.id, c.title])) },
    });
    exportBatch.mutate({ project_id: projectId, clip_ids: toExport.map((c) => c.id), out_dir: dir });
  };

  const exportedOk = s.exported?.results.filter((r) => r.path) ?? [];
  const exportedFailed = s.exported?.results.filter((r) => !r.path) ?? [];

  return (
    <div className="app-shell">
      <TopBar projectId={projectId} projectName={project?.name ?? "…"} />
      <main className="shorts-row">
        <section className="shorts-player">
          <PreviewPanel projectId={projectId} />
          <p className="clips-note shorts-player-note">
            ▶ plays a clip here · Space plays / pauses. "crop" frames a 9:16 clip over the picture: drag it, or
            centre it on the speaker.
          </p>
          <PlayerDrawer projectId={projectId} />
        </section>

        <section className="shorts-main">
          <div className="shorts-bar">
            <span className="clips-note">Tick clips to export them as MP4s into one folder.</span>
            <span className="shorts-bar-gap" />
            <button
              className="ghost-btn primary"
              onClick={() => void runExport()}
              disabled={toExport.length === 0 || exporting || making}
              title={
                making
                  ? "Wait for the shorts being made to be framed"
                  : "Export the ticked clips as MP4s into one folder"
              }
            >
              {exporting ? "exporting…" : `export ${toExport.length || ""}`.trim()}
            </button>
          </div>

          {(making || exporting) && (
            <div className="shorts-tasks">
              {making && <TaskLine label="make" event={s.progress.make_clips ?? null} waiting="making the shorts…" />}
              {exporting && <TaskLine label="export" event={s.progress.export_clips ?? null} waiting="starting…" />}
            </div>
          )}

          {s.exportError && <p className="empty-error">{s.exportError}</p>}

          {s.exported && (
            <div className="shorts-result">
              <span>
                exported {exportedOk.length} of {s.exported.results.length} to {s.exported.dir}
                {exportedOk[0]?.path && (
                  <button className="reveal-btn" onClick={() => void revealPath(exportedOk[0]!.path!)}>
                    reveal
                  </button>
                )}
              </span>
              {exportedOk.map((r) => (
                <span key={r.clip_id} className="shorts-file">
                  {r.path!.split("/").pop()}
                </span>
              ))}
              {exportedFailed.map((r) => (
                <span key={r.clip_id} className="shorts-failed">
                  “{s.exported!.titles[r.clip_id] ?? r.clip_id}” failed: {r.error}
                </span>
              ))}
            </div>
          )}

          <div className="clips-label">
            <span>saved clips ({clips.length})</span>
            {clips.length > 0 && (
              <span className="clips-method shorts-select">
                {toExport.length > 0 && `${toExport.length} ticked · `}
                <button
                  className="link-btn"
                  onClick={() => setTicked(projectId, clips.map((c) => c.id))}
                  disabled={making}
                  title={making ? "Wait for the shorts being made to be framed" : undefined}
                >
                  all
                </button>
                {" / "}
                <button className="link-btn" onClick={() => setTicked(projectId, [])}>
                  none
                </button>
              </span>
            )}
          </div>
          {clips.length > 0 ? (
            <div className="clips-list">
              {clips.map((c) => (
                <SavedClipRow
                  key={c.id}
                  projectId={projectId}
                  projectName={project?.name ?? "project"}
                  clip={c}
                  checked={ticked.has(c.id)}
                  onCheck={(on) => toggleTicked(projectId, c.id, on)}
                />
              ))}
            </div>
          ) : (
            <p className="clips-note">
              No saved clips yet. Save one in the{" "}
              <button className="link-btn" onClick={() => setScreen("editor")}>
                editor
              </button>{" "}
              (select transcript text → right-click → Add to clip, or the Clips tab's finder), or make shorts on
              the{" "}
              <button className="link-btn" onClick={() => setScreen("shorts")}>
                Shorts
              </button>{" "}
              screen.
            </p>
          )}
        </section>
      </main>
    </div>
  );
}
