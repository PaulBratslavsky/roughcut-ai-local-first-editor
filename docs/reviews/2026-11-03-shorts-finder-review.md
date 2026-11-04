# Shorts finder review (2026-11-03)

Why the Shorts scan proposes clips that feel disjointed or start in the wrong
place, and what to change. Based on the 59 candidates currently in two
projects, compared line by line with their transcripts, and on
`core/src/clip_finder.rs` and `core/src/story.rs`.

| Project | Scored by | Candidates | Start mid-sentence | Touch/overlap the next one | Scores |
|---|---|---|---|---|---|
| ai-literacy-is-the-new-prinitg-press (17 min) | Hosted: Claude Sonnet 5.5 | 19 | **17 (89%)** | 10 of 18 pairs | 42–63 |
| final-ai-doom-ane-gloom (25 min) | Local: gemma4 | 40 | 10 (25%) | 21 of 39 pairs | 68–95 |

## TL;DR

The model isn't choosing bad moments. It's handed bad cuts and can only
score them. Candidate shorts are cut on whisper's segment edges (often
mid-sentence), may start anywhere inside a 2–4 minute topic, and are shown
to the model without the sentences around them, so it can't see a missing
setup. The model can lower a score but can't move a start or end. The fix:
build shorts from real sentences, and let the model **pick** each short's
first and last sentence from the full topic instead of grading fixed
windows.

## How it works today

1. **Outline.** The model splits the transcript into "beats": topics of
   2–4 minutes (`story::outline`).
2. **Windows.** Every run of transcript segments that lasts 15–60 s and
   stays inside one beat becomes a candidate (`candidate_windows`).
3. **Rules.** A rule-based score: penalties for openers like
   "and/but/so", bonuses for questions and "here's…" openers, a sweet spot
   of 25–45 s (`heuristic_score`).
4. **Model.** The best windows go to the model six at a time. It gives
   each one a 0–100 score, a title and a reason (`llm_rescore`).
5. **List.** The best are kept, dropping any that share more than 30% with
   a better one (`diversify`).

## Findings

### 1. Shorts start where whisper chopped the audio, not where a sentence starts

Segments come straight from whisper and aren't sentences. In the first
project, **152 of 174 segments (87%) end mid-sentence**: whisper cut the
speech into ~6 s pieces at arbitrary points. Every window starts on a
segment edge, so 17 of 19 candidates open mid-sentence:

> …before: "Because before I hired them, I had to do all that work"
> **short opens:** "myself and I couldn't, right? That took me all of my day…"

> …before: "It's almost like there's this"
> **short opens:** "AI literacy that is happening. Like I was thinking about it…"

The code calls these windows "sentence-aligned", but they're only aligned
to segments. The rule penalty for a bad opener only checks the first
word, so it never notices that "myself" or "how to think about systems"
continues the previous line.

### 2. Shorts start after their setup or stop before the payoff

Even on whole sentences, a short can start at *any* sentence inside a
2–4 minute topic, and nothing rewards starting where an idea starts.
The model sees the excerpt alone, so a missing setup is invisible to it:

- **"One, we're all screwed."** The line before is *"There's literally two
  things that could happen."* The setup is cut off, and the model scored
  it 91.
- **"Are you fearing that you're not going to be making the same amount
  of money…"** This is the second question in a series; the first is cut
  off. Scored 94.
- **Ends on "hey, you've been in a very bad spot."** The next line, *"You've
  been in a very good spot."*, completes the pair. Scored 90.
- **Ends on "is it software engineering is dying?"** The next line, *"Or
  is the fear that I will not be able to get a job…"*, is the real
  question. Scored 91.

### 3. One argument becomes several back-to-back shorts

The overlap rule only drops candidates sharing more than 30% with a better
one, so slices that sit next to each other all get through. In project 2,
5:37–6:06, 6:03–6:37 and 6:37–7:15 are three cuts of one ~100 s argument.
The first has the setup, the second the question, the third the advice,
and none works alone. Across both projects, more than half of all
neighboring candidates touch or overlap. This is the "disjointed" feeling.

