# fimo.ai prompt — RoughCut landing page

Copy everything below the line into fimo.ai. Image placeholders are written
as `[IMAGE-N: …]` with a description of the exact screenshot to capture.

---

Build a single-page landing page for **RoughCut**, a free, open-source,
local-first AI video editor for macOS. One audience, one offer, one action.

**Audience:** talking-head video creators (YouTubers, course makers, devs who
record tutorials) who hate timeline scrubbing and are wary of cloud AI
editors that make them upload footage and pay monthly.

**The one action:** Download for macOS (free). Every section funnels to it.

**Voice:** confident, plain, slightly technical — like a well-written README.
No marketing fluff, no exclamation points. Short sentences.

**Visual style:** match the app itself — clean modern terminal aesthetic.
Monospace headings (JetBrains Mono / SF Mono), near-black background
(#0c0d0f), off-white text (#e8eaed), one accent only for the CTA, square
corners, hairline borders, generous spacing. Think Linear's confidence with a
TUI flavor. Dark theme only. No purple gradients, no stock photos, no
illustrations — real product screenshots do the talking.

---

## 1. Hero

**Headline:** Edit video by editing text.

**Subhead:** RoughCut turns raw talking-head footage into a clean rough cut —
on your machine, with local AI. Strike a sentence and the cut follows. No
upload. No subscription. No account.

**Primary CTA button:** `Download for macOS — free`
(links to https://github.com/PaulBratslavsky/roughcut-ai-local-first-editor/releases/latest)

**Under the button, small text:** Open source (GPL-3.0) · Apple Silicon & Intel · your footage never leaves this machine

**Secondary action (text link, not a button):** View on GitHub →

[IMAGE-1: full-app screenshot, dark theme — transcript panel on the left with
a sentence struck through (a cut), video preview top-right, timeline with
thumbnail filmstrip and waveform across the bottom. This is the money shot;
capture at 2x retina, ~1440px wide.]

## 2. Trust strip (one line under the hero)

`100% local · 0 bytes uploaded · GPL-3.0 open source · drives DaVinci Resolve · works with Claude`

## 3. Problem (3 short lines, make them feel seen)

- You talk for 40 minutes to get 18 good ones — then spend three hours finding them on a timeline.
- The AI tools that fix this want your raw footage on their servers, and $20+ every month, forever.
- You shouldn't have to choose between your evening and your footage.

## 4. How it works (3 numbered steps)

1. **Drop in your video.** Whisper transcribes it on your Mac — GPU-accelerated, a 26-minute video takes about two minutes.
2. **One click: rough cut.** Silences, filler words, and repeated takes are gone. Every cut is undoable.
3. **Refine like a document.** Delete sentences in the transcript, say "make this 20 minutes" in chat, then export to Premiere, Final Cut, or Resolve — or render the MP4.

[IMAGE-2: close crop of the transcript panel showing struck-through text mid-
paragraph and the right-click menu with "Cut selection" visible.]

## 5. Features (benefits first — keep each to one line of benefit + one of mechanism)

- **Edit text, not timelines** — click a word to seek; arrow keys step word-by-word with an audio cue; select text and cut it.
- **"Make this 20 minutes"** — a semantic planner ranks every segment by importance (local embeddings) and cuts the least important material to hit your target, in one undoable step.
- **Chat with your footage** — a local LLM (Ollama / llama.cpp) executes edits: "remove the filler words", "cut the part about pricing".
- **Find by meaning** — hybrid search (BM25 + embeddings): "the part where I talk about burnout" works even if you never said "burnout".
- **Real NLE hand-off** — Premiere XML, Final Cut XML, EDL, OTIO, SRT, or direct import into DaVinci Resolve (free version included) via the bundled Scripts-menu plugin.
- **Claude can drive it** — 30+ MCP tools; Claude Desktop or Claude Code can read your transcript, plan cuts, and land them in one batch. You approve anything destructive.
- **Seamless preview** — audio crossfades at every cut; toggle Cut vs Original instantly.
- **Honest engineering** — non-destructive everything, undo survives restarts, the source file is never modified.

[IMAGE-3: the Tools tab showing the TARGET LENGTH card with a minutes value
and the preview line "cut 38 segments → 19:58".]

[IMAGE-4: the Chat tab with action cards visible — "Find segments — searching
for …" and "✓ Cut 24 transcript segment(s)" — showing the AI narrating its
edits.]

## 6. The local-first section (this is the differentiator — give it room)

**Headline:** Your footage stays yours.

Copy: RoughCut makes zero network calls while editing. Transcription,
embeddings, and the chat model all run on your Mac. The only network use is
the model downloads you trigger yourself — checksum-verified, from the Setup
screen. Verify it with any network monitor; the test procedure is in the
repo.

Three stat blocks: `0` bytes of footage uploaded · `$0` forever (GPL-3.0) ·
`1` machine — yours.

[IMAGE-5: the Setup screen showing the capability rows — Media engine,
Speech-to-text with model downloads, Chat editing, Semantic search, DaVinci
Resolve — all with status dots.]

## 7. The Resolve / Claude section (two columns)

**Column A — "Finish in DaVinci Resolve":** Two Scripts-menu entries ship
with the app: *RoughCut AI Draft* turns a media-pool clip into a transcribed,
rough-cut timeline; *RoughCut Import Cut* pulls your refined RoughCut edit
into Resolve with media auto-linked. Works in the free version of Resolve.

[IMAGE-6: DaVinci Resolve's Workspace ▸ Scripts menu open, showing the two
RoughCut entries — or the imported timeline in Resolve's media pool.]

**Column B — "Or let Claude edit":** RoughCut exposes its entire tool set
over MCP. Tell Claude "tighten the intro and get this under 20 minutes" and
watch the cuts land — every change tagged, undoable, and gated behind your
approval for exports and deletes.

[IMAGE-7: Claude Desktop (or Claude Code) mid-conversation calling roughcut
tools — apply_edits / plan_duration_cut visible in the tool-call UI.]

## 8. FAQ (the top objections, answered plainly)

- **Is it really free?** Yes — GPL-3.0 open source. No tiers, no trial, no account. Model weights download free from the Setup screen.
- **What do I need?** A Mac (macOS 11+, Apple Silicon or Intel) and `brew install ffmpeg`. Whisper downloads in-app (~550 MB). Chat and semantic search optionally use Ollama.
- **Does my video get uploaded anywhere?** No. Editing makes zero network calls. The repo documents how to verify this yourself.
- **Will it replace my editor?** No — it replaces the worst three hours. You hand a clean rough cut to Premiere, Final Cut, or Resolve and do the craft there.
- **What footage does it work best on?** Talking-head content: tutorials, vlogs, courses, podcasts on camera. Anything where the words drive the edit.
- **The .dmg says it's from an unidentified developer.** The build is unsigned for now — right-click → Open the first time. Or build from source; it's three commands.

## 9. Final CTA

**Headline:** The rough cut is the robot's job.

**Button:** `Download for macOS — free`
**Under it:** GPL-3.0 · no account · your footage never leaves your machine
**Text link:** Star it on GitHub →

## Footer

Minimal: GitHub · Releases · Tool API docs · GPL-3.0. A one-liner: "RoughCut
is an experiment in replacing cloud AI subscriptions with software you own."

---

**Build notes for fimo:** single column on mobile, headline ≤8 words above
the fold, the download button is the ONLY element in the accent color, all
images lazy-loaded except IMAGE-1, target LCP under 2.5s.
