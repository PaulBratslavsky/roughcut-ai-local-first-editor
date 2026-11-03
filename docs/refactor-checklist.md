# Core refactor checklist — deepening `core/src`

Working checklist for the architecture-deepening pass on `core/src`. Goal: turn
**shallow** modules (interface nearly as complex as the implementation) into
**deep** ones — more behaviour behind a smaller, testable interface. We measure
each item by **locality** (change/bugs/knowledge land in one place) and
**leverage** (callers get more from less).

> **2026-09 audit:** a full pass over every tier is in
> [`architecture-audit.md`](architecture-audit.md). Its corrections to items
> 1–7 are folded in below (marked *audit*), and item 8 came out of it.

## How to use this
- Work top-down; each item is independently shippable as its own PR.
- Apply the **deletion test** before/after: if deleting the module would make
  complexity reappear across N callers, it's earning its keep.
- The **interface is the test surface** — an item isn't done until its new seam
  has unit tests it didn't have before.
- Update `CONTEXT.md` whenever an item names a concept (see item 0).

## Constraints (do not re-litigate — see `docs/adr/`)
- **ADR-0001** — MCP stays hand-rolled JSON-RPC. Don't reach for `rmcp`.
- **ADR-0002** — edit-journal inverses are **snapshots**, not analytic. Item 1
  co-locates apply/describe logic; it does **not** change the inverse strategy.
- **ADR-0003** — frontend model types are generated; regen, never hand-edit
  `frontend/src/ipc/generated/`.
- **ADR-0004** — provisioning stays out of the tool registry.

---

## 0. Seed `CONTEXT.md` (do first, then grow per item)  ✅ done
A terse canonical glossary of domain nouns — distinct from the narrative in
`docs/07-architecture.md`. One line per noun: definition · key invariants · home file.
- [x] Create `CONTEXT.md` with the core nouns: `Project`, `Media`, `Transcript`,
      `Word`, `TranscriptSegment`, `Timeline`, `Clip`, `Padding`, `EditOp`,
      `EditAction`, `Checkpoint`, `CutSuggestion`, `Outline`, `Beat`,
      `FlowReview`, `Boundary`.
- [x] Document the three **implicit state machines** explicitly: transcript
      state, undo/redo position, suggestion lifecycle.
- [ ] Add each new module name (e.g. `Search`) here as later items create it.

---

## 1. `EditOp` pipeline — one edit type should be one local change  ★ start here
**Problem:** an op's behaviour is smeared across ≥3 matches in 2 files —
`kind()` (`model.rs`), plus `apply_op` **and** `frame_snap` (`engine/edits.rs`) —
then again as a handler + schema + registry row in `tools.rs` and a parity arm in
`mockApi.ts`. No single place tells you what `CutRange` *is*.
**Nuance (*audit* — corrects the earlier "all matches are exhaustive" claim):**
`kind()`, `describe()` and `apply_op` are exhaustive, but the two places that
enforce invariants are NOT: `normalize_op` (what this list called `frame_snap`)
ends in `other => other` (`edits.rs:603`) and the range-bounds check is an
`if let` (`edits.rs:59`). A new op silently skips frame-snapping and bounds
checks. Real touch-count is ~11 for `TrimClip` / ~14 for `CutRange` (add
`describe()`, the bounds check, `apply_edits`' blacklist + hand-written `oneOf`,
`story.rs:862`). So this is a **correctness** win, not only locality. (`ReorderClip` was a
journaled no-op — `normalize()` re-sorted it away — and has been removed.)
- [ ] Inventory every `EditOp` variant and its touch-points (enum, `kind`,
      `apply_op`, `frame_snap`, `tools.rs` handler/schema/registry, `mockApi` parity).
- [ ] Choose the co-location shape (per-op module, or a trait each op implements
      carrying apply + `kind`/describe + schema). Grill the interface first.
- [ ] Move apply logic out of the `apply_op` mega-match into per-op locality.
- [ ] Keep snapshot inverses (ADR-0002) exactly as-is.
- [ ] Make `apply` unit-testable without a live `Editor` — add tests for each op
      (currently `apply_op` has 0 tests).
- [ ] Verify: adding a hypothetical new op now touches ≤2 places.
- [ ] Update `CONTEXT.md` (`EditOp` invariants: contiguous, frame-snapped, etc.).
- **Done when:** a new op = one local block + a registry line; apply logic tested.

## 2. `tools.rs` — kill the 40×-copied handler boilerplate
**Problem:** each tool's JSON schema and its handler's arg parsing are two
independent descriptions of one interface, and they drift (*audit*):
`remove_silences` advertises `min_duration_s` and ignores it; `set_global_padding`
marks `end_s` required but defaults it. 58 handlers (not 45); `arg_uuid(a,"project_id")?` ×52, 26 raw `a["x"]`
reads. Visibility is no longer disjoint — see item 8.
- [ ] Catalogue the arg-parsing patterns (`arg_uuid`/`arg_f64`/`arg_str`/raw
      `a["x"]`) and the handler→engine-method shapes.
