# Contributing

Thanks for helping build local-first software people own.

## Ground rules

- **Read the principles** in [README](README.md#principles-non-negotiable) and
  the architecture in [`docs/06-build-spec.md`](docs/06-build-spec.md) first.
  PRs that add network calls to the core editing path will not be accepted.
- New editing capability goes into the **tool registry** (`core/src/tools.rs`)
  so the UI, the local agent, and MCP clients all get it at once — never into
  the frontend or the Tauri layer directly.
- Adapters (video / inference / transcription / store) stay behind their
  traits in `core/src/adapters/`. Platform-specific code lives in an adapter
  implementation, not in the core logic.
- Mutating operations must record an `EditAction` and be undoable.

## Dev loop

```sh
cargo test -p roughcut-core          # unit + e2e through the tool registry
cd frontend && npm run dev           # UI against the in-browser mock API
cargo tauri dev                      # the full app (from app/src-tauri)
```

The fixture adapters (`FABLE_DEMO=1`) let you work on almost everything
without ffmpeg/whisper/Ollama installed.

## License

By contributing you agree your work is licensed under GPL-3.0.
