# ADR-0003: Frontend model types are generated from the Rust core

**Status:** accepted (2026-06)

## Context

The model shapes (Project, Timeline, Clip, Transcript, EditAction, …) were
hand-mirrored in `frontend/src/ipc/types.ts` and re-implemented in the browser
mock. That drifted once in practice (the mock's `cut_range` didn't split clips
like the core), and every model change required synchronized edits in three
places.

## Decision

Model types derive `ts_rs::TS` behind the `ts-bindings` feature and are
exported to `frontend/src/ipc/generated/` (committed). Regenerate with:

```sh
TS_RS_EXPORT_DIR="$(pwd)/frontend/src/ipc/generated" \
  cargo test -p roughcut-core --features ts-bindings export_bindings
```

`types.ts` re-exports the generated types and keeps only the event payloads
and tool-result envelopes by hand. The browser mock still implements edit
*semantics* in TS (it compiles against the generated shapes, which is what
catches drift); replacing it with a WASM build of the core's timeline model is
the known next step if mock fidelity becomes a problem again.

## Consequences

- A model change that the frontend doesn't handle is now a compile error, not
  a runtime surprise (proved immediately: generation surfaced three stale spots
  in the mock).
- Contributors must re-run the export after model changes; CI builds the
  frontend, so forgetting fails the build.
