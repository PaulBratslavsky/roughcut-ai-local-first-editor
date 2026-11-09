# Clip edit session: edit a saved clip in the Editor

Status: implemented on `fix/word-timings` (2026-11-08). Asked for by the
owner ("open clip in editor to do additional edits"); built directly after a
short plan in chat, not through an EGM review.

## Problem

A saved clip (`SavedClip`) has its own timeline: the main cut restricted to
the clip's pieces, snapshotted when it was made. The only ways to change one
are re-picking its pieces (`update_clip` ranges, which re-snapshots the
*main* cut) and the 9:16 crop. There is no way to cut a word, a pause or a
false start inside a short, or to start it one sentence earlier, without
changing the main edit and making the clip again.

## What the user does

1. Clips screen, a clip's **edit** button. The app opens the Editor on that
   clip, with a bar across the top: *Editing clip "Title" · 0:31 · Save ·
   Cancel*. Cut material is hidden (transcript and timeline show only the
   clip), the timeline fits the clip, the playhead is at its start, and a
   9:16 clip shows its crop over the preview.
2. Everything in the Editor works as usual, on the clip: cutting words from
   the transcript, restoring them (also just outside the clip, to extend it),
   Rough cut with its review, chat edits, padding, undo/redo, checkpoints.
3. **Save** writes the edit back to the clip; **Cancel** drops it. Either way
   the main edit comes back exactly as it was: its timeline, its undo
   history, its checkpoints, its pending proposed cuts and its suggestions.

## Design: swap the clip's timeline in

Every edit tool, the agent, proposals and the undo journal work on
`Project.timeline`. Rather than teach each of them about a second timeline,
a session swaps the clip's timeline into `Project.timeline` and sets
everything that belongs to the main edit aside.

- `Project.clip_session: Option<ClipEditSession>` (persisted, sent to the UI):
  `{ clip_id, title, started_at }`.
- The main edit, set aside, lives in the key-value store under
  `clip-session-main:{project_id}` (not on `Project`, so `get_project` stays
  small; `open_project` returns it): `MainEdit { timeline, cut_proposal, checkpoints, suggestions,
  undo, redo }`.
- `begin_clip_edit(project, clip)`: writes `MainEdit`, then puts
  `clip.timeline` in `Project.timeline`, clears the proposal, checkpoints,
  suggestions and journal, sets `clip_session`, saves. Beginning the same
  clip again is a no-op; another clip while one is open is an error.
- `end_clip_edit(project, save)`: with `save`, the edited timeline becomes the
  clip's timeline and its pieces are recomputed (`pieces_after_edit`, below);
  it refuses while proposed cuts are pending ("apply or discard them first")
  and when nothing would play. Then (save or cancel) it restores `MainEdit`,
  clears `clip_session`, saves, and deletes the key.
- Order of writes: the key first, then the project, on the way in; the
  project, then the journal, then the key, on the way out. A crash between
  two writes on the way in leaves an unused key; on the way out it can leave
  the clip's journal over the main edit for one session (the timeline itself
  is always one or the other, never mixed). Accepted: the window is a few
  milliseconds and the store has no cross-table transaction.
- The session survives restarts: `clip_session` is on the project, the
  journal is the normal `journal:{id}` key.

### The clip's pieces after an edit

`pieces_after_edit(old_pieces, timeline)`: each stretch that plays is given to
the old piece it overlaps, or else the nearest one; each piece becomes the
span from its first stretch's start to its last stretch's end (pieces with no
stretch are dropped, overlapping ones merged). Cuts inside a piece stay in the
clip's timeline, as when a clip is made. So restoring the sentence before the
clip extends its first piece; cutting the last sentence shortens it.

### What can't run during a session

Guarded in the engine (so every caller, including the Tauri `send_to_resolve`
command, gets the same refusal: "you're editing the clip "Title" — save or
cancel it first"):

- `export` of the project (it would export the clip as the whole video;
  export the clip from the Clips screen instead), and so Send to Resolve;
- `find_clips` (it scans the main cut);
- `create_clip`, `create_clips`, and `update_clip` with new pieces (they
  snapshot the main cut);
- `delete_clip` of the clip being edited;
- `import_media` into the project, `append_media`, `attach_screen_media`;
- `story_edit` (a whole-video plan).

Everything else runs. Transcript changes (fix word timings, text edits,
filler flags) are the project's, shared with the main edit, as before.

### The chat

The agent's project context, in a session, says it is editing the clip and
lists only the transcript segments around it (the clip's span plus 30 s
either side, for "add the sentence before"), instead of the whole video,
where the clip's lines could fall past the context budget. Save and Cancel
are not agent tools: they are the user's.

### Tools

| Tool | Callers | Returns |
| ---- | ------- | ------- |
| `begin_clip_edit { project_id, clip_id }` | UI, MCP | `{ session, timeline }` |
| `end_clip_edit { project_id, save }` | UI, MCP | `{ clip: SavedClipSummary \| null, timeline }` (the clip when saved) |

Both emit `timeline-changed`, `clips-changed` and `cut-proposal-changed`.

## Out of scope

- Several clips open at once; a clip editing its own clip.
- Showing the 9:16 crop in the timeline or the transcript.
- Carrying the crop into NLE exports (unchanged: MP4 only).
