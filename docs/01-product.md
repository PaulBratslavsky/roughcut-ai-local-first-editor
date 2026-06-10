# Local-First AI Video Editor *(working name TBD)*

## Mission
The deeper goal is bigger than one app: **empower the local-first AI movement by taking services people currently rent from the cloud and moving them on-device — as software users own and run on their own computers.** This video editor is the first concrete instance of that idea: a category (AI rough-cut editing) that today means a subscription and an upload, reimagined as something private, owned, and offline. Success for the project is partly the app itself, and partly proving the pattern — that a capable local LLM plus on-device processing can replace a paid cloud service without compromise.

Funding is intentionally lightweight: **donations and community contributions are welcome but optional.** There is no paid tier, no upsell, no captured value — the point is user ownership, not revenue. This shapes every downstream decision: features exist to serve the user who owns the software, not a billing model.

## One-liner
A local-first AI video editor that turns raw talking-head footage into a clean rough cut entirely on your own machine — no upload, no subscription — by letting you chat with your footage instead of scrubbing a timeline.

## The problem
Solo creators — YouTubers, course creators, video podcasters — spend 2–3 hours per video on the most tedious part of editing: cutting silences, removing filler words ("um," "uh"), deleting bad takes, and assembling a rough cut before the "real" editing even begins.

The current AI tools that automate this (Gling, Descript, Opus Clip) all share the same structural cost: **they run in the cloud.** That means:
- **Your raw footage leaves your machine.** A real privacy/IP concern for anyone under NDA, working on unreleased content, or in a regulated field (legal, medical, corporate, journalism) — and a low-grade unease even for ordinary creators.
- **You depend on the internet.** Large 4K files have to upload before anything happens; unusable on a flight, a remote shoot, or a slow connection.
- **You pay forever, with caps.** Subscriptions plus monthly "hours" ceilings and overage fees. Gling's free tier is 1 watermarked hour/month; paid tiers run $10–$100/mo. Even the local one-time-purchase tools (TimeBolt ~$247, Recut ~$99) are closed source — you can't verify what they do with your footage, and you can't extend them.

The few tools that *are* local (TimeBolt, Recut) avoid the cloud but are waveform/rule-based — they can cut silence, but they don't have the semantic intelligence to pick the best take, understand content, or generate titles and chapters. **No one yet combines fully on-device processing with LLM-grade editing intelligence and no subscription.** That gap is the product.

## The value
**The "after" picture:** a creator drops a raw recording into the app, and within minutes — on their own laptop, offline, with nothing uploaded anywhere — gets back a clean rough cut with silences and filler removed and the best takes kept. The app transcribes the footage on-device and turns it into editable text, so the creator can refine the cut two ways: by **editing the transcript like a document** (delete a sentence → the matching video is cut) and by **chatting** with the footage ("cut the tangent about my weekend," "find the three best moments for a short," "tighten the intro"). Crucially, the AI's cuts aren't final — every clip stays **editable, with draggable start/end handles** so the creator can nudge any boundary frame-by-frame to fix where a cut lands. Then they export an XML/EDL straight into Premiere, Final Cut, or DaVinci Resolve for finishing.

This is the key interaction-model difference from Gling's "footage in → rough cut out" black box: the AI does the tedious first pass, but the human stays fully in control of the fine edit — through text, through chat, and through direct manipulation of clip boundaries.

Concretely, versus Gling, the user gets:
- **Privacy / ownership** — footage never leaves the device. Unlocks NDA, enterprise, legal, medical, and journalism work that cloud tools can't touch.
- **Works fully offline** — process 4K instantly on-device; no upload wait; usable anywhere.
- **One-time purchase, no caps** — no subscription, no monthly hour ceiling, no overage fees.
- **Open source** — the project is open source, so it's free to use, inspectable (you can verify nothing is uploaded — a powerful proof of the privacy claim), self-hostable, and community-extensible. This reinforces the trust/privacy story in a way no closed cloud product can match and turns users into potential contributors.
- **A conversational interface** — a local LLM (Gemma 4) acts as an orchestrator the user talks to, so editing feels like giving instructions, not operating software.
- **Transcript-based editing** — footage is transcribed on-device into editable text; deleting words/sentences cuts the matching video, so editing is as easy as editing a document.
- **Full control after the AI pass** — every cut produces clips with adjustable start/end handles; nothing the AI decides is locked, so the creator can fine-tune every boundary.
- **Global clip padding** — add or remove a buffer (e.g., 0.15s) to the start and end of *all* talking clips at once, so cuts breathe instead of feeling abrupt — tunable globally rather than clip-by-clip, with start/end adjustable independently or linked together.
- **Multilingual by default** — local transcription + a multilingual model attack the English-centric weakness of incumbents.
- **Open & extensible via MCP** — the app exposes its editing tools through the Model Context Protocol, so external AI clients like **Claude Desktop** can drive the editor directly. A user can edit *from inside Claude Desktop* ("cut the filler, find three short clips, tighten the intro") and the app executes those edits — and third parties can build on the same tool surface. This turns the editor from a closed app into an open, scriptable editing engine.

