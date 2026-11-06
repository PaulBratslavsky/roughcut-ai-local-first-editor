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
  ClipBatchError,
  ClipCandidate,
  ClipExportResult,
  ClipFormat,
  HostedKeyResult,
  KeyCheck,
  ModelMode,
  ModelStatus,
  RouteReason,
  SavedClip,
  SavedClipSummary,
  SourceRange,
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
// Mock switches: walk the failure paths in the browser build. Turn one on
// for a page load with a URL parameter (http://localhost:5173/?nomodel), or
// for every load with localStorage.setItem("rc-mock-nomodel", "1") in the
// console (removeItem to turn it off), then reload.
//   nomodel     the on-device model is unreachable: find_clips ranks by the
//               rule-based score only and reports method "heuristic"
//               (bulk-shorts CUJ 6); setup reports inference unreachable.
//   failexport  export_clips fails every second clip, so the batch's
//               "one failure doesn't stop the rest" path shows.
//   failtranscribe  transcribe fails part-way, with no final progress event
//               (like the core), so an import's transcription error shows.
//   failimport  create_project fails, as if the file couldn't be probed.
//   nohosted    Anthropic is unreachable: a key check can't verify (Save
//               keeps the key, "saved; couldn't verify while offline"), and
//               Hosted requests fail — except a multi-request run's first
//               one, as if the Wi-Fi dropped mid-run: a scan or clean-up
//               finishes on the local model (mixed scorers, chip HOSTED →
//               LOCAL (offline), hosted CUJ 3), while a chat or story edit
//               stops rather than switch models half-way. One-request steps
//               (outline, flow review, chapters, titles) run locally.
//   badkey      Anthropic rejects the key: Save / Test report it, and Hosted
//               runs stop with hosted_key_rejected (hosted CUJ 4). Read on
//               every call, so localStorage.setItem("rc-mock-badkey", "1")
//               revokes the key of an already-Hosted session without reload.
//   noaligner   the word alignment model isn't downloaded: Setup offers it,
//               and Fix word timings only trims (with a note saying why).
// ---------------------------------------------------------------------------