- [ ] Design a declarative tool definition co-locating name + schema +
      arg-extraction + impl (grill the interface; keep dispatch a table lookup).
- [ ] Collapse duplicated schemas (e.g. `cut_range`/`restore_range` drift risk).
- [ ] Migrate handlers in batches; keep the `Caller` policy (item 8) intact.
- [ ] Keep provisioning out of the registry (ADR-0004).
- [ ] Verify `tools.rs` shrinks materially and a new tool is one local block.
- **Done when:** the deletion test passes hard — boilerplate vanishes, real
  logic concentrates; registry-invariant test still green.

## 3. Extract a `Search` module (name the missing concept)
**Problem:** hybrid retrieval is smeared across `detect.rs` (`bm25_rank`),
`engine/semantic.rs` (`semantic_find`), and the **RRF fusion lives in the
`tools.rs` handler**. No named seam.
- [ ] Create a `search` module owning BM25 + embedding cosine + RRF behind one
      interface; move fusion out of the handler.
- [ ] Put ranking behind the seam so it's swappable (BM25 → alt) and fakeable.
- [ ] Add unit tests for fusion + ranking (currently only `bm25_rank` is tested).
- [ ] Add `Search` to `CONTEXT.md`.
- **Done when:** "how does search work" is answered by one module; fusion tested.

## 4. `detect.rs` — split the 5-algorithm grab-bag
**Problem:** silence / filler / takes / suggestion-composition / search share one
662-line file. (*audit* — corrects the earlier claim: `suggest_cuts` already
returns a value and has a unit test; only its embedding branch is untested. The
mutation lives in `engine/edits.rs:222-228`. `detect_silences` has no callers —
delete it. The real duplication is the candidates→refine pipeline copied three
times with three different min-duration rules: `detect.rs:216`, `edits.rs:163`,
`tools.rs:318`. Filler/take flags only ever accumulate.)
- [ ] Separate detectors into cohesive homes (silence, filler, takes).
- [x] ~~Make `suggest_cuts` return a value~~ — already does (*audit*); add a
      test for its embedding-repetition branch instead.
