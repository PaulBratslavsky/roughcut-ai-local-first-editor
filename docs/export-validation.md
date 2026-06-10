# NLE export validation (POC criterion: "imports cleanly into a real NLE")

Validated on a real project (multi-cut timeline from real transcribed media,
manual + AI cuts + padding). Every text target is checked with the strongest
independent implementation available on this machine.

| Target | Validator | Result |
|---|---|---|
| `premiere_xml` / `resolve_xml` (FCP7 xmeml) | xmllint + **OpenTimelineIO `fcp_xml` parser** (reference implementation) | round-trips; clip count + durations match |
| `fcp_xml` (FCPXML 1.9) | xmllint + OTIO `fcpx_xml` parser + **Apple's official FCPXML 1.9 DTD, taken from inside the installed Final Cut Pro app** | **valid per Apple's own spec** |
| `edl` (CMX3600) | **OTIO `cmx_3600` parser** | parses; V1 duration matches the cut |
| `otio` | OpenTimelineIO core | parses; clips/durations match |
| `mp4` | ffprobe | rendered duration ≈ included duration |
| `srt` | block parse | cues remapped to output time, cut segments excluded |

The EDL validation **caught a real bug** before any human test would have: a
one-frame source/record duration mismatch (independent float→frame rounding
of timecode endpoints), which CMX-compatible importers reject. Fixed by doing
all EDL math in whole frames (`core/src/export/edl.rs`).

## On the GUI import step

DaVinci Resolve is **not installed** on the development machine (only its
manual folder remains), so a literal drag-into-Resolve test is not possible
here; the xmeml/EDL coverage above uses the same parser implementations the
post-production ecosystem builds on. Final Cut Pro is installed but exposes
no scripting API for verifiable import — its own bundled DTD is the formal
acceptance contract, and our FCPXML passes it.

When an NLE is available: test files generated under
`~/Desktop/roughcut-nle-test/` (source media + all three interchange files);
expected result is a ~25.6s timeline of 5 clips skipping fillers, the first
duplicate take, and the pauses.
