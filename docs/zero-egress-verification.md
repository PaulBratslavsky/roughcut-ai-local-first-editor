# Zero-egress verification

**Claim:** core editing makes no network calls beyond this machine. The only
sanctioned egress is a user-triggered model download (Hugging Face, sha256
verified). Hosted mode (the user's LOCAL / HOSTED switch, ADR-0007) is the
one opt-in exception for AI steps; this run was in Local mode.

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
the only requests to a fixed remote URL in the codebase are the model
downloader in `core/src/setup.rs` and, in Hosted mode only (or when the user
saves or tests a key in Setup), `core/src/adapters/anthropic.rs`
(`https://api.anthropic.com`).

The app log (`core/src/applog.rs`, `~/Library/Logs/RoughCut/`) is a local
file: the module does file I/O only, and nothing in the app reads it over the
network or sends it anywhere. Sharing it is the user copying the file.

## Hosted mode (2026-10-31, ADR-0007)

Hosted mode is off by default and can only be turned on in the app (a Tauri
command, not a tool), after a stored API key and the user's confirmation of
what is sent. To verify both modes:

1. Resolve Anthropic's addresses at test time: `dig +short api.anthropic.com`.
2. **Local mode with a key stored** — run the session above while sampling
   `lsof -nP -i -a -p <pid>` at 2 Hz. Expected: no non-loopback peers (a
   stored key changes nothing while the switch is on Local). The one
   exception is a key check the user starts in Setup (Save or Test), which
   calls `GET /v1/models/{model}` once.
3. **Hosted mode** — the same session. Expected: non-loopback peers only
   among the addresses from step 1, and only while an AI step runs (outline,
   scan, chat, story edit, flow review with `llm`, cleanup, chapters, title)
   or a key is checked. Search, face framing and export make no off-machine
   connections in either mode. Transcription itself stays on-device, but in
   Hosted mode it is followed by the background clean-up, which sends the
   transcript to Claude.

What Hosted sends is text: transcripts (with segment times and ids), the
project name, the user's chat and recent chat turns, the agent's tool
results (which can include the media file's path) and the app's prompts.
Never video, audio, frames or thumbnails.

## Link import (2026-11-05, ADR-0009)

Link import is off until the user clicks Install in Setup → "Import from a
link". That click downloads yt-dlp and Deno from their GitHub releases
(`github.com` and its release-asset host), each checked against its published
SHA-256. Update repeats it; Remove deletes both and turns link import off.

Once it is on, `import_url` (from the app, or an MCP client the user
connected) runs yt-dlp as a **child process**: its connections go to the
link's site and whatever hosts yt-dlp contacts for it (YouTube's video CDN,
for example), which is not a fixed list. `lsof -p <app pid>` can't see a
child's sockets, so verify by process:

1. With link import off, or on but with no import running, `pgrep -P <app
   pid> yt-dlp` prints nothing, and the session above passes unchanged.
2. During an import, sample the child as well:
   `lsof -nP -i -a -p "$(pgrep -P <app pid> yt-dlp)"`. Expected: peers only
   while the import runs, and the yt-dlp process gone when it ends (or when
   the app quits: `Editor::cancel_imports`).
3. Opening Setup runs nothing: status reads `<data dir>/bin/link-import.json`
   for the versions, so no yt-dlp or Deno process appears.