### 4. The model can see the problem but can't fix it

Claude Sonnet's reasons name the problem: *"it starts mid-sentence
('myself and I couldn't')"*, *"it cuts off mid-sentence at the end"*,
*"opens mid-thought and rambles"*. It scored them 56–63, and they're still
the best on the list, because every option in the pool has the same flaw.
Its best score, 63, means it doesn't think any of them are worth posting.

### 5. The local model's scores don't separate good from bad

gemma4 gave 40 candidates scores from 68 to 95, median 86, despite the
"be harsh" instruction. With most scores within a few points of each
other, the ranking is close to noise.

### 6. Smaller issues

- **Cut material.** After a rough cut, a short plays only what's kept in
  its range, but the model is shown the full text, including the cut
  lines. So a short can jump in ways the model never saw. Both projects
  here are uncut, so this didn't show up yet.
- **Truncated text.** Candidate text is cut at 900 characters before the
  model sees it, so for long windows the model judges "ends on a complete
  thought" without the ending. This happened to 1 of 59 here.
- **To check: outline caching in Hosted mode.** Project 1 was scanned in
  Hosted mode, but no outline is cached for it, while project 2's (Local)
  is. If Hosted outlines never cache, every scan re-runs the outline,
  which costs money and can change the topics between scans. Not
  confirmed.

## Recommendations (in order)

| # | Change | Fixes | Effort |
|---|---|---|---|
| R1 | **Build shorts from sentences.** Split the transcript into sentences using whisper's word timings (punctuation has its own timestamps: project 1's 174 segments become 177 sentences, median 4.5 s). Windows start and end only on sentence edges. | 1 | Small–medium |
| R2 | **Let the model pick the edges.** Per beat, give the model the beat's numbered sentences plus the last few of the previous beat. Ask: "pick up to N standalone shorts: first sentence, last sentence, title, hook, payoff, score". The code checks lengths, sentence edges and overlaps. Today's windows plus rules remain the fallback when no model is reachable. | 2, 3, 4 | Medium |
| R3 | **Show context when scoring** (if R2 isn't done first). Show one sentence before and after, marked [BEFORE] / [AFTER], and ask "does it need BEFORE?" and "does AFTER finish the thought?". Extend or trim by a sentence, then rescore. | 2, 4 | Small |
| R4 | **One short per idea.** Drop a candidate that touches or overlaps a better one in the same beat. If two neighbors both score well and fit the length limit together, offer them as one longer short. | 3 | Small |
| R5 | **Rank by comparison for local models.** Ask "which 2 of these 6 would you post?", or four 1–5 sub-scores (hook, standalone, payoff, ending), instead of a free 0–100. | 5 | Small |
| R6 | **Score what will play.** Build the text from kept sentences only, mark or avoid windows with cuts inside, and shorten long text from the middle, not the end. Check the Hosted outline cache. | 6 | Small |

R1 + R2 + R4 together are one change and fix most of what you're seeing.
To measure it, re-scan these two projects and compare the same numbers:
mid-sentence starts and ends, touching neighbors, and score spread.

## Pipeline, agent, or skills?

**A custom agent** would read the transcript, propose a short, check it
and adjust it in a loop. A strong model would do this well. But it's
slower and costs more per scan, and the codebase's own rule
(`story.rs`, from experience) is that local models can't reliably run
long multi-step flows. R2 gives most of an agent's judgment (the model
reads the whole topic and picks the edges) in one bounded step that
works on both Local and Hosted. For Hosted, a second pass could re-read
each picked short as it will play and accept it or fix its edges. The
open-ended agent route already exists today: Claude Code over MCP can
read the transcript and save clips with exact ranges.
**Recommendation: do R2, not a separate agent framework.**

**A Skills tab.** The app has no skills today. Five prompts written into
the code drive every AI step:

