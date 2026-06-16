// In-memory mock of the core tool surface (docs/tool-api.md), used when the app
// runs in a plain browser (no Tauri). Emits the same events the Rust core emits.

import type {
  AgentStepEvent,
  Clip,
  EditAction,
  Media,
  Padding,
  Preferences,
  ProgressEvent,
  Project,
  ProjectSummary,
  Timeline,
  Transcript,
  TranscriptSegment,
  Word,
  EditOp,
} from "./types";

// ---------------------------------------------------------------------------
// Tiny event emitter (mirrors @tauri-apps/api/event listen/emit)
// ---------------------------------------------------------------------------

type Handler = (payload: unknown) => void;
const listeners = new Map<string, Set<Handler>>();

export function mockOn(event: string, handler: Handler): () => void {
  let set = listeners.get(event);
  if (!set) {
    set = new Set();
    listeners.set(event, set);
  }
  set.add(handler);
  return () => { set.delete(handler); };
}

function emit(event: string, payload: unknown): void {
  const set = listeners.get(event);
  if (!set) return;
  // Async delivery, like real IPC events.
  setTimeout(() => { for (const h of [...set]) h(payload); }, 0);
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

// ---------------------------------------------------------------------------
// Deterministic PRNG for fixture data / waveforms
// ---------------------------------------------------------------------------

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0; a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ---------------------------------------------------------------------------
// Fixture transcript: ~5 min talking-head monologue, ~40 segments
// ---------------------------------------------------------------------------

const SENTENCES: string[] = [
  "Hey everyone, so today I want to talk about why I finally quit my job and what I learned along the way.",
  "Um, this is something a lot of you have been asking about in the comments.",
  "About two years ago I was working as a senior engineer at a company you have definitely heard of.",
  "On paper everything looked great, good salary, remote work, smart coworkers.",
  "But, uh, every Sunday night I would get this knot in my stomach thinking about Monday.",
  "And I kept telling myself that feeling was normal, that everybody hates Mondays.",
  "Spoiler alert, it is not normal, at least not at that intensity.",
  "So the first thing I did was start tracking how I actually spent my time every week.",
  "Um, the results were honestly kind of shocking to me.",
  "I was spending almost sixty percent of my week in meetings that produced nothing.",
  "The actual engineering work, the thing I was hired to do, was squeezed into the gaps.",
  "And when I raised this with my manager, the answer was always the same, that is just how it is here.",
  "Uh, so I started building things on the side, little tools, small experiments.",
  "One of those side projects started getting real users within a couple of months.",
  "Nothing crazy, like a few hundred people, but they were paying actual money.",
  "And that completely changed how I thought about my own value as an engineer.",
  "Because here is the thing nobody tells you when you are grinding away at a day job.",
  "Your skills are worth a lot more than the line item on your offer letter.",
  "Um, the company knows this, that is literally the whole business model.",
  "So I set myself a deadline, twelve months to replace half my salary or I stay put.",
  "I am not going to pretend it was easy, because it absolutely was not.",
  "There were months where revenue went down instead of up and I panicked.",
  "Uh, there was one launch that completely flopped, like single digit sales.",
  "But every failure taught me something I could not have learned in a meeting room.",
  "I learned how to talk to customers without a product manager in between.",
  "I learned how to ship something imperfect and improve it in public.",
  "And honestly, I learned how to manage my own energy instead of my calendar.",
  "By month ten I had matched about forty percent of my salary, close but not the goal.",
  "Um, and this is where most people would either give up or quit too early.",
  "I did neither, I renegotiated with myself and extended the deadline by six months.",
  "That flexibility, being honest about the data instead of the dream, saved me.",
  "At month fourteen a bigger company asked to license one of my tools.",
  "That single deal pushed me past the finish line basically overnight.",
  "So I walked into my manager's office, uh, well, a video call, and I resigned.",
  "No drama, no burning bridges, I even helped hire my replacement.",
  "It has now been eight months since my last day and I want to be real with you.",
  "Some weeks are amazing and some weeks I miss having coworkers to complain with.",
  "But I have not had a single Sunday night knot in my stomach since I left.",
  "If you take one thing from this video, let it be this, run the experiment first.",
  "Quit with data, not with rage, and thanks so much for watching, see you in the next one.",
];

// Indices after which a standalone filler ("um"/"uh") segment is inserted.
const FILLER_AFTER = new Set([2, 7, 13, 19, 26, 33]);
// Indices after which a silence gap is inserted.
const SILENCE_AFTER = new Set([11, 28]);

interface Fixture { transcript: Transcript; clips: Clip[]; duration: number }

function buildFixture(): Fixture {
  const rng = mulberry32(0xfab1e);
  const segments: TranscriptSegment[] = [];
  let t = 0.4;
  let segIdx = 0;

  const pushSegment = (text: string, opts: { filler?: boolean; silence?: boolean; dur?: number }) => {
    const id = `seg-${String(segIdx++).padStart(2, "0")}`;
    let words: Word[] = [];
    let end: number;
    if (opts.silence) {
      end = t + (opts.dur ?? 2.5 + rng() * 1.5);
    } else {
      const tokens = text.split(/\s+/);
      let wt = t;
      words = tokens.map((tok) => {
        const dur = 0.22 + rng() * 0.2 + Math.min(tok.length, 10) * 0.012;
        const w: Word = {
          text: tok,
          start: Number(wt.toFixed(3)),
          end: Number((wt + dur).toFixed(3)),
          confidence: 0.82 + rng() * 0.17,
        };
        wt += dur + 0.03 + rng() * 0.05;
        return w;
      });
      end = words[words.length - 1]!.end;
    }
    segments.push({
      id,
      start: Number(t.toFixed(3)),
      end: Number(end.toFixed(3)),
      text,
      words,
      is_filler: !!opts.filler,
      is_silence: !!opts.silence,
      take_group_id: null,
      is_best_take: false,
    });
    t = end + 0.25 + rng() * 0.45;
  };

  SENTENCES.forEach((sentence, i) => {
    pushSegment(sentence, {});
    if (FILLER_AFTER.has(i)) pushSegment(rng() > 0.5 ? "Um." : "Uh...", { filler: true });
    if (SILENCE_AFTER.has(i)) pushSegment("", { silence: true });
  });

  const duration = Number((t + 1.0).toFixed(3));

  // Build ~30 clips: speech segments pair-merged into included clips;
  // filler/silence segments become their own clips (some excluded up-front).
  const clips: Clip[] = [];
  let order = 0;
  let cursor = 0;
  let i = 0;
  const isSpeech = (s: TranscriptSegment) => !s.is_filler && !s.is_silence;

  while (i < segments.length) {
    const seg = segments[i]!;
    if (isSpeech(seg)) {
      const group = [seg];
      if (i + 1 < segments.length && isSpeech(segments[i + 1]!)) {
        group.push(segments[i + 1]!);
        i += 2;
      } else {
        i += 1;
      }
      const next = segments[i];
      const out = next ? (group[group.length - 1]!.end + next.start) / 2 : duration;
      clips.push({
        id: `clip-${String(order).padStart(2, "0")}`,
        source_in: Number(cursor.toFixed(3)),
        source_out: Number(out.toFixed(3)),
        included: true,
        order,
        origin: "initial",
        linked_segment_ids: group.map((s) => s.id),
      });
      order += 1;
      cursor = out;
    } else {
      i += 1;
      const next = segments[i];
      const out = next ? (seg.end + next.start) / 2 : duration;
      clips.push({
        id: `clip-${String(order).padStart(2, "0")}`,
        source_in: Number(cursor.toFixed(3)),
        source_out: Number(out.toFixed(3)),
        // A few fillers/silences arrive pre-cut so the timeline shows excluded clips.
        included: !(seg.is_silence || order % 3 === 0),
        order,
        origin: "ai_cut",
        linked_segment_ids: [seg.id],
      });
      order += 1;
      cursor = out;
    }
  }

  const transcript: Transcript = {
    id: "transcript-demo",
    media_id: "media-demo",
    language: "en",
    segments,
    model_used: "whisper-small.en (mock)",
  };

  return { transcript, clips, duration };
}

// ---------------------------------------------------------------------------
// Mock core state
// ---------------------------------------------------------------------------

interface Snapshot { timeline: Timeline; action: EditAction }

interface MockState {
  trashed: Project | null;
  project: Project | null;
  transcript: Transcript | null;
  undoStack: Snapshot[];
  redoStack: Snapshot[];
  preferences: Preferences;
}

const defaultPadding: Padding = { start_s: 0.15, end_s: 0.15, linked: true };

const state: MockState = {
  trashed: null,
  project: null,
  transcript: null,
  undoStack: [],
  redoStack: [],
  preferences: {
    default_padding: { ...defaultPadding },
    cut_aggressiveness: "natural",
    custom_filler_words: ["like", "you know"],
    silence_min_duration_s: 0.8,
    export_target: "premiere_xml",
    language: "en",
    model_tier: "auto",
    inference_endpoint: "http://localhost:11434",
    inference_model: "gemma-4 (mock)",
    embedding_model: "nomic-embed-text",
  },
};

let actionCounter = 0;

const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

function requireProject(projectId?: unknown): Project {
  const p = state.project;
  if (!p) throw { code: "not_found", message: "No project open" };
  if (projectId && projectId !== p.id) {
    throw { code: "not_found", message: `Unknown project ${String(projectId)}` };
  }
  return p;
}

function snapshotOp(timeline: Timeline): EditOp {
  return {
    type: "set_clips",
    clips: clone(timeline.clips),
    global_padding: clone(timeline.global_padding),
  };
}

function makeAction(kind: string, description: string, before: EditOp): EditAction {
  actionCounter += 1;
  return {
    id: `action-${actionCounter}`,
    kind,
    source: "ui",
    timestamp: new Date().toISOString(),
    description,
    // The mock mirrors the core's shape; `op` is snapshot-based here.
    op: before,
  };
}

function recordEdit(project: Project, kind: string, description: string): EditAction {
  const action = makeAction(kind, description, snapshotOp(project.timeline));
  state.undoStack.push({ timeline: clone(project.timeline), action });
  state.redoStack = [];
  return action;
}

let splitCounter = 0;

/** Mirror the Rust core: split clips at the range boundaries so only the
 *  exact [start, end] span flips state (the cut piece drops segment links). */
function setRangeIncluded(project: Project, start: number, end: number, included: boolean): void {
  const EPS = 1e-6;
  if (end - start < EPS) return;
  const out: Clip[] = [];
  for (const c of project.timeline.clips) {
    if (c.source_out <= start + EPS || c.source_in >= end - EPS) {
      out.push(c);
      continue;
    }
    if (c.source_in < start - EPS) {
      out.push({ ...c, id: `clip-split-${splitCounter++}`, source_out: start });
    }
    out.push({
      ...c,
      id: `clip-split-${splitCounter++}`,
      source_in: Math.max(c.source_in, start),
      source_out: Math.min(c.source_out, end),
      included,
      linked_segment_ids: [],
    });
    if (c.source_out > end + EPS) {
      out.push({ ...c, id: `clip-split-${splitCounter++}`, source_in: end });
    }
  }
  out.sort((a, b) => a.source_in - b.source_in);
  out.forEach((c, i) => (c.order = i));
  project.timeline.clips = out;
}

function refreshTimeline(project: Project): void {
  project.timeline.cut_count = project.timeline.clips.filter((c) => !c.included).length;
  project.updated_at = new Date().toISOString();
  emit("timeline-changed", { project_id: project.id });
}

function emptyTimeline(): Timeline {
  return {
    id: "timeline-demo",
    clips: [],
    global_padding: clone(state.preferences.default_padding),
    cut_count: 0,
    duration: 0,
  };
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

type Args = Record<string, unknown>;

async function emitProgress(task: ProgressEvent["task"], projectId: string, steps: string[], totalMs: number) {
  for (let i = 0; i < steps.length; i++) {
    emit("progress", {
      task,
      project_id: projectId,
      fraction: i / steps.length,
      message: steps[i],
    } satisfies ProgressEvent);
    await sleep(totalMs / steps.length);
  }
  emit("progress", { task, project_id: projectId, fraction: 1, message: "Done" } satisfies ProgressEvent);
}

const tools: Record<string, (args: Args) => Promise<unknown> | unknown> = {
  // ---- Project / state ----------------------------------------------------

  list_projects(): { projects: ProjectSummary[]; trash: ProjectSummary[] } {
    const summary = (p: Project, deletedAt: string | null): ProjectSummary => ({
      id: p.id,
      name: p.name,
      updated_at: p.updated_at,
      deleted_at: deletedAt,
    });
    return {
      projects: state.project ? [summary(state.project, null)] : [],
      trash: state.trashed ? [summary(state.trashed, new Date().toISOString())] : [],
    };
  },

  create_project(args): Project {
    const now = new Date().toISOString();
    // Browser-harness real-video mode: an http file_path becomes playable
    // media with a known duration so engine paths run end to end.
    const httpMedia = typeof args.file_path === "string" && args.file_path.startsWith("http");
    state.project = {
      id: "project-demo",
      name: typeof args.name === "string" && args.name ? args.name : "Untitled project",
      media: httpMedia
        ? {
            id: "media-http",
            file_path: String(args.file_path),
            duration: 30,
            frame_rate: 30,
            width: 640,
            height: 360,
            audio_sample_rate: 48000,
            codec: "h264",
            imported_at: now,
          }
        : null,
      timeline: httpMedia
        ? {
            id: "timeline-http",
            clips: [
              {
                id: "clip-http",
                source_in: 0,
                source_out: 30,
                included: true,
                origin: "initial",
                order: 0,
                linked_segment_ids: [],
              },
            ],
            global_padding: { start_s: 0, end_s: 0, linked: true },
            cut_count: 0,
            duration: 30,
          }
        : emptyTimeline(),
      created_at: now,
      updated_at: now,
      schema_version: 2,
      deleted_at: null,
      audio_offset_s: 0,
      screen_media: null,
      layout: { mode: "pip", shape: "rounded", corner: "br", size: 0.25 },
      suggestions: [],
      checkpoints: [],
    };
    state.undoStack = [];
    state.redoStack = [];
    state.transcript = null;
    emit("projects-changed", {});
    return clone(state.project);
  },

  duplicate_project(args): Project {
    const p = requireProject(args.project_id);
    // Single-project mock: a duplicate just renames a clone in place.
    const copy = { ...clone(p), id: `project-copy-${Date.now() % 100000}`, name: String(args.name ?? `${p.name} copy`) };
    emit("projects-changed", {});
    return copy;
  },

  delete_project(args): { deleted: boolean } {
    state.trashed = requireProject(args.project_id);
    state.project = null;
    emit("projects-changed", {});
    return { deleted: true };
  },

  restore_project(args): Project {
    const trashed = state.trashed;
    if (!trashed || trashed.id !== args.project_id) {
      throw { code: "not_found", message: "nothing in the trash with that id" };
    }
    state.project = trashed;
    state.trashed = null;
    emit("projects-changed", {});
    return clone(trashed);
  },

  open_project(args): Project {
    return clone(requireProject(args.project_id));
  },

  save_project(args): Project {
    const p = requireProject(args.project_id);
    p.updated_at = new Date().toISOString();
    return clone(p);
  },

  get_media_assets(args): { peaks_path: null; peaks_per_second: number; thumbnails: []; playback_path: null } {
    requireProject(args.project_id);
    return { peaks_path: null, peaks_per_second: 50, thumbnails: [], playback_path: null };
  },

  get_suggestions(args): unknown {
    return { suggestions: requireProject(args.project_id).suggestions ?? [] };
  },

  accept_suggestion(args): unknown {
    const p = requireProject(args.project_id);
    p.suggestions = (p.suggestions ?? []).filter((s) => s.id !== args.suggestion_id);
    return { action: null };
  },

  accept_all_suggestions(args): unknown {
    const p = requireProject(args.project_id);
    const n = (p.suggestions ?? []).length;
    p.suggestions = [];
    return { action: null, accepted: n };
  },

  dismiss_suggestion(args): unknown {
    const p = requireProject(args.project_id);
    p.suggestions = (p.suggestions ?? []).filter((s) => s.id !== args.suggestion_id);
    return { dismissed: true };
  },

  get_timeline(args): Timeline {
    return clone(requireProject(args.project_id).timeline);
  },

  read_transcript(args): unknown {
    requireProject(args.project_id);
    if (!state.transcript) throw { code: "invalid_argument", message: "no transcript yet" };
    const offset = Number(args.offset ?? 0);
    const limit = Math.min(Number(args.limit ?? 50), 200);
    const segs = state.transcript.segments.slice(offset, offset + limit);
    return {
      total_segments: state.transcript.segments.length,
      offset,
      returned: segs.length,
      language: state.transcript.language,
      segments: segs.map(({ words: _words, ...lean }) => lean),
    };
  },

  get_transcript(args): Transcript | null {
    requireProject(args.project_id);
    return state.transcript ? clone(state.transcript) : null;
  },

  get_preferences(): Preferences {
    return clone(state.preferences);
  },

  set_preferences(args): Preferences {
    const incoming = (args.preferences ?? {}) as Partial<Preferences>;
    state.preferences = { ...state.preferences, ...clone(incoming) };
    return clone(state.preferences);
  },

  create_checkpoint(args): unknown {
    const p = requireProject(args.project_id);
    const cp = {
      id: `cp-${(p.checkpoints?.length ?? 0)}-${Math.floor(Date.now() / 1000)}`,
      name: String(args.name ?? "Checkpoint"),
      created_at: new Date().toISOString(),
      clips: clone(p.timeline.clips),
      global_padding: clone(p.timeline.global_padding),
      cut_count: p.timeline.cut_count,
      auto: false,
    };
    p.checkpoints = [...(p.checkpoints ?? []), cp];
    return { checkpoint: { id: cp.id, name: cp.name, created_at: cp.created_at, cut_count: cp.cut_count, auto: cp.auto } };
  },

  get_checkpoints(args): unknown {
    const p = requireProject(args.project_id);
    return {
      checkpoints: [...(p.checkpoints ?? [])].reverse().map((c: any) => ({
        id: c.id, name: c.name, created_at: c.created_at, cut_count: c.cut_count, auto: c.auto,
      })),
    };
  },

  restore_checkpoint(args): unknown {
    const p = requireProject(args.project_id);
    const cp = (p.checkpoints ?? []).find((c: any) => c.id === args.checkpoint_id);
    if (cp) { p.timeline.clips = clone(cp.clips); p.timeline.global_padding = clone(cp.global_padding); refreshTimeline(p); }
    const action = recordEdit(p, "set_clips", `Restored checkpoint “${cp?.name ?? ""}”`);
    emit("timeline-changed", { project_id: p.id });
    return { action };
  },

  delete_checkpoint(args): unknown {
    const p = requireProject(args.project_id);
    p.checkpoints = (p.checkpoints ?? []).filter((c: any) => c.id !== args.checkpoint_id);
    return { deleted: true };
  },

  get_history(args): unknown {
    requireProject(args.project_id);
    const applied = state.undoStack.map(({ action: a }, i) => ({
      id: a?.id ?? `u${i}`,
      kind: a?.kind ?? "edit",
      source: a?.source ?? "ui",
      description: a?.description ?? "Edit",
      timestamp: a?.timestamp ?? new Date().toISOString(),
      applied: true,
    }));
    const future = [...state.redoStack].reverse().map(({ action: a }, i) => ({
      id: a?.id ?? `r${i}`,
      kind: a?.kind ?? "edit",
      source: a?.source ?? "ui",
      description: a?.description ?? "Edit",
      timestamp: a?.timestamp ?? new Date().toISOString(),
      applied: false,
    }));
    return { entries: [...applied, ...future], current_index: state.undoStack.length - 1 };
  },

  jump_to(args): unknown {
    const p = requireProject(args.project_id);
    // Chronological: undoStack ++ redoStack.reversed(). Desired undo length
    // makes the target the newest applied (null = original = 0).
    const chrono = [...state.undoStack, ...[...state.redoStack].reverse()];
    let desired: number;
    if (args.action_id == null) {
      desired = 0;
    } else {
      const idx = chrono.findIndex((s) => s.action.id === args.action_id);
      if (idx < 0) throw { code: "not_found", message: "history entry" };
      desired = idx + 1;
    }
    while (state.undoStack.length > desired) {
      const s = state.undoStack.pop()!;
      state.redoStack.push(s);
    }
    while (state.undoStack.length < desired) {
      const s = state.redoStack.pop()!;
      state.undoStack.push(s);
    }
    if (desired > 0) p.timeline = clone(state.undoStack[desired - 1]!.timeline);
    refreshTimeline(p);
    emit("timeline-changed", { project_id: p.id });
    return { timeline: clone(p.timeline) };
  },

  undo_actions(args): unknown {
    const p = requireProject(args.project_id);
    const ids = (args.action_ids as string[]) ?? [];
    for (let i = 0; i < ids.length; i++) state.undoStack.pop();
    return { undone: ids.length, timeline: { cut_count: p.timeline.cut_count, included_duration_s: p.timeline.duration } };
  },

  undo(args) {
    const p = requireProject(args.project_id);
    const snap = state.undoStack.pop();
    if (!snap) return { action: null, timeline: clone(p.timeline) };
    state.redoStack.push({ timeline: clone(p.timeline), action: snap.action });
    p.timeline = snap.timeline;
    refreshTimeline(p);
    return { action: snap.action, timeline: clone(p.timeline) };
  },

  redo(args) {
    const p = requireProject(args.project_id);
    const snap = state.redoStack.pop();
    if (!snap) return { action: null, timeline: clone(p.timeline) };
    state.undoStack.push({ timeline: clone(p.timeline), action: snap.action });
    p.timeline = snap.timeline;
    refreshTimeline(p);
    return { action: snap.action, timeline: clone(p.timeline) };
  },

  // ---- Ingest & transcription ----------------------------------------------

  import_media(args): Media {
    const p = requireProject();
    const filePath = typeof args.file_path === "string" ? args.file_path : "/demo/talking-head.mp4";
    p.media = {
      id: "media-demo",
      file_path: filePath,
      duration: 0, // known after transcription in mock
      frame_rate: 29.97,
      width: 3840,
      height: 2160,
      audio_sample_rate: 48000,
      codec: "h264",
      imported_at: new Date().toISOString(),
    };
    p.updated_at = new Date().toISOString();
    return clone(p.media!);
  },

  async transcribe(args): Promise<Transcript> {
    const p = requireProject(args.project_id);
    await emitProgress("transcribe", p.id, [
      "Loading whisper model…",
      "Decoding audio…",
      "Transcribing 0:00–1:40…",
      "Transcribing 1:40–3:20…",
      "Transcribing 3:20–5:00…",
      "Aligning words…",
    ], 1600);

    const fixture = buildFixture();
    state.transcript = fixture.transcript;
    if (p.media) p.media.duration = fixture.duration;
    p.timeline = {
      id: "timeline-demo",
      clips: fixture.clips,
      global_padding: clone(state.preferences.default_padding),
      cut_count: fixture.clips.filter((c) => !c.included).length,
      duration: fixture.duration,
    };
    p.updated_at = new Date().toISOString();
    emit("transcript-changed", { project_id: p.id });
    emit("timeline-changed", { project_id: p.id });
    return clone(state.transcript);
  },

  // ---- Analysis -------------------------------------------------------------

  remove_silences(args): unknown {
    const p = requireProject(args.project_id);
    return {
      applied: 0,
      actions: [],
      note: "(browser mock) silence removal needs the desktop app's audio analysis",
      cut_count: p.timeline.cut_count,
      included_duration_s: p.timeline.duration,
      source_duration_s: p.timeline.duration,
    };
  },

  detect_silences(args): unknown {
    requireProject(args.project_id);
    const gaps = (state.transcript?.segments ?? [])
      .filter((s) => s.is_silence)
      .map((s) => ({ start: s.start, end: s.end, source: "transcript_only", confidence: 0.5 }));
    return { method: "transcript", segments: gaps };
  },

  detect_fillers(args) {
    requireProject(args.project_id);
    const custom = Array.isArray(args.custom_words) ? (args.custom_words as string[]) : [];
    const customSet = new Set(custom.map((w) => w.toLowerCase()));
    const ids =
      state.transcript?.segments
        .filter((s) => s.is_filler || s.words.some((w) => customSet.has(w.text.toLowerCase().replace(/[^a-z']/g, ""))))
        .map((s) => s.id) ?? [];
    return { word_ranges: [] as { start: number; end: number }[], segment_ids: ids };
  },

  detect_takes(args) {
    requireProject(args.project_id);
    return { take_groups: [] };
  },

  async generate_rough_cut(args) {
    const p = requireProject(args.project_id);
    const aggressiveness = args.aggressiveness === "aggressive" ? "aggressive" : "natural";
    await emitProgress("rough_cut", p.id, ["Detecting silences…", "Detecting fillers…", "Assembling cut…"], 900);

    const action = recordEdit(p, "generate_rough_cut", `Generated ${aggressiveness} rough cut`);
    const cuttable = new Set(
      state.transcript?.segments.filter((s) => s.is_filler || s.is_silence).map((s) => s.id) ?? [],
    );
    for (const clip of p.timeline.clips) {
      if (clip.linked_segment_ids.some((id) => cuttable.has(id))) clip.included = false;
      if (aggressiveness === "aggressive" && clip.source_out - clip.source_in < 3.5) clip.included = false;
    }
    refreshTimeline(p);
    // Flag a couple of still-included speech segments as Tier-2 suggestions
    // (repeated take + repetition) so the review UI is exercised in-browser.
    const speech = (state.transcript?.segments ?? []).filter(
      (s) => !s.is_silence && !s.is_filler && s.text,
    );
    p.suggestions = speech.slice(1, 3).map((s, i) => ({
      id: `sugg-${i}`,
      segment_ids: [s.id],
      start: s.start,
      end: s.end,
      reason: i === 0 ? "repeated_take" : "repetition",
      confidence: 0.85 - i * 0.05,
      duplicate_of: speech[0]?.id ?? null,
      preview: s.text.slice(0, 80),
    }));
    return {
      action,
      timeline: clone(p.timeline),
      cut_count: p.timeline.cut_count,
      suggestions: p.suggestions.length,
    };
  },

  // ---- Editing ---------------------------------------------------------------

  cut_range(args) {
    const p = requireProject(args.project_id);
    const start = Number(args.start), end = Number(args.end);
    const action = recordEdit(p, "cut_range", `Cut ${start.toFixed(2)}s–${end.toFixed(2)}s`);
    setRangeIncluded(p, start, end, false);
    refreshTimeline(p);
    return { action, timeline: clone(p.timeline) };
  },

  restore_range(args) {
    const p = requireProject(args.project_id);
    const start = Number(args.start), end = Number(args.end);
    const action = recordEdit(p, "restore_range", `Restored ${start.toFixed(2)}s–${end.toFixed(2)}s`);
    setRangeIncluded(p, start, end, true);
    refreshTimeline(p);
    return { action, timeline: clone(p.timeline) };
  },

  cut_by_transcript(args) {
    const p = requireProject(args.project_id);
    const ids = new Set((args.segment_ids as string[]) ?? []);
    const action = recordEdit(p, "cut_by_transcript", `Cut ${ids.size} transcript segment(s)`);
    for (const c of p.timeline.clips) {
      if (c.linked_segment_ids.some((id) => ids.has(id))) c.included = false;
    }
    refreshTimeline(p);
    return { action, timeline: clone(p.timeline) };
  },

  restore_by_transcript(args) {
    const p = requireProject(args.project_id);
    const ids = new Set((args.segment_ids as string[]) ?? []);
    const action = recordEdit(p, "restore_by_transcript", `Restored ${ids.size} transcript segment(s)`);
    for (const c of p.timeline.clips) {
      if (c.linked_segment_ids.some((id) => ids.has(id))) c.included = true;
    }
    refreshTimeline(p);
    return { action, timeline: clone(p.timeline) };
  },

  trim_clip(args) {
    const p = requireProject(args.project_id);
    const clip = p.timeline.clips.find((c) => c.id === args.clip_id);
    if (!clip) throw { code: "not_found", message: `Unknown clip ${String(args.clip_id)}` };
    const action = recordEdit(p, "trim_clip", `Trimmed clip ${clip.id}`);
    const newIn = Math.max(0, Number(args.new_source_in));
    const newOut = Math.min(p.timeline.duration, Number(args.new_source_out));
    if (newOut - newIn >= 0.05) {
      clip.source_in = Number(newIn.toFixed(3));
      clip.source_out = Number(newOut.toFixed(3));
    }
    refreshTimeline(p);
    return { action, timeline: clone(p.timeline) };
  },

  split_clip(args) {
    const p = requireProject(args.project_id);
    const clip = p.timeline.clips.find((c) => c.id === args.clip_id);
    if (!clip) throw { code: "not_found", message: `Unknown clip ${String(args.clip_id)}` };
    const at = Number(args.at_time);
    if (!(at > clip.source_in + 0.05 && at < clip.source_out - 0.05)) {
      throw { code: "invalid_args", message: "Split point must be inside the clip" };
    }
    const action = recordEdit(p, "split_clip", `Split clip ${clip.id} at ${at.toFixed(2)}s`);
    const right: Clip = {
      ...clone(clip),
      id: `clip-split-${Date.now().toString(36)}`,
      source_in: Number(at.toFixed(3)),
      origin: "split",
      order: clip.order + 1,
    };
    clip.source_out = Number(at.toFixed(3));
    clip.origin = "split";
    for (const c of p.timeline.clips) {
      if (c.order > clip.order) c.order += 1;
    }
    p.timeline.clips.push(right);
    p.timeline.clips.sort((a, b) => a.order - b.order);
    refreshTimeline(p);
    return { clips: [clone(clip), clone(right)], timeline: clone(p.timeline), action };
  },

  reorder_clip(args) {
    const p = requireProject(args.project_id);
    const clip = p.timeline.clips.find((c) => c.id === args.clip_id);
    if (!clip) throw { code: "not_found", message: `Unknown clip ${String(args.clip_id)}` };
    const action = recordEdit(p, "reorder_clip", `Reordered clip ${clip.id}`);
    clip.order = Number(args.new_order);
    p.timeline.clips.sort((a, b) => a.order - b.order);
    p.timeline.clips.forEach((c, i) => { c.order = i; });
    refreshTimeline(p);
    return { action, timeline: clone(p.timeline) };
  },

  set_global_padding(args) {
    const p = requireProject(args.project_id);
    const action = recordEdit(p, "set_global_padding", "Set global clip padding");
    p.timeline.global_padding = {
      start_s: Number(args.start_s),
      end_s: Number(args.end_s),
      linked: !!args.linked,
    };
    refreshTimeline(p);
    return { action, timeline: clone(p.timeline) };
  },

  /** Batch power tool: each op routes through the matching single-op handler
   *  above, mirroring the core (one EditAction per op, lean receipt). */
  async apply_edits(args) {
    const p = requireProject(args.project_id);
    const edits = Array.isArray(args.edits) ? (args.edits as Record<string, unknown>[]) : [];
    if (edits.length > 100) throw { code: "invalid_args", message: "max 100 edits per call" };
    const actions: unknown[] = [];
    for (const edit of edits) {
      const { type, ...fields } = edit;
      const name = type === "cut_segments" ? "cut_by_transcript"
        : type === "restore_segments" ? "restore_by_transcript"
        : String(type);
      const handler = tools[name];
      if (!handler) throw { code: "invalid_args", message: `unknown edit type ${String(type)}` };
      const res = (await handler({ ...fields, project_id: args.project_id })) as { action?: unknown };
      if (res?.action) actions.push(res.action);
    }
    const included = p.timeline.clips
      .filter((c) => c.included)
      .reduce((s, c) => s + (c.source_out - c.source_in), 0);
    return {
      applied: edits.length,
      actions,
      cut_count: p.timeline.cut_count,
      included_duration_s: Number(included.toFixed(3)),
      source_duration_s: p.timeline.duration,
    };
  },

  /** Duration planner: longest still-included segments go first (the core
   *  ranks by embedding centrality; the mock only mirrors the shape). */
  async plan_duration_cut(args) {
    const p = requireProject(args.project_id);
    const target = Number(args.target_duration_s);
    const includedOverlap = (start: number, end: number) =>
      p.timeline.clips
        .filter((c) => c.included)
        .reduce((acc, c) => acc + Math.max(0, Math.min(end, c.source_out) - Math.max(start, c.source_in)), 0);
    const before = p.timeline.clips
      .filter((c) => c.included)
      .reduce((acc, c) => acc + (c.source_out - c.source_in), 0);
    const candidates = (state.transcript?.segments ?? [])
      .filter((s) => !s.is_silence && s.text)
      .slice(2, -2)
      .map((s) => ({ id: s.id, saved: includedOverlap(s.start, s.end) }))
      .filter((c) => c.saved > 0.2)
      .sort((a, b) => b.saved - a.saved);
    const segment_ids: string[] = [];
    let projected = before;
    for (const c of candidates) {
      if (projected <= target) break;
      segment_ids.push(c.id);
      projected -= c.saved;
    }
    if (args.apply && segment_ids.length > 0) {
      const cut = (await tools.cut_by_transcript({ project_id: args.project_id, segment_ids })) as {
        action?: unknown;
      };
      const after = p.timeline.clips
        .filter((c) => c.included)
        .reduce((acc, c) => acc + (c.source_out - c.source_in), 0);
      return {
        applied: segment_ids.length,
        action: cut.action,
        before_s: before,
        included_duration_s: after,
        target_s: target,
        method: "heuristic",
        notes: "(browser mock) longest segments first",
      };
    }
    return {
      segment_ids,
      before_s: before,
      projected_after_s: Math.max(0, projected),
      target_s: target,
      method: "heuristic",
      notes: "(browser mock) longest segments first",
    };
  },

  attach_screen_media(args): unknown {
    requireProject(args.project_id);
    return { attached: true };
  },

  set_layout(args): unknown {
    const p = requireProject(args.project_id);
    p.layout = {
      mode: String(args.mode ?? p.layout.mode),
      shape: String(args.shape ?? p.layout.shape),
      corner: String(args.corner ?? p.layout.corner),
      size: Number(args.size ?? p.layout.size),
    };
    return clone(p.layout);
  },

  set_audio_offset(args): unknown {
    const p = requireProject(args.project_id);
    p.audio_offset_s = Number(args.offset_s ?? 0);
    return { audio_offset_s: p.audio_offset_s };
  },

  append_media(args): unknown {
    const p = requireProject(args.project_id);
    const extra = 30;
    const old = p.timeline.duration;
    p.timeline.duration += extra;
    p.timeline.clips.push({
      id: crypto.randomUUID(),
      source_in: old,
      source_out: old + extra,
      included: true,
      origin: "manual",
      order: p.timeline.clips.length,
      linked_segment_ids: [],
    });
    if (p.media) p.media.duration += extra;
    emit("timeline-changed", { project_id: p.id });
    return { media: clone(p.media), note: "(browser mock) appended 30s" };
  },

  outline_transcript(args): unknown {
    requireProject(args.project_id);
    const speech = (state.transcript?.segments ?? []).filter((s) => !s.is_silence && s.text);
    const third = Math.max(1, Math.floor(speech.length / 3));
    const beat = (segs: TranscriptSegment[], title: string, role: string) => ({
      title,
      summary: "(browser mock)",
      role,
      cut_priority: role === "tangent" ? 4 : 2,
      segment_ids: segs.map((s) => s.id),
      start: segs[0]?.start ?? 0,
      end: segs[segs.length - 1]?.end ?? 0,
    });
    return {
      method: "heuristic",
      beats: [
        beat(speech.slice(0, third), "Opening", "hook"),
        beat(speech.slice(third, third * 2), "Middle", "point"),
        beat(speech.slice(third * 2), "Closing", "recap"),
      ],
    };
  },

  review_flow(args): unknown {
    requireProject(args.project_id);
    return { coherent: true, boundaries_checked: 0, issues: [], method: "deterministic" };
  },

  async story_edit(args): Promise<unknown> {
    const p = requireProject(args.project_id);
    const before = p.timeline.clips
      .filter((c) => c.included)
      .reduce((acc, c) => acc + (c.source_out - c.source_in), 0);
    return {
      actions: [],
      summary: "(browser mock) story edit reviewed the outline and kept every beat.",
      before_s: before,
      after_s: before,
      coherent: true,
      issues_remaining: 0,
    };
  },

  // ---- Semantic / LLM ----------------------------------------------------------

  find_segments(args) {
    requireProject(args.project_id);
    const q = String(args.query ?? "").toLowerCase();
    const segments = (state.transcript?.segments ?? [])
      .filter((s) => s.text.toLowerCase().includes(q))
      .map((s) => ({
        id: s.id,
        start: s.start,
        end: s.end,
        text: s.text,
        is_filler: s.is_filler,
        is_silence: s.is_silence,
        take_group_id: s.take_group_id,
        is_best_take: s.is_best_take,
        score: 0.5,
      }));
    return { segments, method: "bm25" };
  },

  async apply_instruction(args) {
    const p = requireProject(args.project_id);
    const instruction = String(args.instruction ?? "");
    const lower = instruction.toLowerCase();
    let step = 0;
    const say = (e: Omit<AgentStepEvent, "project_id" | "step">) => {
      step += 1;
      emit("agent-step", { project_id: p.id, step, ...e } satisfies AgentStepEvent);
    };

    say({ kind: "thinking", text: "Reading the transcript and timeline to plan the edit…" });
    await sleep(450);

    const actions: EditAction[] = [];
    let summary: string;

    const restore = lower.includes("restore") || lower.includes("bring back") || lower.includes("undo");

    if (restore) {
      say({ kind: "tool_call", tool: "undo", args: { project_id: p.id } });
      await sleep(350);
      const res = tools.undo!({ project_id: p.id }) as { action: EditAction | null };
      say({ kind: "tool_result", tool: "undo", result: { action: res.action?.description ?? null } });
      if (res.action) actions.push(res.action);
      summary = res.action ? `Reverted: ${res.action.description}.` : "Nothing to revert.";
    } else if (lower.includes("silence")) {
      say({ kind: "tool_call", tool: "detect_silences", args: { project_id: p.id } });
      await sleep(400);
      const det = tools.detect_silences!({ project_id: p.id }) as { segments: { start: number; end: number }[] };
      say({ kind: "tool_result", tool: "detect_silences", result: { count: det.segments.length } });
      const ids = state.transcript?.segments.filter((s) => s.is_silence).map((s) => s.id) ?? [];
      say({ kind: "tool_call", tool: "cut_by_transcript", args: { project_id: p.id, segment_ids: ids } });
      await sleep(400);
      const res = tools.cut_by_transcript!({ project_id: p.id, segment_ids: ids }) as { action: EditAction };
      say({ kind: "tool_result", tool: "cut_by_transcript", result: { description: res.action.description } });
      actions.push(res.action);
      summary = `Cut ${ids.length} silence gap(s) from the timeline.`;
    } else if (lower.includes("filler") || lower.includes("um") || lower.includes("uh")) {
      say({ kind: "tool_call", tool: "detect_fillers", args: { project_id: p.id } });
      await sleep(400);
      const det = tools.detect_fillers!({ project_id: p.id }) as { segment_ids: string[] };
      say({ kind: "tool_result", tool: "detect_fillers", result: { segment_ids: det.segment_ids } });
      say({ kind: "tool_call", tool: "cut_by_transcript", args: { project_id: p.id, segment_ids: det.segment_ids } });
      await sleep(400);
      const res = tools.cut_by_transcript!({ project_id: p.id, segment_ids: det.segment_ids }) as { action: EditAction };
      say({ kind: "tool_result", tool: "cut_by_transcript", result: { description: res.action.description } });
      actions.push(res.action);
      summary = `Removed ${det.segment_ids.length} filler segment(s).`;
    } else {
      // Generic path: try to find matching segments and cut them.
      const keyword = lower.replace(/(cut|remove|delete|the|all|parts?|about|where i say)/g, " ").trim().split(/\s+/)[0] ?? "";
      say({ kind: "tool_call", tool: "find_segments", args: { project_id: p.id, query: keyword } });
      await sleep(450);
      const found = tools.find_segments!({ project_id: p.id, query: keyword }) as { segments: TranscriptSegment[] };
      say({ kind: "tool_result", tool: "find_segments", result: { count: found.segments.length, query: keyword } });
      if (found.segments.length > 0 && keyword.length > 1) {
        const ids = found.segments.slice(0, 5).map((s) => s.id);
        say({ kind: "tool_call", tool: "cut_by_transcript", args: { project_id: p.id, segment_ids: ids } });
        await sleep(400);
        const res = tools.cut_by_transcript!({ project_id: p.id, segment_ids: ids }) as { action: EditAction };
        say({ kind: "tool_result", tool: "cut_by_transcript", result: { description: res.action.description } });
        actions.push(res.action);
        summary = `Found ${found.segments.length} segment(s) mentioning “${keyword}” and cut ${ids.length}.`;
      } else {
        summary = "I could not map that instruction to an edit. Try “remove the filler words” or “cut the silences”.";
      }
    }

    say({ kind: "final", text: summary });
    return { actions, summary };
  },

  // ---- Metadata -------------------------------------------------------------

  generate_chapters(args) {
    requireProject(args.project_id);
    return {
      chapters: [
        { title: "Why I quit", start: 0 },
        { title: "Tracking my time", start: 55 },
        { title: "Side projects", start: 110 },
        { title: "The 12-month deadline", start: 170 },
        { title: "Lessons learned", start: 240 },
      ],
    };
  },

  generate_title_description(args) {
    requireProject(args.project_id);
    return {
      titles: ["Why I Finally Quit My Job", "I Quit — Here's the Data", "Quit With Data, Not Rage"],
      description: "The honest story of leaving a senior engineering job, with the numbers behind the decision.",
    };
  },

  generate_captions(args) {
    requireProject(args.project_id);
    const segs = state.transcript?.segments.filter((s) => !s.is_silence) ?? [];
    const fmt = (t: number) => {
      const h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = Math.floor(t % 60), ms = Math.round((t % 1) * 1000);
      const pad = (n: number, l = 2) => String(n).padStart(l, "0");
      return `${pad(h)}:${pad(m)}:${pad(s)},${pad(ms, 3)}`;
    };
    const srt = segs.map((s, i) => `${i + 1}\n${fmt(s.start)} --> ${fmt(s.end)}\n${s.text}\n`).join("\n");
    return { srt };
  },

  // ---- Export -----------------------------------------------------------------

  async export(args) {
    const p = requireProject(args.project_id);
    await emitProgress("export", p.id, ["Resolving timeline…", "Writing file…"], 700);
    return { path: String(args.out_path ?? "~/Downloads/export.xml") };
  },

  // ---- MCP client (opt-in) -------------------------------------------------------

  connect_external() {
    throw { code: "unsupported", message: "External connections are not available in mock mode" };
  },

  escalate_to_frontier() {
    throw { code: "unsupported", message: "Frontier escalation is not available in mock mode" };
  },
};

export async function mockCallTool(name: string, args: Args): Promise<unknown> {
  await sleep(30 + Math.random() * 50);
  const tool = tools[name];
  if (!tool) throw { code: "unknown_tool", message: `Unknown tool: ${name}` };
  return tool(args);
}
