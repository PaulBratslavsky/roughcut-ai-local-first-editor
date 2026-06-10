# Users & Usage

## Personas

### Primary — "Maya," the values-driven YouTuber
- **Background:** A solo (or near-solo) YouTuber who publishes talking-head content — commentary, tutorials, vlogs, educational explainers. Records long takes, then needs to cut them down. Technically comfortable: not a developer, but fluent with software, willing to install a desktop app, download a model, and tweak settings.
- **What drives her:** She wants to **own her data and her tools.** She's uncomfortable uploading raw footage to a cloud service, dislikes renting software by the month, and actively *wants* to support open source and the local-first movement. This is partly principle and partly practical — she's seen creators get burned by tools that change pricing, lock features behind tiers, or quietly use customer content. Owning the software means it can't be taken away or turned against her.
- **Current workaround:** Either editing rough cuts by hand in CapCut/Premiere/Resolve (slow, tedious — the 2-3 hours per video of cutting silence, filler, and bad takes), or using a cloud tool like Gling/Descript while holding her nose about the upload and the subscription. She's looking for a way out that doesn't cost her quality or speed.
- **Tech comfort:** Medium-high. Comfortable installing apps, managing files, and following a one-time model-download setup. Not afraid of a settings panel. Doesn't want to compile from source or run a terminal to *use* the app (though she's glad she could inspect it if she wanted).
- **Relationship to AI:** Pragmatic, not purist. She's happy to use a frontier model like Claude when it genuinely helps — she just doesn't want it to be the *only* way, or to be forced to ship her footage to the cloud to get basic editing done. **Local by default, frontier by choice.**

### Secondary — "Devin," the tinkerer / contributor (emerges from open source)
- **Background:** A more technical user — a developer-creator, a self-hoster, or someone active in the local-AI / open-source community. May contribute code, build extensions on the MCP tool surface, or run the app in unusual setups.
- **Why they matter:** Open source means this persona shows up whether or not you court them. They drive contributions, extensions, word-of-mouth credibility in the local-AI community, and they're the ones most likely to use the MCP/Claude-Desktop path heavily and push its limits.
- **Note:** Devin is not the beachhead. V1 must delight Maya first. But the architecture (MCP tool surface, open source) should leave the door open for Devin without bending the product toward developers at the expense of creators.

## Jobs-to-be-done

- When **Maya finishes recording a long take**, she wants to **get a clean rough cut fast — silences, filler, and bad takes removed — without uploading her footage anywhere**, so she can spend her time on the creative edit, not the tedious cleanup.
- When **Maya wants to refine the AI's first pass**, she wants to **adjust cuts precisely — via the transcript, by nudging clip boundaries, and by setting global padding** — so the result matches her judgment, not just the algorithm's.
- When **Maya hits a task the local model can't do well** (or just wants more power), she wants to **optionally bring her data and tools to a frontier model like Claude via MCP**, so she gets frontier capability *on her terms* — as a deliberate choice, never a forced default.
- When **Maya is done**, she wants to **export to her real editor (Premiere/FCP/Resolve)**, so the rough cut slots into her existing finishing workflow.
- When **Devin wants to extend or automate the editor**, he wants to **drive it from Claude Desktop or his own scripts through the MCP tool surface**, so he can build workflows the core app doesn't ship.

## Primary user journey

1. **Discovery:** Maya hears about the app in a local-AI / open-source / creator community — "the private, open-source Gling alternative that runs on your machine." The pitch that lands: *own your data, no subscription, no upload.*
2. **Setup (one-time):** She installs the app and it walks her through a first-run setup — downloads the local model(s) and transcription engine sized to her machine. A few GB, once. After this, it works fully offline.
3. **First use:** She drops in a raw recording. The app transcribes it on-device, then produces a first-pass rough cut — silences, filler, and bad takes removed — and shows a cut count, a transcript/script panel, a video preview, and a timeline with filmstrip + waveform.
4. **Aha moment:** She edits by *deleting text in the transcript* and watches the video cut to match, drags a clip boundary to fix a cut that landed a beat too early, and sets a global 0.15s padding so every cut breathes — all instantly, all local, nothing uploaded. The "I'm in control and my footage never left my laptop" realization is the hook.
5. **Optional power-up:** For a harder task, she chooses to send context to Claude via MCP — or drives the whole edit from Claude Desktop. She notices this is *her choice*, not a requirement.
6. **Export:** She exports an XML/EDL to her NLE and finishes the video there.
7. **Ongoing use:** Next video, she opens the app first thing — it's now the default first step in her pipeline.

## Core loop
**The repeated action:** record → drop footage into the app → get an AI first-pass cut → refine it (transcript edits, clip-boundary nudges, global padding, optional chat) → export to NLE. This repeats every video.

**What pulls users back:**
- It's *faster* than manual rough-cutting and *more controllable* than a cloud black box.
- It's *private and owned* — no upload, no subscription, can't be taken away. For a values-driven user, using it is itself an expression of what they believe.
- It *remembers their preferences* (padding defaults, cut aggressiveness, export target) so each video gets easier — a stickiness lever worth building.
- The *optional* frontier-model path means they never hit a hard ceiling: when local isn't enough, Claude-via-MCP is right there — without compromising the local-first default.

---

### Assumptions — ADOPTED AS DEFAULTS (locked when we moved to Stage 3; revisit anytime)
1. **Personas:** "Maya" (values-driven creator, primary) and "Devin" (tinkerer/contributor, secondary) as working placeholders.
2. **Devin stays secondary** — v1 delights the creator; the MCP/developer experience rides along, not co-primary.
3. **Preference memory** (padding/cut/export defaults) — in scope as a stickiness lever.
4. **Content type:** talking-head long-form; **Shorts generation deferred to v2** (confirmed in Stage 3).
