# Zero-egress verification

**Claim:** core editing makes no network calls beyond this machine. The only
sanctioned egress is a user-triggered model download (Hugging Face, sha256
verified). The optional frontier path (`connect_external`) is opt-in and was
not configured during this run.

## Method

While the app ran a complete edit session driven over the MCP endpoint —
`create_project` (real media) → `transcribe` (whisper.cpp on-device, plus the
background Gemma cleanup and embedding-index passes against local Ollama) →
`generate_rough_cut` → `cut_range` → `find_segments` → `generate_chapters` →
`export` — the app process's sockets were sampled twice per second:

```sh
lsof -nP -i -a -p <app pid>   # sampled at 2 Hz for the whole session
```

Every observed connection's remote endpoint was collected and checked
against loopback.

## Result (2026-06-10, commit range up to this file)

```
listening sockets:    127.0.0.1:62936          # the MCP endpoint (localhost-only bind)
peer connections:     127.0.0.1:11434          # Ollama (local LLM + embeddings)
                      127.0.0.1:<ephemeral>×3  # the MCP test client itself
NON-LOCALHOST CONNECTIONS: NONE ✓
```

**PASS.** Nothing left the machine during ingest, transcription, LLM cleanup,
semantic indexing, the AI rough cut, search, or metadata generation.

Side observation: the externally-driven `export` and `delete_project` calls in
the same session were **denied by the confirmation guard** (no user approval
was given), demonstrating the destructive-op protection in the same run.

## Reproducing

1. Start the app; note its pid.
2. Run the sampler loop above in one shell.
3. Drive any session (UI or MCP).
4. Assert every `->host:port` in the log is `127.0.0.1`/`[::1]`.

Caveats: `lsof` sampling at 2 Hz could miss a sub-half-second connection;
for adversarial assurance use a packet capture (`tcpdump -i en0`) or Little
Snitch instead. The architecture-level guarantee remains the code itself —
the only `reqwest::get` to a non-configurable remote URL in the codebase is
the model downloader in `core/src/setup.rs`.