- [ ] Fold the search half into item 3 (don't leave it here).
- [ ] Resolve the three copies of candidates→refine (one min-duration rule),
      delete dead `detect_silences`, and clear stale flags on re-detect.
- **Done when:** each detector has a home; `suggest_cuts` has tests.

## 5. `story.rs` — move snapping out, split the subsystems
**Problem:** 5 loosely-coupled subsystems in 987 lines; `snap_cut_range` is pure
boundary math but lives here, forcing `tools.rs` to depend on `story` just to cut.
- [ ] Move `snap_cut_range` to a boundary/time module (kill the `tools.rs →
      story.rs` dependency for a plain cut). Add `Boundary` to `CONTEXT.md`.
- [ ] Split narrative outlining / flow-review / story-edit pipeline so each is
      reusable and testable on its own.
- [ ] Unify the duplicated LLM-vs-heuristic fallback fork (*audit*: 7 copies —
      `story.rs:88, 435, 593`, `agent.rs:178, 660, 727, 779`; `llm::ask_json`,
      extracted for this, has 0 callers).
- [ ] Add tests for the heuristic paths (no live model needed). The 2 existing
      `story.rs` tests exercise a *copy* of the guard, not `story.rs:712`.
- [ ] Fix the `llm_outline` clamp panic on overlapping beats (`story.rs:187-188`).
- **Done when:** snapping is independent; outline/flow testable without an LLM.

---

## 6. `Editor` facade — make the mutation funnel legible  ⚠ tackle LAST (biggest lift)
**Problem:** `Editor` is **one struct spread across 5 files**
(`engine/{mod,edits,projects,media,semantic,confirm}.rs`) — each just adds another
`impl Editor` block, so it reads like 5 modules but is really one object with its
behaviour scattered. Understanding "what is an `Editor`" needs all five. State
leaks through the shared `Mutex<HashMap<Uuid, ProjectState>>` every method locks,
and three **implicit state machines** (transcript state, undo/redo position,
suggestion lifecycle) are inferred from field-presence rather than made explicit.

> ⚠ **Why last / why risky.** This is the **mutation funnel the entire app flows
> through** — every tool, edit, undo/redo, and persistence. Blast radius is huge;
> a mistake breaks everything, and it's the hardest thing to unit-test today.
> Do items 1–5 first: they're contained, low-risk, and will sharpen what this
> refactor should actually be (e.g. item 1 already clarifies the edit/journal core).

- [ ] Land items 1–5 first; re-read this with fresh eyes before starting.
- [ ] Make the three implicit state machines **explicit** (named enums/types), so
      a change to one can't silently break another (see `CONTEXT.md` item 0).
- [ ] Pin down the locking contract: what the `state` `Mutex` guards, what may run
      while held, and the `rough_cut_peaks` set-before-call precondition.
- [ ] Give undo/redo a single home that says "here's how undo works" (today it's a
      call chain across `edits.rs`). Keep ADR-0002 snapshot inverses.
- [ ] Separate the persistence coupling so a failed `save_project` can't leave
      in-memory and on-disk state silently diverged without a recovery path.
- [ ] Add unit tests for the mutation funnel that don't need the live Tauri app
      (`edits.rs`/`media.rs`/`semantic.rs` have 0 unit tests today).
- **Done when:** "what is an `Editor`" and "how does undo work" are each answerable
  from one place; the state machines are types, not field-presence guesses.

## 7. Media asset cache — no lifecycle, grows without bound
**Problem:** `<data dir>/cache/<asset_cache_key>/` is written by
`adapters/video.rs` (peaks, `peaks_raw.bin`, thumbnail filmstrip, `play.mp4`) and
**never** read back for size, age, or ownership — there is no eviction, no budget,
no "this project is gone, drop its assets". Nothing in `core/` or `app/` prunes it.
Two compounding effects:
- `ensure_playable_copy` (video.rs:446) is a **lossless remux** (`-c:v copy`,
  `+faststart`), not a downscaled proxy — by design, but it means every source
  needing faststart or a >2ch audio downmix leaves a **full-size second copy** of
  the video on disk. Observed: 4.0 GB of `play.mp4` across 4 imports, one of them
  2.4 GB (1920x1080, 8 Mbps, 43 min).
- `asset_cache_key` (video.rs:540) intentionally invalidates when the file
  changes — but the superseded directory is **orphaned, not removed**, so
  re-encoding a source doubles its cache footprint.

Note this is a *lifecycle* gap, not a depth one: the write path is fine, the
missing concept is the cache as an owned, bounded thing. `Journal` and
`Checkpoint` already model exactly this (cap + eviction policy, `edits.rs:346`) —
follow that precedent rather than inventing a new one.
- [ ] Name the concept (`AssetCache`?) and give it one home; today `data_dir()
      .join("cache").join(asset_cache_key(..))` is recomputed at 4+ call sites.
- [ ] Put size/age accounting behind it: total bytes, per-key bytes, last-used.
- [ ] Add eviction with a user-visible budget (default cap, LRU by last-used),
      mirroring the checkpoint eviction policy rather than a new scheme.
- [ ] Drop a media key's directory when its last owning project is deleted, and
      drop orphaned keys (no project references them) on startup.
- [ ] Surface it in Setup: cache size + a "clear cache" action, alongside the
      existing model management.
- [ ] Decide whether `play.mp4` should stay lossless or become a real downscaled
      proxy for playback only (export already re-reads the source). Record it as
      an ADR either way — "the playable copy is intentionally lossless" is a
      decision worth pinning, not rediscovering.
- [ ] Tests: eviction picks the right victim; project deletion removes its keys;
      orphan sweep is idempotent and never touches a live key.
- **Done when:** cache size is bounded, observable, and clearable, and no disk is
  held by media no project references.

## 8. Caller policy — one home for "who may call what"  ✅ done ([#3](https://github.com/PaulBratslavsky/roughcut-ai-local-first-editor/issues/3))
**Problem:** visibility was filtered at *listing* (`mcp_defs`, `agent_defs`)
but not at *dispatch*; `dispatch` accepted every row and `dispatch_basic` only
excluded `meta`. MCP clients could call the hidden meta tools; the agent loop
could call `export` / `delete_project` / `set_preferences` unconfirmed;
`set_preferences` could point inference off-machine from MCP and was broken
from the UI (full-struct replace vs the Inspector's partial).
- [x] `tools::Caller` (`Ui | Mcp | Agent { attribute_to }`) decides visibility,
      arg validation and attribution; one `dispatch(…, caller)` + `defs(caller)`.
- [x] `set_preferences` is a merge patch; unknown keys rejected; non-UI callers
      can't set a non-loopback `inference_endpoint`.
- [x] Tests: every caller × tool agrees between listing and dispatch; meta tools
      refused over MCP HTTP; preferences patch/egress; loopback + merge units.
- [ ] Follow-up: move the confirmation guards (`export`, `delete_project`) and
      the egress guard onto `ToolSpec` (e.g. `guard: ConfirmExternal`), and add
      `delete_checkpoint` to the confirmed set.
- [ ] Follow-up: schema-check `Ui` calls too, once the frontend has a typed tool
      client (audit §3.1).
- [ ] Follow-up: emit a `preferences-changed` event so MCP-driven changes reach
      the UI.

## Deferred (revisit only if friction bites)
- **`Store` trait (20+ methods):** an intentional SQLite facade — already a **deep**
  seam (the deletion test exonerates it: deleting it scatters SQL/persistence across
  every `Editor` facet). Not a depth problem. The only minor friction is
  interface-segregation: a test fake must implement all 20 methods even to exercise
  two. If test ergonomics ever bite, split into role-traits (`ProjectStore` /
  `TranscriptStore` / `KvStore`) — otherwise leave it.

## Cross-cutting definition of done (every item)
- [ ] New/changed seam has unit tests it lacked before.
- [ ] `cargo test -p roughcut-core` green, with and without `--features ts-bindings`.
- [ ] `CONTEXT.md` updated if the item named a concept.
- [ ] No ADR violated (or a new ADR recorded if one was knowingly reopened).
- [ ] Shipped as its own focused PR.