export function mockSwitch(
  name: "nomodel" | "failexport" | "failtranscribe" | "failimport" | "nohosted" | "badkey" | "noaligner",
): boolean {
  try {
    if (new URLSearchParams(window.location.search).has(name)) return true;
    return localStorage.getItem(`rc-mock-${name}`) === "1";
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// The LOCAL / HOSTED switch. In the app these are Tauri shell commands, not
// registry tools (ADR-0004; ipc/api.ts calls them); this mirrors
// core/src/engine/route.rs. The key lives in memory; Hosted works unless
// the nohosted / badkey switches say otherwise.
// ---------------------------------------------------------------------------

const HOSTED_MODELS: [id: string, name: string][] = [
  ["claude-opus-5-5", "Claude Opus 5.5"],
  ["claude-sonnet-5-5", "Claude Sonnet 5.5"],
  ["claude-haiku-5-5", "Claude Haiku 5.5"],
];
const hostedName = (id: string) => HOSTED_MODELS.find(([m]) => m === id)?.[1] ?? id;
const KEY_REJECTED = "Anthropic rejected the API key (401). Update it in Setup.";

const route = {
  key: null as string | null,
  effective: null as ModelMode | null,
  reason: null as RouteReason | null,
  lastOkAt: null as string | null,
  lastError: null as string | null,
  /** What the chip was last told, so model-route fires on a change only. */
  sent: null as string | null,
};

/** Shell commands reject with `{ error: { code, message } }`. */
const commandError = (code: string, message: string) => ({ error: { code, message } });

export function mockModelStatus(): ModelStatus {
  const p = state.preferences;
  const local = p.model_mode === "local";
  return {
    mode: p.model_mode,
    hosted_model: p.hosted_model,
    hosted_model_name: hostedName(p.hosted_model),
    key_present: route.key !== null,
    consented: p.hosted_consent_at !== null,
    effective: local ? "local" : (route.effective ?? p.model_mode),
    reason: local ? null : route.reason,
    last_ok_at: route.lastOkAt,
    last_error: route.lastError,
  };
}

function sendRoute(mode: ModelMode, effective: ModelMode, reason: RouteReason | null): void {
  route.sent = `${effective}/${reason}`;
  emit("model-route", reason ? { mode, effective, reason } : { mode, effective });
}

/** After a Hosted run's request: where it ran, and tell the chip on a change. */
function recordRoute(effective: ModelMode, reason: RouteReason | null, opts: { ok?: boolean; error?: string } = {}): void {
  route.effective = effective;
  route.reason = reason;
  if (opts.ok) {
    route.lastOkAt = new Date().toISOString();
    route.lastError = null;
  }
  if (opts.error) route.lastError = opts.error;
  if (route.sent !== `${effective}/${reason}`) sendRoute(state.preferences.model_mode, effective, reason);
}

/** A Hosted request with a revoked key: the run stops, nothing changes. */
function hostedKeyRejected(): never {
  recordRoute("hosted", "key_rejected", { error: KEY_REJECTED });
  throw { code: "hosted_key_rejected", message: KEY_REJECTED };
}

/** One AI run's route, after the core's HostedRouter. Local mode: the local
 *  model. Hosted: Claude, until Anthropic is unreachable (a batch run pins
 *  Local and goes on; a conversation that already had a Claude step stops)
 *  or the user switches to Local mid-run (same rule). A revoked key stops
 *  any run. `next()` before each AI request says where it runs — null when
 *  no model is reachable (the caller keeps its rule-based result). */
class MockRun {
  private pin: "hosted" | "local" | null = null;
  private requests = 0;
  constructor(
    private readonly policy: "batch" | "conversation",
    /** More than one request: under `nohosted` the first still gets through. */
    private readonly multi: boolean,
  ) {}

  private stop(why: string): never {
    throw { code: "unavailable", message: `unavailable: ${why}; stopped so this run doesn't switch models half-way` };
  }

  private local(): "local" | null {
    this.pin = "local";
    return mockSwitch("nomodel") ? null : "local";
  }

  next(): "hosted" | "local" | null {
    const n = ++this.requests;
    if (state.preferences.model_mode === "local") {
      if (this.pin === "hosted" && this.policy === "conversation") this.stop("switched to Local");
      return this.local();
    }
    if (this.pin === "local") return this.local();
    if (mockSwitch("badkey")) hostedKeyRejected();
    if (mockSwitch("nohosted") && !(this.multi && n === 1)) {
      recordRoute("local", "offline", { error: "mock: Anthropic unreachable" });
      if (this.pin === "hosted" && this.policy === "conversation") {
        this.stop("Anthropic stopped answering (mock: no network)");
      }
      return this.local();
    }
    this.pin = "hosted";
    recordRoute("hosted", null, { ok: true });
    return "hosted";
  }
}

/** The transcript clean-up that follows every transcription (in the
 *  background, two chunks here). A Hosted failure is recorded for Setup and
 *  the chip (last_error, "key rejected"); the transcript stays as it is. */
async function mockCleanup(): Promise<void> {
  const run = new MockRun("batch", true);
  try {
    for (let chunk = 0; chunk < 2; chunk++) {
      await sleep(250);
      run.next();
    }
  } catch {
    /* recorded by the route: last_error, and the chip for a rejected key */
  }
}

export async function mockSetModelMode(mode: ModelMode, consent: boolean): Promise<ModelStatus> {
  await sleep(40);
  const p = state.preferences;
  if (mode !== "local" && mode !== "hosted") throw commandError("invalid_argument", `invalid argument: unknown mode ${String(mode)}`);
  if (mode === "hosted") {
    if (route.key === null) throw commandError("hosted_no_key", "add an Anthropic API key in Setup first");
    if (p.hosted_consent_at === null) {
      if (!consent) throw commandError("hosted_not_consented", "confirm what Hosted mode sends before turning it on");
      p.hosted_consent_at = new Date().toISOString();
    }
  }
  p.model_mode = mode;
  route.effective = null;
  route.reason = null;
  sendRoute(mode, mode, null);
  return mockModelStatus();
}

export async function mockSetHostedModel(model: string): Promise<ModelStatus> {
  await sleep(40);
  if (!HOSTED_MODELS.some(([m]) => m === model)) {
    throw commandError(
      "invalid_argument",
      `invalid argument: unknown hosted model ${model}; one of ${HOSTED_MODELS.map(([m]) => m).join(", ")}`,
    );
  }
  state.preferences.hosted_model = model;
  return mockModelStatus();
}

const MOCK_LATENCY_MS = 120 as unknown as bigint; // ts-rs types u64 as bigint; JSON carries a number

export async function mockSetHostedKey(key: string | null): Promise<HostedKeyResult> {
  await sleep(40);
  const p = state.preferences;
  if (key === null) {
    route.key = null;
    if (p.model_mode === "hosted") {
      p.model_mode = "local";
      sendRoute("local", "local", null);
    }
    return { status: mockModelStatus(), check: null, saved: false };
  }
  const k = key.trim();
  if (!k) throw commandError("invalid_argument", "invalid argument: the API key is empty");
  await sleep(160);
  let check: KeyCheck;
  let save: boolean;
  if (mockSwitch("badkey")) {
    check = { ok: false, latency_ms: null, error: KEY_REJECTED };
    save = false;
  } else if (mockSwitch("nohosted")) {
    check = { ok: false, latency_ms: null, error: "saved; couldn't verify while offline (mock: Anthropic unreachable)" };
    save = true;
  } else {
    check = { ok: true, latency_ms: MOCK_LATENCY_MS, error: null };
    save = true;
  }
  if (save) {
    route.key = k;
    if (check.ok) {
      route.lastOkAt = new Date().toISOString();
      route.lastError = null;
      if (route.reason === "key_rejected" || route.reason === "no_key") route.reason = null;
    }
  }
  return { status: mockModelStatus(), check, saved: save };
}

export async function mockCheckHostedKey(): Promise<KeyCheck> {
  await sleep(160);
  if (route.key === null) return { ok: false, latency_ms: null, error: "no API key is stored" };
  if (mockSwitch("badkey")) {
    route.lastError = KEY_REJECTED;
    return { ok: false, latency_ms: null, error: KEY_REJECTED };
  }
  if (mockSwitch("nohosted")) {
    const e = "unavailable: Anthropic is unreachable (mock: no network)";
    route.lastError = e;
    return { ok: false, latency_ms: null, error: e };
  }
  route.lastOkAt = new Date().toISOString();
  route.lastError = null;
  return { ok: true, latency_ms: MOCK_LATENCY_MS, error: null };
}

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
    words_trimmed: false,
    words_aligned: false,
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
    model_mode: "local",
    hosted_model: "claude-opus-5-5",
    hosted_consent_at: null,
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

function includedWithin(tl: Timeline, start: number, end: number): number {
  return tl.clips
    .filter((c) => c.included)
    .reduce((sum, c) => sum + Math.max(0, Math.min(c.source_out, end) - Math.max(c.source_in, start)), 0);
}

/** The main cut restricted to `ranges` (the core's Timeline::restricted_to). */
function restrictTimeline(tl: Timeline, ranges: SourceRange[]): Timeline {
  const inside = (a: number, b: number) => ranges.some((r) => a >= r.start && b <= r.end);
  const edgesAll = ranges.flatMap((r) => [r.start, r.end]);
  const clips: Clip[] = [];
  for (const c of tl.clips) {
    const cuts = edgesAll.filter((t) => t > c.source_in && t < c.source_out).sort((x, y) => x - y);
    const edges = [c.source_in, ...cuts, c.source_out];
    for (let i = 0; i < edges.length - 1; i++) {
      const [a, b] = [edges[i], edges[i + 1]];
      clips.push({ ...clone(c), id: `${c.id}-s${clips.length}`, source_in: a, source_out: b, included: c.included && inside(a, b) });
    }
  }
  clips.forEach((c, i) => (c.order = i));
  return { ...clone(tl), id: `${tl.id}-clip`, clips, cut_count: clips.filter((c) => !c.included).length };
}

/** Sorted, merged pieces; throws like the core on a bad range. */
function normalizeRanges(p: Project, ranges: SourceRange[]): SourceRange[] {
  if (ranges.length === 0) throw { code: "invalid_arg", message: "a clip needs at least one range" };
  const sorted = ranges
    .map((r) => {
      if (!(r.start < r.end) || r.start < 0 || r.start >= p.timeline.duration) {
        throw { code: "invalid_arg", message: `range ${r.start}s–${r.end}s is invalid` };
      }
      return { start: r.start, end: Math.min(r.end, p.timeline.duration) };
    })
    .sort((a, b) => a.start - b.start);
  const merged: SourceRange[] = [];
  for (const r of sorted) {
    const last = merged[merged.length - 1];
    if (last && r.start <= last.end) last.end = Math.max(last.end, r.end);
    else merged.push({ ...r });
  }
  // Like the core's set_ranges: pieces cut from the main edit can't be a clip.
  if (merged.reduce((sum, r) => sum + includedWithin(p.timeline, r.start, r.end), 0) < 1) {
    throw { code: "invalid_arg", message: "those ranges have under a second of kept footage in the current cut" };
  }
  return merged;
}

/** Seconds two piece lists share, over the shorter one's source seconds —
 *  the core's "same moment" test (over MAX_OVERLAP = 30% is the same). */
function pieceOverlap(a: SourceRange[], b: SourceRange[]): number {
  const len = (rs: SourceRange[]) => rs.reduce((s, r) => s + Math.max(0, r.end - r.start), 0);
  let shared = 0;
  for (const x of a) for (const y of b) shared += Math.max(0, Math.min(x.end, y.end) - Math.max(x.start, y.start));
  const shorter = Math.min(len(a), len(b));
  return shorter > 0 ? shared / shorter : 0;
}
const MAX_OVERLAP = 0.3;

/** A saved clip made from a candidate (its pieces, validated first). */
function clipFromCandidate(p: Project, c: ClipCandidate, format: ClipFormat): SavedClip {
  const pieces = normalizeRanges(p, c.pieces.length > 0 ? c.pieces : [{ start: c.start, end: c.end }]);
  return {
    id: `clip-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
    title: c.title || `Clip ${p.saved_clips.length + 1}`,
    ranges: pieces,
    format,
    crop_x: 0.5,
    start: pieces[0]!.start,
    end: pieces[pieces.length - 1]!.end,
    timeline: restrictTimeline(p.timeline, pieces),
    score: c.score,
    reason: c.reason,
    hook: c.hook,
    created_at: new Date().toISOString(),
  };
}

/** The core's batch file name: `NN-<slug>.<ext>` (see export_clips). */
function batchFileName(position: number, total: number, title: string, ext: string): string {
  const width = total > 99 ? 3 : 2;
  const slug = title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40).replace(/-+$/, "") || "clip";
  return `${String(position).padStart(width, "0")}-${slug}.${ext}`;
}

/** Files the mock "wrote", per folder, so a second batch gets `-2` names. */
const mockFolders = new Map<string, Set<string>>();

/** Projects with a find_clips run in flight (the core allows one each). */
const mockScans = new Set<string>();

/** The core's `format` argument: aliases accepted, anything else an error. */
function argFormat(args: Args): ClipFormat | undefined {
  const f = args.format;
  if (f == null || f === "") return undefined;
  if (f === "landscape" || f === "16:9") return "landscape";
  if (f === "vertical" || f === "9:16" || f === "short") return "vertical";
  throw { code: "invalid_arg", message: `'format' must be "landscape" (16:9) or "vertical" (9:16), got "${String(f)}"` };
}

/** Pieces from ranges / segment_ids / start+end, whichever the caller sent. */
function argRanges(args: Args): SourceRange[] | null {
  if (Array.isArray(args.ranges)) return (args.ranges as SourceRange[]).map((r) => ({ start: Number(r.start), end: Number(r.end) }));
  if (Array.isArray(args.segment_ids)) {
    return (args.segment_ids as string[]).map((id) => {
      const seg = state.transcript?.segments.find((s) => s.id === id);
      if (!seg) throw { code: "not_found", message: `segment ${id}` };
      return { start: seg.start, end: seg.end };
    });
  }
  if (args.start != null && args.end != null) return [{ start: Number(args.start), end: Number(args.end) }];
  return null;
}

function clipSummary(c: SavedClip): SavedClipSummary {
  return {
    id: c.id,
    title: c.title,
    ranges: clone(c.ranges),
    format: c.format,
    crop_x: c.crop_x,
    start: c.start,
    end: c.end,
    duration: c.ranges.reduce((sum, r) => sum + includedWithin(c.timeline, r.start, r.end), 0),
    score: c.score,
    reason: c.reason,
    hook: c.hook,
    created_at: c.created_at,
  };
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

/** Demo footage has no real frames: the mock "finds" a speaker a little
 *  right of centre, so face framing visibly does something in the browser. */
const MOCK_FACE_CROP_X = 0.62;

/** Mirror of the core's background face pass for a clip that just became
 *  9:16 — moves the crop only if nobody moved it meanwhile. */
function mockAutoCenter(projectId: string, clip: { crop_x: number }): void {
  const before = clip.crop_x;
  setTimeout(() => {
    if (clip.crop_x !== before) return;
    clip.crop_x = MOCK_FACE_CROP_X;
    emit("clips-changed", { project_id: projectId });
  }, 900);
}

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

function progress(task: ProgressEvent["task"], projectId: string, fraction: number, message: string): void {
  emit("progress", { task, project_id: projectId, fraction, message } satisfies ProgressEvent);
}

/** Story beats the mock pretends the outline found: equal stretches. */
const MOCK_BEATS = 6;

/** find_clips, after the core's clip_finder: sentence windows of the current
 *  cut and a rule score; with the model "reachable" a seeded pseudo-model is
 *  blended in like the core's 0.8 × model + 0.2 × rule, so scores spread and
 *  `min_score` drops some. `scan` fills the pool beat by beat (every beat's
 *  best before any beat's second) and leaves out moments already saved as
 *  clips — before pooling and again when writing; a plain run never does. */
async function mockFindClips(p: Project, t: Transcript, args: Args): Promise<unknown> {
  const localUp = !mockSwitch("nomodel");
  // healthy(): in Hosted mode a key is enough (a missing or bad one must fail
  // visibly, not fall back to rules); in Local mode the local server.
  const model = state.preferences.model_mode === "hosted" || localUp;
  const scan = args.scan === true;
  const rawCount = Number(args.count ?? 8);
  const count = Math.min(60, Math.max(1, Number.isFinite(rawCount) ? Math.round(rawCount) : 8));
  const minS = Math.max(3, Number(args.min_s ?? 15));
  const maxS = Math.max(Number(args.max_s ?? 60), minS + 1);
  const minScore = args.min_score != null && Number.isFinite(Number(args.min_score)) ? Number(args.min_score) : null;
  const theme = String(args.theme ?? "").trim();
  const speech = t.segments.filter((s) => !s.is_silence && s.text.trim() !== "");
  if (speech.length === 0) throw { code: "invalid_arg", message: "transcript has no speech" };

  progress("find_clips", p.id, 0.05, "outlining the video…");
  await sleep(scan ? 600 : 300);

  const kept = (s: TranscriptSegment) => {
    const mid = (s.start + s.end) / 2;
    return p.timeline.clips.some((c) => c.included && c.source_in <= mid && mid < c.source_out);
  };
  const usable = speech.map((s) => !s.is_filler && kept(s));
  const dangling = ["and", "but", "so", "because", "which", "also", "anyway", "then", "or"];
  const firstWord = (s: string) => s.split(/\s+/)[0]?.toLowerCase().replace(/[^a-z']/g, "") ?? "";
  type W = {
    first: number; last: number; start: number; end: number; duration: number;
    rule: number; score: number; judged: boolean; beat: number; scorer: string | null;
  };
  const total = Math.max(1, p.timeline.duration);
  let windows: W[] = [];
  for (let i = 0; i < speech.length; i++) {
    if (!usable[i]) continue;
    for (let j = i; j < speech.length; j++) {
      const start = speech[i].start, end = speech[j].end;
      const duration = includedWithin(p.timeline, start, end);
      if (duration > maxS) break;
      if (!usable[j] || duration < minS) continue;
      const open = speech[i].text.trim();
      let rule = 60;
      if (dangling.includes(firstWord(open))) rule -= 20;
      if (open.includes("?")) rule += 12;
      if (/[.!?]$/.test(speech[j].text.trim())) rule += 8;
      rule += 10 - Math.min(10, Math.abs(duration - 35) / 3);
      const beat = Math.min(MOCK_BEATS - 1, Math.floor((start / total) * MOCK_BEATS));
      windows.push({ first: i, last: j, start, end, duration, rule, score: rule, judged: false, beat, scorer: null });
    }
  }

  const savedOverlap = (w: W) => (p.saved_clips ?? []).some((c) => pieceOverlap([w], c.ranges) > MAX_OVERLAP);
  if (scan && windows.length > 0) {
    windows = windows.filter((w) => !savedOverlap(w));
    if (windows.length === 0) {
      p.clip_candidates = [];
      progress("find_clips", p.id, 1, "every moment that fits is already saved as a clip");
      emit("clips-changed", { project_id: p.id });
      return { candidates: [], method: model ? "llm" : "heuristic", theme: theme || null };
    }
  }
  if (windows.length === 0) {
    throw {
      code: "invalid_arg",
      message: `no ${minS.toFixed(0)}-${maxS.toFixed(0)}s stretch of speech in the current cut — widen the length range or restore cut material`,
    };
  }
  if (theme) {
    progress("find_clips", p.id, 0.2, `searching for “${theme}”…`);
    // Keyword stand-in for the core's hybrid search.
    const words = theme.toLowerCase().split(/\W+/).filter((w) => w.length > 2);
    windows = windows.filter((w) =>
      speech.slice(w.first, w.last + 1).some((s) => words.some((x) => s.text.toLowerCase().includes(x))),
    );
    if (windows.length === 0) {
      throw { code: "invalid_arg", message: `nothing in the current cut matches the theme “${theme}” — try different words` };
    }
  }

  // The core's diversify: judged first, then score, then earlier; skip a
  // moment that is the same as one already picked.
  const ranked = (ws: W[]) =>
    [...ws].sort((a, b) => Number(b.judged) - Number(a.judged) || b.score - a.score || a.start - b.start);
  const clashes = (w: W, taken: W[]) => taken.some((q) => pieceOverlap([q], [w]) > MAX_OVERLAP);
  const diversify = (ws: W[], cap: number) => {
    const out: W[] = [];
    for (const w of ranked(ws)) {
      if (out.length >= cap) break;
      if (!clashes(w, out)) out.push(w);
    }
    return out;
  };
  let pool: W[];
  if (!scan) {
    pool = diversify(windows, Math.max(24, count));
  } else {
    // round_robin_pool: one window per beat per round — a beat's next one
    // that isn't a moment already taken.
    const lanes = Array.from({ length: MOCK_BEATS }, (_, b) => diversify(windows.filter((w) => w.beat === b), Infinity))
      .filter((ws) => ws.length > 0)
      .map((ws) => ({ ws, next: 0 }));
    pool = [];
    for (;;) {
      const round: W[] = [];
      for (const lane of lanes) {
        while (lane.next < lane.ws.length) {
          const w = lane.ws[lane.next++]!;
          if (!clashes(w, pool) && !clashes(w, round)) {
            round.push(w);
            break;
          }
        }
      }
      if (round.length === 0) break;
      round.sort((a, b) => b.score - a.score);
      pool.push(...round);
    }
  }

  // Picked topic by topic, like the core's pick_all: a scan asks about every
  // topic (here, a beat), a plain run only the best few (choose_regions). One
  // route per request like the core's HostedRouter: a Hosted run pins Local
  // once Anthropic is unreachable or the user switches to Local, and never
  // goes back within the run; a rejected key stops it. A topic whose request
  // fails keeps its rule scores.
  const localName = state.preferences.inference_model;
  if (model) {
    const best = (beat: number) => Math.max(...pool.filter((w) => w.beat === beat).map((w) => w.rule));
    let topics = [...new Set(pool.map((w) => w.beat))].sort((a, b) => a - b);
    const cap = scan ? Infinity : Math.min(12, Math.max(4, Math.ceil(count / 2)));
    if (topics.length > cap) {
      topics = [...topics].sort((a, b) => best(b) - best(a)).slice(0, cap).sort((a, b) => a - b);
    }
    // The core's model_label, read once before the first topic.
    const label =
      state.preferences.model_mode === "hosted" && route.effective !== "local"
        ? hostedName(state.preferences.hosted_model)
        : "the on-device model";
    const run = new MockRun("batch", topics.length > 1);
    for (const [n, beat] of topics.entries()) {
      progress(
        "find_clips",
        p.id,
        0.3 + (0.65 * n) / topics.length,
        `picking shorts with ${label} — topic ${n + 1}/${topics.length}`,
      );
      const by = run.next();
      await sleep((scan ? 2400 : 500) / topics.length);
      if (!by) continue;
      for (const w of pool.filter((w) => w.beat === beat)) {
        const r = mulberry32(w.first * 7919 + w.last * 104729 + 17)();
        const judge = Math.min(98, Math.max(5, w.rule + (r - 0.5) * 80));
        w.score = 0.8 * judge + 0.2 * w.rule;
        w.judged = true;
        w.scorer = by === "hosted" ? hostedName(state.preferences.hosted_model) : localName;
      }
    }
  } else {
    await sleep(scan ? 900 : 300);
  }
  const method = pool.some((w) => w.judged) ? "llm" : "heuristic";

  // min_score drops what the model judged below it; unjudged windows stay.
  let picked = diversify(
    pool.filter((w) => !(w.judged && minScore !== null && w.score < minScore)),
    count,
  );
  // Written in one go at the end: a scan also drops moments saved while it ran.
  if (scan) picked = picked.filter((w) => !savedOverlap(w));
  p.clip_candidates = picked.map((w, i): ClipCandidate => {
    const open = speech[w.first].text.trim();
    const hook = open.replace(/^((um|uh|er|erm|ah|hmm)[,.…]?\s+)+/i, "").replace(/^./, (c) => c.toUpperCase());
    const why: string[] = [];
    if (theme) why.push("on theme");
    if (open.includes("?")) why.push("opens on a question");
    else if (!dangling.includes(firstWord(open))) why.push("cold open stands alone");
    if (/[.!?]$/.test(speech[w.last].text.trim())) why.push("ends on a full sentence");
    const reasons = why.join(", ") || "stands alone";
    return {
      id: `cand-${Date.now()}-${i}`,
      start: w.start,
      end: w.end,
      pieces: [{ start: w.start, end: w.end }],
      duration: w.duration,
      title: hook.split(/\s+/).slice(0, 8).join(" ").replace(/[,.;:]$/, ""),
      hook,
      reason: w.judged ? `${reasons[0]!.toUpperCase()}${reasons.slice(1)} (mock model)` : `Rule-based: ${reasons}`,
      score: Math.round(Math.min(100, Math.max(0, w.score)) * 10) / 10,
      segment_ids: speech.slice(w.first, w.last + 1).map((s) => s.id),
      method,
      theme: theme || null,
      scorer: w.scorer,
    };
  });
  progress("find_clips", p.id, 1, `found ${p.clip_candidates.length} clip candidates`);
  emit("clips-changed", { project_id: p.id });
  return { candidates: clone(p.clip_candidates), method, theme: theme || null };
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
    if (mockSwitch("failimport")) {
      throw { code: "invalid_argument", message: "ffprobe could not read the file (mock failure)" };
    }
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
      saved_clips: [],
      clip_candidates: [],
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

  get_media_assets(args): {
    peaks_path: null;
    peaks_per_second: number;
    thumbnails: [];
    playback_path: null;
    playback_error: null;
  } {
    requireProject(args.project_id);
    return { peaks_path: null, peaks_per_second: 50, thumbnails: [], playback_path: null, playback_error: null };
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
    const next = { ...state.preferences, ...clone(incoming) };
    // The model switch is the user's, in the app (shell commands) — never here.
    if (
      next.model_mode !== state.preferences.model_mode ||
      next.hosted_model !== state.preferences.hosted_model ||
      next.hosted_consent_at !== state.preferences.hosted_consent_at
    ) {
      throw {
        code: "invalid_argument",
        message:
          "invalid argument: model_mode, hosted_model and hosted_consent_at can only be changed in the app (the LOCAL / HOSTED switch and Setup)",
      };
    }
    state.preferences = next;
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

  // After the core's retime_words: words trimmed to the audio. The mock has
  // no audio, so it only flips the flag and reports a plausible count.
  retime_words(args): {
    aligned: boolean;
    chunks: number;
    chunks_kept: number;
    words: number;
    trimmed: number;
    seconds_removed: number;
    relocated: number;
    note?: string;
  } {
    requireProject(args.project_id);
    if (!state.transcript) throw { code: "invalid_argument", message: "project has no transcript; transcribe first" };
    const aligned = !mockSwitch("noaligner");
    state.transcript.words_trimmed = true;
    state.transcript.words_aligned = aligned;
    const words = state.transcript.segments.reduce((n, s) => n + s.words.length, 0);
    emit("transcript-changed", { project_id: state.project!.id });
    return {
      aligned,
      chunks: aligned ? 10 : 0,
      chunks_kept: 0,
      words,
      trimmed: Math.round(words * 0.35),
      seconds_removed: 12.4,
      relocated: 0,
      ...(aligned ? {} : { note: "download the word alignment model in Setup for measured word times" }),
    };
  },

  async transcribe(args): Promise<Transcript> {
    const p = requireProject(args.project_id);
    if (mockSwitch("failtranscribe")) {
      progress("transcribe", p.id, 0.1, "Decoding audio…");
      await sleep(800);
      throw { code: "internal", message: "whisper exited with status 1 (mock failure)" };
    }
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
    void mockCleanup();
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

  cut_passage(args) {
    // The mock skips the sentence snap: first segment's start to last's end.
    const p = requireProject(args.project_id);
    const segs = state.transcript?.segments ?? [];
    const from = segs.find((s) => s.id === args.from_segment_id);
    const to = segs.find((s) => s.id === args.to_segment_id);
    const start = from ? from.start : Number(args.start);
    const end = to ? to.end : Number(args.end);
    const action = recordEdit(p, "cut_range", `Cut ${start.toFixed(2)}s–${end.toFixed(2)}s`);
    setRangeIncluded(p, start, end, false);
    refreshTimeline(p);
    return { action, timeline: clone(p.timeline), cut: { start, end, seconds: end - start, starts_with: "", ends_with: "" } };
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
    const by = new MockRun("batch", false).next(); // one chunk of 120 sentences
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
      method: by ? "llm" : "heuristic",
      beats: [
        beat(speech.slice(0, third), "Opening", "hook"),
        beat(speech.slice(third, third * 2), "Middle", "point"),
        beat(speech.slice(third * 2), "Closing", "recap"),
      ],
    };
  },

  review_flow(args): unknown {
    requireProject(args.project_id);
    const by = args.llm === true ? new MockRun("batch", false).next() : null;
    return { coherent: true, boundaries_checked: 0, issues: [], method: by ? "llm" : "deterministic" };
  },

  async story_edit(args): Promise<unknown> {
    const p = requireProject(args.project_id);
    // A conversation: outline, plan, flow review — one model throughout.
    const run = new MockRun("conversation", true);
    for (let request = 0; request < 3; request++) {
      run.next(); // outline, plan, flow review
      await sleep(250);
    }
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
    // A conversation: each model step goes through one route. A rejected key
    // stops before any edit (never the offline keyword cuts); losing
    // Anthropic, or a switch to Local, after a Claude step stops the run —
    // with its partial result when edits already landed.
    const run = new MockRun("conversation", true);
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
    try {
      const restore = lower.includes("restore") || lower.includes("bring back") || lower.includes("undo");

      if (restore) {
        run.next();
        say({ kind: "tool_call", tool: "undo", args: { project_id: p.id } });
        await sleep(350);
        const res = tools.undo!({ project_id: p.id }) as { action: EditAction | null };
        say({ kind: "tool_result", tool: "undo", result: { action: res.action?.description ?? null } });
        if (res.action) actions.push(res.action);
        summary = res.action ? `Reverted: ${res.action.description}.` : "Nothing to revert.";
      } else if (lower.includes("silence")) {
        run.next();
        say({ kind: "tool_call", tool: "detect_silences", args: { project_id: p.id } });
        await sleep(400);
        const det = tools.detect_silences!({ project_id: p.id }) as { segments: { start: number; end: number }[] };
        say({ kind: "tool_result", tool: "detect_silences", result: { count: det.segments.length } });
        const ids = state.transcript?.segments.filter((s) => s.is_silence).map((s) => s.id) ?? [];
        run.next();
        say({ kind: "tool_call", tool: "cut_by_transcript", args: { project_id: p.id, segment_ids: ids } });
        await sleep(400);
        const res = tools.cut_by_transcript!({ project_id: p.id, segment_ids: ids }) as { action: EditAction };
        say({ kind: "tool_result", tool: "cut_by_transcript", result: { description: res.action.description } });
        actions.push(res.action);
        summary = `Cut ${ids.length} silence gap(s) from the timeline.`;
      } else if (lower.includes("filler") || lower.includes("um") || lower.includes("uh")) {
        run.next();
        say({ kind: "tool_call", tool: "detect_fillers", args: { project_id: p.id } });
        await sleep(400);
        const det = tools.detect_fillers!({ project_id: p.id }) as { segment_ids: string[] };
        say({ kind: "tool_result", tool: "detect_fillers", result: { segment_ids: det.segment_ids } });
        run.next();
        say({ kind: "tool_call", tool: "cut_by_transcript", args: { project_id: p.id, segment_ids: det.segment_ids } });
        await sleep(400);
        const res = tools.cut_by_transcript!({ project_id: p.id, segment_ids: det.segment_ids }) as { action: EditAction };
        say({ kind: "tool_result", tool: "cut_by_transcript", result: { description: res.action.description } });
        actions.push(res.action);
        summary = `Removed ${det.segment_ids.length} filler segment(s).`;
      } else {
        // Generic path: try to find matching segments and cut them.
        const keyword = lower.replace(/(cut|remove|delete|the|all|parts?|about|where i say)/g, " ").trim().split(/\s+/)[0] ?? "";
        run.next();
        say({ kind: "tool_call", tool: "find_segments", args: { project_id: p.id, query: keyword } });
        await sleep(450);
        const found = tools.find_segments!({ project_id: p.id, query: keyword }) as { segments: TranscriptSegment[] };
        say({ kind: "tool_result", tool: "find_segments", result: { count: found.segments.length, query: keyword } });
        if (found.segments.length > 0 && keyword.length > 1) {
          const ids = found.segments.slice(0, 5).map((s) => s.id);
          run.next();
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

      await sleep(300);
      run.next(); // the final reply
    } catch (e) {
      // Edits that already landed are reported, never hidden behind an error.
      if (actions.length === 0) throw e;
      summary = `The model connection failed after ${actions.length} change(s) were applied — review the timeline or undo. (${(e as { message?: string }).message ?? String(e)})`;
    }
    say({ kind: "final", text: summary });
    return { actions, summary };
  },

  // ---- Metadata -------------------------------------------------------------

  generate_chapters(args) {
    requireProject(args.project_id);
    new MockRun("batch", false).next();
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
    new MockRun("batch", false).next();
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

  // ---- Clips ------------------------------------------------------------------
  // Same shape as the core; the finder is a crude stand-in for its heuristic
  // (sentence windows of the current cut, cold-open penalty, overlap pruning).
  // The model counts as reachable unless mockSwitch("nomodel") is on.

  async find_clips(args): Promise<unknown> {
    const p = requireProject(args.project_id);
    const t = state.transcript;
    if (!t) throw { code: "invalid_arg", message: "no transcript; transcribe first" };
    if (mockScans.has(p.id)) {
      throw { code: "invalid_arg", message: "a clip scan is already running for this project" };
    }
    mockScans.add(p.id);
    try {
      return await mockFindClips(p, t, args);
    } finally {
      mockScans.delete(p.id);
    }
  },

  get_clip_candidates(args): unknown {
    const p = requireProject(args.project_id);
    return { candidates: clone(p.clip_candidates ?? []) };
  },

  create_clip(args): unknown {
    const p = requireProject(args.project_id);
    const format = argFormat(args);
    const byRank = args.rank != null ? p.clip_candidates?.[Number(args.rank) - 1]?.id : undefined;
    const candidateId = args.candidate_id ?? byRank;
    if (args.rank != null && !byRank) {
      throw { code: "invalid_arg", message: `rank ${String(args.rank)} is out of range` };
    }
    let clip: SavedClip;
    if (candidateId) {
      const c = (p.clip_candidates ?? []).find((x) => x.id === candidateId);
      if (!c) throw { code: "not_found", message: `clip candidate ${String(candidateId)}` };
      // Validate before taking it off the list: a candidate cut from the main
      // edit since the scan stays listed.
      clip = clipFromCandidate(p, c, format ?? "vertical");
      p.clip_candidates = p.clip_candidates.filter((x) => x.id !== c.id);
    } else {
      const ranges = argRanges(args);
      if (!ranges) throw { code: "invalid_arg", message: "create_clip needs rank, candidate_id, segment_ids, ranges, or start + end" };
      const pieces = normalizeRanges(p, ranges);
      clip = {
        id: `clip-${Date.now()}-${p.saved_clips.length}`,
        title: `Clip ${p.saved_clips.length + 1}`,
        ranges: pieces,
        format: format ?? "landscape",
        crop_x: 0.5,
        start: pieces[0]!.start,
        end: pieces[pieces.length - 1]!.end,
        timeline: restrictTimeline(p.timeline, pieces),
        score: null,
        reason: "",
        hook: "",
        created_at: new Date().toISOString(),
      };
    }
    const title = String(args.title ?? "").trim();
    if (title) clip.title = title;
    p.saved_clips = [...(p.saved_clips ?? []), clip];
    if (clip.format === "vertical") mockAutoCenter(p.id, clip);
    return { clip: clipSummary(clip) };
  },

  /** Make several candidates, then "frame" each vertical one before
   *  returning, like the core (make_clips progress, "framing 2/6"). */
  async create_clips(args): Promise<unknown> {
    const p = requireProject(args.project_id);
    const ids = Array.isArray(args.candidate_ids) ? (args.candidate_ids as unknown[]).map(String) : [];
    if (ids.length === 0) {
      throw { code: "invalid_arg", message: "candidate_ids is empty: pass the ids of the candidates to make" };
    }
    const format = argFormat(args);
    const made: SavedClip[] = [];
    const errors: ClipBatchError[] = [];
    for (const id of ids) {
      const c = (p.clip_candidates ?? []).find((x) => x.id === id);
      if (!c) {
        errors.push({ candidate_id: id, message: "not in the candidate list (made already, or replaced by a newer scan)" });
        continue;
      }
      try {
        const clip = clipFromCandidate(p, c, format ?? "vertical");
        p.clip_candidates = p.clip_candidates.filter((x) => x.id !== c.id);
        p.saved_clips = [...(p.saved_clips ?? []), clip];
        made.push(clip);
      } catch (e) {
        errors.push({ candidate_id: id, message: (e as { message?: string })?.message ?? String(e) });
      }
    }
    emit("clips-changed", { project_id: p.id });
    const vertical = made.filter((c) => c.format === "vertical");
    // Compare-and-set, like the core's face pass: each crop moves only if it
    // still has the value it was made with — one someone moved meanwhile wins.
    const madeCrop = new Map(vertical.map((c) => [c.id, c.crop_x]));
    for (let i = 0; i < vertical.length; i++) {
      progress("make_clips", p.id, i / vertical.length, `framing ${i + 1}/${vertical.length}`);
      const clip = vertical[i]!;
      await sleep(450);
      if (clip.crop_x === madeCrop.get(clip.id)) clip.crop_x = MOCK_FACE_CROP_X;
    }
    if (made.length > 0) {
      progress("make_clips", p.id, 1, `made ${made.length} ${made.length === 1 ? "short" : "shorts"}`);
      emit("clips-changed", { project_id: p.id });
    }
    // Re-read: a clip deleted during framing is left out of the result.
    const live = new Set((p.saved_clips ?? []).map((c) => c.id));
    return { clips: made.filter((c) => live.has(c.id)).map(clipSummary), errors };
  },

  list_clips(args): unknown {
    const p = requireProject(args.project_id);
    return { clips: (p.saved_clips ?? []).map(clipSummary) };
  },

  update_clip(args): unknown {
    const p = requireProject(args.project_id);
    const c = (p.saved_clips ?? []).find((x) => x.id === args.clip_id);
    if (!c) throw { code: "not_found", message: `clip ${String(args.clip_id)}` };
    const format = argFormat(args);
    const ranges = argRanges(args);
    if (ranges) {
      c.ranges = normalizeRanges(p, ranges);
      c.start = c.ranges[0]!.start;
      c.end = c.ranges[c.ranges.length - 1]!.end;
      c.timeline = restrictTimeline(p.timeline, c.ranges);
    }
    const title = String(args.title ?? "").trim();
    if (title) c.title = title;
    const wasVertical = c.format === "vertical";
    if (format) c.format = format;
    if (args.crop_x != null) c.crop_x = Math.min(1, Math.max(0, Number(args.crop_x)));
    if (!wasVertical && args.crop_x == null && c.format === "vertical" && c.crop_x === 0.5) {
      mockAutoCenter(p.id, c);
    }
    return { clip: clipSummary(c) };
  },

  async center_clip_on_face(args) {
    const p = requireProject(args.project_id);
    const c = (p.saved_clips ?? []).find((x) => x.id === args.clip_id);
    if (!c) throw { code: "not_found", message: `clip ${String(args.clip_id)}` };
    await new Promise((r) => setTimeout(r, 600));
    c.crop_x = MOCK_FACE_CROP_X;
    return { clip: clipSummary(c), crop_x: c.crop_x, frames_sampled: 12, frames_with_face: 12 };
  },

  delete_clip(args): unknown {
    const p = requireProject(args.project_id);
    p.saved_clips = (p.saved_clips ?? []).filter((x) => x.id !== args.clip_id);
    return { deleted: true };
  },

  async export_clip(args) {
    const p = requireProject(args.project_id);
    await emitProgress("export", p.id, ["Resolving clip…", "Writing file…"], 700);
    return { path: String(args.out_path ?? "~/Downloads/clip.xml") };
  },

  /** One fake file per clip into `out_dir`, named like the core's batch
   *  (NN-<slug>.<ext>, -2 on a name already "written"); one export_clips
   *  progress task for the whole batch. Checks what it can before writing
   *  anything, in the core's order: empty ids, target, project. */
  async export_clips(args): Promise<unknown> {
    const ids = Array.isArray(args.clip_ids) ? (args.clip_ids as unknown[]).map(String) : [];
    const outDir = String(args.out_dir ?? "").trim().replace(/\/+$/, "");
    if (!outDir) throw { code: "invalid_arg", message: "out_dir is required" };
    if (ids.length === 0) {
      throw { code: "invalid_arg", message: "clip_ids is empty: pass the ids of the saved clips to export" };
    }
    const target = String(args.target || "mp4");
    const EXT: Record<string, string> = {
      premiere_xml: "xml", fcp_xml: "fcpxml", resolve_xml: "xml", edl: "edl", otio: "otio", mp4: "mp4", srt: "srt", txt: "txt",
    };
    const ext = EXT[target];
    if (!ext) throw { code: "invalid_arg", message: `unknown export target ${target}` };
    const p = requireProject(args.project_id);
    const written = mockFolders.get(outDir) ?? new Set<string>();
    mockFolders.set(outDir, written);
    const n = ids.length;
    const results: ClipExportResult[] = [];
    for (let i = 0; i < n; i++) {
      const id = ids[i]!;
      const clip = (p.saved_clips ?? []).find((c) => c.id === id);
      if (!clip) {
        results.push({ clip_id: id, path: null, error: `not found: clip ${id}` });
        continue;
      }
      progress("export_clips", p.id, i / n, `${i + 1}/${n} · ${clip.title}`);
      if (target === "mp4") {
        // render_mp4's own events, folded into the batch's.
        for (const pct of [0, 40, 80]) {
          progress("export_clips", p.id, (i + pct / 100) / n, `${i + 1}/${n} · ${clip.title} · rendering MP4… ${pct}%`);
          await sleep(300);
        }
      } else {
        await sleep(150); // text targets write at once: no render events
      }
      if (mockSwitch("failexport") && i % 2 === 1) {
        results.push({ clip_id: id, path: null, error: "ffmpeg exited with status 1 (mock failure)" });
        continue;
      }
      const name = batchFileName(i + 1, n, clip.title, ext);
      let file = name;
      for (let k = 2; written.has(file); k++) file = name.replace(/\.[^.]+$/, `-${k}$&`);
      written.add(file);
      results.push({ clip_id: id, path: `${outDir}/${file}`, error: null });
    }
    const ok = results.filter((r) => r.path !== null).length;
    progress("export_clips", p.id, 1, `exported ${ok} of ${n}`);
    return { results };
  },

  // ---- Export -----------------------------------------------------------------

  async export(args) {
    const p = requireProject(args.project_id);
    await emitProgress("export", p.id, ["Resolving timeline…", "Writing file…"], 700);
    return { path: String(args.out_path ?? "~/Downloads/export.xml") };
  },
};

/** Tool errors reject as Tauri's `call_tool` does: `{ error: { code, message } }`.
 *  Throw sites stay simple (`throw { code, message }`); this wraps them once. */
function toolError(e: unknown): { error: { code: string; message: string } } {
  const o = e as { code?: unknown; message?: unknown; error?: unknown } | null;
  if (o && typeof o === "object" && o.error && typeof o.error === "object") {
    return o as { error: { code: string; message: string } };
  }
  if (o && typeof o === "object" && typeof o.code === "string") {
    return { error: { code: o.code, message: String(o.message ?? "") } };
  }
  return { error: { code: "internal", message: String((o as { message?: unknown } | null)?.message ?? e) } };
}

export async function mockCallTool(name: string, args: Args): Promise<unknown> {
  await sleep(30 + Math.random() * 50);
  const tool = tools[name];
  if (!tool) throw toolError({ code: "unknown_tool", message: `Unknown tool: ${name}` });
  try {
    return await tool(args);
  } catch (e) {
    throw toolError(e);
  }
}