What it deliberately is **not**: a full NLE (no color grading, motion graphics, advanced audio mixing). Like Gling, it's the fast first step that feeds a professional editor — just private, offline, and owned.

## Product category
An **open-source, local-first desktop creativity tool**, in the "AI rough-cut / assistant editor" category — competing with Gling and Descript on intelligence, and with TimeBolt and Recut on the local/private axis, while occupying the white space where those two groups don't overlap (and being the only open-source option among them).

It's also, uniquely, an **open editing engine**: by exposing its tools over MCP, it's both a standalone app *and* a backend that other AI clients (Claude Desktop, future tools) can drive — something no incumbent offers.

## What success looks like
A year in: a creator on a flight with no Wi-Fi opens their laptop, drops in a 45-minute 4K recording, and by the time the drink cart arrives has a tight rough cut — filler gone, best takes chosen, chapters drafted — assembled entirely on-device, exported to their NLE, with not a single frame having touched a server. The product is known as *the* private, open-source way to do AI rough cuts, the default recommendation for creators who can't or won't upload their footage, and the tool people switch to when they get tired of paying a monthly fee with an hours cap.

At the mission level, success is that this becomes a proof point others build on — a working demonstration that a paid cloud AI service can be replaced by owned, local-first software, inspiring (and through MCP and open source, enabling) the same move in adjacent categories.

---

### UI reference (from Gling, provided as a target interaction model)
A screenshot of Gling's editor was provided as a reference for the interface this product should match or improve on. Key elements observed, to carry into Stage 3 (requirements) and the UI design:
- **Transcript/"Script" panel** as the primary editing surface (left), with Chapters, an "Enhance" action, and search.
- **Video preview** (top right) with a cut counter (e.g., "205 Cuts") showing how many cuts the AI made.
- **Timeline** (bottom): thumbnail filmstrip + audio waveform, playhead, zoom in/out.
- **Playback + edit controls:** play, speed (e.g., 2x), "Show cuts" / "Skip cuts" toggles, "Split", and a "Pace" control.
- **Global padding panel:** "Set padding to add or remove time from all talking clips" — independent **Start** and **End** sliders (shown at 0.15s each), a link toggle to lock them together, and an **Apply** button.
- **Export** button (top right) — the NLE hand-off.

This is the bar for the editing experience; the product should match these affordances while adding the local-first, conversational, and human-in-the-loop differentiators above.
1. **Primary audience — RESOLVED:** solo talking-head creators (YouTubers, course creators, video podcasters) who want to own their data and support open source — same core job as Gling's user, different values. (Detailed in Stage 2.)
2. **Scope — RESOLVED:** a rough-cut accelerator that hands off to a real NLE; not a full editor.
3. **Business model — RESOLVED: open source (GPL-3.0), donation-supported, mission-driven.** No paid tier, no upsell. Donations and contributions welcome but optional. The goal is user ownership and advancing the local-first AI movement, not revenue. **License: GPL-3.0** — copyleft, chosen to guarantee the project and anything built on it stays free and open (no one can close it or resell it as a proprietary/cloud service).
4. **Interaction model — CONFIRMED (you specified this):** the AI is *not* an invisible black box. It does the first-pass cut, but the user actively refines via transcript-based text editing and by dragging clip start/end handles, plus chat. Human-in-the-loop control is a headline differentiator vs Gling's one-shot model. *(Detailed capabilities — transcript editing, adjustable cut boundaries — are captured here at the product level and will be specified fully in Stage 3 requirements.)*
5. **Platform — RESOLVED (updated by the Stage 4 pivot):** **one codebase (Tauri + web) shipped to Mac, Windows, and Linux.** Development and first release lead on Mac, but all three platforms are targeted from day one — there is no separate "port" phase anymore.