| Step | Where |
|---|---|
| Short scoring ("You are a short-form video editor…") | `core/src/clip_finder.rs:586` |
| Outline / beats | `core/src/story.rs:187` |
| Cut-point flow review | `core/src/story.rs:519` |
| Story-level edit plan | `core/src/story.rs:717` |
| In-app chat agent | `core/src/agent.rs:32` |

A tab that shows these and lets users edit them, with reset-to-default,
is a good fit for *taste*: what a good short is for your channel, how
harsh to be, house style. It won't fix findings 1–4, which are about
structure and live in code. When it's built, expose only the taste part
of each prompt (the rubric) and keep the JSON reply format and limits in
code, so an edit can't break parsing. **Recommendation:** build it after
R1/R2, with the shorts rubric as the first editable skill, and design it
with EGM like the other features.

## As built (`feat/shorts-sentences`)

R1, R2, R4 and R5 are done; R3 is covered by R2 (the model reads the whole
topic plus two sentences before it); R6 in part.

- **Sentences.** `clip_finder::sentences` re-cuts whisper's segments at
  sentence punctuation, timed from the word tokens (or by character position
  when the tokens don't spell the text), joining a sentence that runs into
  the next segment. A filler, a 1 s pause or a 20 s run-on also ends one.
  Every candidate starts and ends on a sentence.
- **The model picks the edges.** `pick_all` asks once per topic region
  (`pick_regions`: a beat; beats shorter than a max-length short joined;
  over 300 s split with overlap) for up to 3 shorts as first/last line
  numbers. Lines show the playable time each starts at, the edit's cuts are
  marked, and two sentences before the topic are context. The prompt spells
  out the failure modes found above (an answer without its question, the
  second item of a list, stopping on a line the next one finishes).
  `check_pick` validates every pick. A scan reads every topic (up to 40);
  a plain run reads the best few.
- **Rubric.** Four 1-5 ratings (hook, standalone, payoff, ending) instead
  of a free 0-100.
- **One short per idea.** Heuristic windows in one beat less than 2 s apart
  count as one moment.
- **Heuristic (no model).** New signals: "opens mid-sentence", "starts
  after its setup" (a list opener after "…two things…", or a line ending in
  ":"), "answers a question it leaves out", "stops before the thought ends"
  (the next line starts lowercase or with "or/because/which").
- **Outline.** The scan says when the model's outline didn't work and
  topics come from pauses.

## Measured

Both projects re-scanned with `core/examples/shorts_eval` (local gemma4,
the app untouched), and both lists judged the same way on whisper's raw
word timings: a clip starts mid-sentence when the word before it doesn't
end a sentence or its first word is lowercase. It ends mid-sentence when
its last word doesn't end a sentence or the next word is lowercase.

| | Starts mid-sentence | Ends mid-sentence | Touching/overlapping neighbours |
|---|---|---|---|
| ai-literacy… before (Claude Sonnet 5.5) | 17 of 19 (89%) | 8 (42%) | 10 of 18 |
| ai-literacy… after (gemma4) | **0 of 15** | **0** | **3 of 14** |
| final-ai-doom… before (gemma4) | 11 of 40 (28%) | 4 (10%) | 21 of 39 |
| final-ai-doom… after (gemma4) | **1 of 24** | **2** | **4 of 23** |

The earlier examples are fixed: the fear segment starts at its first
question ("What are you really fearing?"), and no list or pair is cut in
half.

Left to do:

- **One-sentence lead-ins.** On gemma4, a few picks still skip the
  sentence that introduces their idea (one starts at "I hired instructors…"
  after "…hiring people in my business, right?"), and one stops just before
  its punchline at the 60 s limit. Next step: an edge check that shows each
  pick with three lines either side and lets the model move each edge by a
  line, measured the same way, or try Hosted first.
- **Local scores are still generous** (median 80-88). The ranking within a
  list is useful; the absolute number isn't.
- **Speed.** A full scan took 12.5 min (17-min video) and 20 min (25-min
  video) on gemma4, including building the outline from scratch, which the
  app keeps after the first scan.
