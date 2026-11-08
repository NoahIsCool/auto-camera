# ADR 0003: Shot words in prompts

Status: Proposed (2026-10-15). Noah approves; until then this is the
default the code follows.

## Context

Noah, on why they chose SAM 3 and what the prompts are for:

> "I liked that it took arbitrary input prompts. This is why I chose SAM 3.
> Also, part of that was to zoom in to the upper body, so we can say
> "speaker upper body". There are ways that we can hack that behavior that
> to other single token models, but that doesn't generalize to objects, or
> give the option for different types of shots."

A prompt like "speaker upper body" asks two questions at once: *who* to
frame ("speaker") and *what shot* to make ("upper body"). No model answers
both (`/mnt/project-files/research/model-picker-design.md` §1, §2, all
measured on Noah's rehearsal footage):

- SAM 3.1 returned no detections for "speaker upper body", even at a 0.2
  threshold; "a person speaking" found the speaker in 10 of 20 labelled
  frames with no false subjects.
- OWLv2 boxes the objects a prompt names, never shots, and to it the bare
  word "speaker" most likely means a loudspeaker (inferred).

Framing had a matching bug. Since commit `079f635` the OWLv2 people-first
matcher returns whole-person boxes, but `ac_framing::compose` sized every
shot as `box.h / fill` with fills tuned on upper-body boxes. For a person
half the frame high, "Close" came out 0.54 of the frame high: a full-length
shot (design §4.5).

The spec asks for both halves: prompt feedback so wording issues show up
(M2) and framing presets for people (MS / MCU / CU) with headroom (M7, with
the C.4 headroom bands). Shot words in prompts are not yet in the spec; the
design proposes adding them to M2 and cut lines from the head to M7 (§4.9).
This ADR records that proposal for Noah.

## Decision

**The operator's prompt is not the detector's prompt.** A pure parser in
`ac-core` (`parse_prompt`) splits each prompt into a subject, the phrase the
detector receives, the shot sizes its shot words name and a basis (person
or object). It runs once, when a query is created (config load or the
`AddQuery` command); the query keeps the operator's text for display and
the parse for everything else (`ac_engine::queries`).

### Vocabulary, as data

The words are a `Lexicon`, built in and extendable from a studio's
`[lexicon]` table (an entry with the key of a built-in one replaces it).
Every entry is a proposal until Noah confirms it:

| Shot words | Sizes |
|---|---|
| close, close-up, CU, head and shoulders, headshot, tight, face | Close |
| medium close, medium close-up, MCU, chest up, bust | Medium close |
| medium, mid shot, MS, waist up | Medium |
| upper body, half body | Medium **and** Medium close (default until Noah picks, design §8 item 6) |
| full, full body, full length, head to toe, long shot, FS | Full |
| wide, after a subject only ("drummer wide") | Full, for now |

"wide" should frame the subject together with its attached objects (spec
V4, "drum kit with drummer's head"); that needs attachment boxes and is
later work, and the operator is told so whenever the word is used. The
camera's own wide shot stays each source's Wide.

Role words are rewritten only when they are the whole subject: speaker,
pastor and preacher become "a person speaking", guitarist "a person playing
guitar", drummer "a person playing drums", singer "a person singing". "bass
guitarist" is sent as typed. "a person speaking" found the speaker on SAM
3.1 (10 of 20 frames, no false subjects). On OWLv2 (benchmark B0:
people-first, the described-person route at the 0.15 person threshold) it
found the speaker in 20 of 20 frames, with 300 false boxes on the 40
labelled frames: it picks everyone, because OWLv2 sees people and objects,
not actions. "a person playing guitar" is measured on both models, "a person
playing drums" on OWLv2 (20 of 20, no false boxes), "a person singing" on
neither.

Each role entry carries a `needs` tag, `action` or `object` (design §4.2):
speaker, pastor, preacher and singer need an action, guitarist and drummer
an object (their instrument, which people-first matching asks for). A prompt
rewritten to an action role carries a notice: OWLv2 gives every person
(photos on the screen included) that query's shots, so the operator takes
the speaker's shot by hand. The design asks for the warning only when the
chosen model lacks the capability (§4.2), read from what the detector
declares (Phase 2, "Capability contract"). That contract is not built, so
the engine reads the one capability it needs from the detector's kind
(`DetectorSettings::sees_actions`): the sidecar, SAM 3.1, sees actions;
OWLv2 and the synthetic detector do not. The parser adds the notice
whatever the detector, and the engine drops it, for the form, the window
and the headless summary alike, when the detector sees actions. A
`[lexicon]` role that replaces a built-in one replaces its tag too; a
studio's entry without `needs` draws no notice.

### Grammar

Shot words count at the end of a prompt, at its start ("close-up of the
guitar", "tight on the drummer"), and right after a possessive ("speaker's
upper body"), ignoring case, hyphens, spaces inside a phrase and typographic
apostrophes, singular or with an "s" plural ("close-ups", "headshots"; not
"es", so "pool cues" holds no "CU"); the longest phrase wins. Separators,
brackets and quotes left where shot words were cut off are trimmed, so
`guitar "close-up"` sends "guitar". A shot word right after an article or
preposition is a noun ("a face"). A prompt that is only shot words
("close-up", "upper body") is invalid: config load and `AddQuery` refuse it
with the parser's reason, which suggests "the face" or literal mode to look
for the thing itself, and the window disables Add. `literal = true` (a
query's config key, a command flag, a checkbox in the window) sends the text
as typed.

Some shot words are everyday words too, marked in the lexicon (`also =
"modifier"` or `"noun"`). The parser still reads them as shot sizes, but
adds a notice pointing at literal mode where the everyday reading is likely:
a modifier at the start right before the subject ("full band", "close
friend of the speaker", "tight end", "Ms Smith close-up"), a noun at the end
right after a word that is not a person ("a rock face", "marble bust"), and
a modifier at the end right after an "-ing" word ("a man standing close").

### Presets

First rule that applies: explicit presets (a query's `presets`, the
window's ticked sizes) always win; else one standard preset per shot size
named, in typed order; else medium and close, as before. Prompts without
shot or role words therefore keep their detector phrase and presets (their
framing can still change; see Framing).

Because role words collapse different prompts into one detector phrase
("speaker upper body" and "speaker close-up" both ask for "a person
speaking"), two queries often find the same person, and the subject store
makes them one subject answering both (an alias). That subject gets the
shots of every query it answers, the primary query's first, de-duplicated by
preset id (design §4.4, rule 7), each named after its own query ("Speaker
tight 1 Close"), so typing one query per shot works. An alias's shots
appear once the store has seen it in `alias_confirm_passes` (2) detection
passes that observed the subject, and stop after `alias_stale_passes` (8)
such passes without it; passes, not seconds, so the rule is the same on a
1 Hz OWLv2 and on a shared sidecar that visits each camera every few
seconds. Identity is unchanged: the alias continues the subject from its
first sighting, so one stray box (a guitar handed to whoever stands in
front of it) makes no shot and no twin. The primary query, chosen at
admission, is outside this rule and always frames. When the operator
removes it, the store promotes the first alias that frames; only when none
does is an unconfirmed alias promoted, and that one frames at once. A
subject on program or preview keeps the shots it holds, with their names,
until it leaves the air.

### Framing

A subject's shots are cut at body lines only when its query's basis is a
person (the head noun is a person or role word) **and** the detector marked
the box a whole person (`Extent::Person`: OWL people-first output, scripts
standing in for people). Then, with the head height taken from a 7.5-head
canon of an untruncated box (no head detector yet):

```text
crown = box top        cut = crown + heads(size) * head        heads: Close 1.6, MCU 2.3, MS 3, Full 7.5
h = (cut - crown) / (1 - headroom)                             top = crown - headroom * h
```

A box is measured on its own proportions, so that its Full is the whole
box, when it is untruncated and, while its shot reads the person as cut by
the bottom on a stored count (below), ends clear of the release band. Any
other box (one that touches the top or bottom edge of the source, is too
squat in pixels for a standing body, or is read as cut by the bottom) uses
a head height the shot remembers from an earlier measured box: the larger
of the remembered one and the box height over 7.5, because the visible part
of a body bounds the whole from below. Each shot's framer owns that memory
and is its only writer: smoothed with a 5 s half-life over measured boxes,
raised (never lowered) toward the visible bound by the others. A lectern
hiding the legs therefore keeps the zoom, and a person who walks toward the
camera until their feet leave the picture is not sized by the smaller head
they had further away.

With nothing remembered, a box cut by the bottom edge with its crown in the
picture (a speaker framed from the knees up) is cut from the crown on a
bounded assumed head instead of the fill rule. It is read as showing
`V = clamp(1.875 * pixel aspect, 1.6, 3.0)` head heights, so the head is the
box height over `V`, and no frame reaches below the box, so the crown keeps
the preset's headroom. For every box tall enough to be a standing body `V`
is 3, crown to waist, the loosest head the assumption allows: when more of
the body shows, shots come out looser than their size; only a box showing
less (a chest-up in profile) comes out tighter. The width term only keeps
`V` continuous across the squat threshold. The framer smooths `V` with the
same 5 s half-life in a slot of its own that never becomes a remembered
head; it seeds it only from boxes clear of the side edges, then frames every
box cut by the bottom on it (also at a side edge or with the crown at the
frame top) and reads the bottom edge with hysteresis: after a box at the
edge, a box ending within 3% of the bottom is still read as cut off, until
a box measured on the canon (untruncated) ends above that band. A box that
measures nothing (too squat, as behind a lectern, or cut by the top) keeps
that reading wherever it ends, so a speaker never seen whole who steps out
from behind a lectern with one box just above the edge is not measured as
a whole body. Full, boxes clear of the bottom (cut by the top or too
squat), and, until the shot holds a `V`, boxes cut by the bottom that
touch a side edge or the frame top keep the fill rule. Every other subject
keeps the fill rule, unchanged.

A shot the preset's zoom ceiling keeps from its size is drawn at the
ceiling and marked: a LIMITED badge on its multiview tile (never in the
caption, which a narrow tile clips), "Close (limited)" on the monitors and
in the director's suggestion, and a flag in the headless summary. The
verdict has 5% hysteresis, so tracker jitter near the ceiling does not make
the mark flicker. When two sizes of one subject are both drawn at the
ceiling and look alike, the tighter one's hover text says which shot it
matches, and the headless summary gives that shot's id (`same_picture_as`).

A shot keeps the basis of the query that asked for it. While its subject is
on program or preview, removing that query (the subject becomes an orphan)
or promoting an alias with another basis never changes how the shot on air
is measured; once off air, it follows its current query.

An object prompt keeps the fill rule even when people-first matching
answers it with the person holding the object: "guitar close-up" frames the
guitarist's whole box with the fill rule, as before this change, rather
than their head. Framing the guitar itself needs its box attached to the
subject (design §4.2), which is later work.

### Feedback

The operator window shows a live parse line under the prompt ("Detector
sees: a person speaking · Framing: Medium, Medium close (person)") with the
parser's notices (an action role's among them, unless the detector sees
actions), and its size checkboxes follow the parse until the operator
touches one. Each live query shows what its detector is asked for, with the
notices in its hover text. The headless summary reports the parse of every
query, notices included.

## Consequences

- Prompts stay free text, as Noah wants, and shot words work on any
  subject, person or object; adding a model later (SAM 3.1, design Phase 2)
  changes what reads the detector phrase, not the parser.
- On whole people, shots now mean their size: Close is head and shoulders.
  Distant people need more zoom than the 4x ceiling allows (a CU of a person
  half the frame high needs about 9x), so many such shots are labelled
  limited instead of passed off as close-ups. Which sizes each camera can
  really deliver is benchmark B6.
- Head geometry is a body-proportion estimate. Raised hands or a guitar
  headstock above the head move the crown; a seated drummer or a lectern
  makes boxes look truncated. A head source (an OWLv2 head query, SAM masks
  or pose; design §4.5) replaces it later without changing this decision.
- The no-head fallback for a person cut off by the bottom of the picture is
  now bounded. On a speaker framed from the knees up (a box 0.35 of the
  frame high) Close is drawn at the 4x ceiling and marked limited, and
  Medium close and Medium are 0.285 and 0.38 high with the crown 6% and 8%
  down, where the fill rule framed the whole visible body at every size
  (0.38, 0.45 and 0.58 high, crown 8%, 22% and 40% down). Band members whose
  boxes run off the bottom get tighter shots, more of them limited, which
  the band clip has yet to confirm; this supersedes design §4.5 item 3 for
  such boxes.
- The parser is a heuristic. Known misreadings follow the table as written,
  each now with a notice that points at `literal = true`, the escape hatch:
  a leading "ms" ("Ms Smith close-up" asks for Medium and Close of "Smith"),
  "close friend of the speaker" (Close of "friend of the speaker"), "full
  band" and "full choir" (Full of "band", "choir"), "tight end" (Close of
  "end"), "marble bust" (Medium close of "marble"), "a rock face" (Close of
  "a rock") and "a man standing close" (Close of "a man standing").
- A config that loaded before this change can now be refused at startup:
  a `[[queries]]` prompt that is only a shot word ("face", "bust",
  "headshot") is invalid even when the query lists explicit presets. Write
  "the face" or set `literal = true` to keep looking for the thing itself.
- Prompts without shot or role words keep their detector phrase and presets,
  but those that name a person ("person", "a person playing guitar") are now
  cut at body lines when the detector returns whole people (OWLv2
  people-first), so their shots change size and some are marked limited.
  On a guitarist half the frame high, `config/file-source.example.toml`'s
  Full goes from the wide to a 1.8x crop, and its 6x Tight, which needs
  about 9x, is now drawn at the ceiling and marked.
- The design document numbers "Model lanes and capabilities" as ADR 0003;
  that ADR takes the next free number when it is written.

### For Noah to approve

1. The shot-word table above, and "upper body" giving both Medium and
   Medium close. On a person too far away for the 4x ceiling (half the
   frame high or less), Medium needs about 4.6x and Medium close about
   6.1x, so both are drawn at the ceiling and look the same apart from
   their headroom: two tiles and two shortcut slots for one picture, both
   marked limited, the tighter one saying which shot it matches. Choosing
   one of the two for "upper body" avoids that.
2. The role lexicon, each entry a hypothesis until benchmarked, and each
   tagged with what it needs. OWLv2 cannot carry the Speaker (B0: "a person
   speaking" picks every person), so an action role's notice tells the
   operator to take the speaker's shot by hand, on every detector but the
   sidecar (SAM 3.1), which sees actions.
3. Explicit presets winning over shot words.
4. "wide" giving a Full shot until the kit-inclusive version exists.
5. Adding shot words in prompts to spec M2 and cut lines from the head to
   M7.
6. Object prompts answered by a person keep the fill rule on the person's
   box until attachment boxes exist. Design §4.2 frames "guitar close-up"
   on the guitar's own box; people-first OWLv2 returns the guitarist
   instead, and the fill rule then makes that "Close" the player's full
   length. Such shots are not marked: marking them (as limited, or as
   approximate) is an option.
7. Refusing a prompt that is only shot words even when the query lists
   explicit presets (the compatibility break above), rather than reading it
   as the thing to look for.
8. A subject found by several queries getting the shots of all of them,
   each named after its query.
9. The assumed head for a person cut off by the bottom of the picture, and
   its inferred constants: a shoulder width of 1.875 head heights (which
   ties the 1.6 squat threshold to crown to waist), a floor of 1.6 and a cap
   of 3.0 visible head heights (the Close and Medium cut lines), and the
   3% release band at the bottom edge. Also open: whether such shots get
   an approximate mark (none today; a Close drawn at the ceiling is already
   marked limited) and its wording; what Full of such a person should be
   (the fill rule today, a 1.14x crop with the crown 60% down, or the
   visible box with Full headroom); and the residual risks. A chest-up box
   seen in profile gets its Close cut near the neck; a whole body whose
   feet touch the bottom edge gets its Close cut near the hip; a
   mid-distance person cut by the bottom (a box about 0.25 high) has Close
   and Medium close both drawn at the ceiling and marked as the same
   picture; a shot whose very first box ends just above the edge (bottom
   0.97 to 0.99) still measures a whole body, and so can an alias shot
   made again after its label went stale (shot labels drop an alias's
   shots after missed passes, and the new shot starts with an empty head
   memory and no stored `V`); after a box at the edge, a whole body whose
   feet stay within the 3% band is still read as crown to waist and its
   Full falls to the fill rule; and a head raised while the person was cut
   stays raised until a box measured on the canon ends above 0.97.
10. The shot-label rule for aliases and its inferred constants: an alias
    frames shots once 2 detection passes that observed the subject have
    seen it (`alias_confirm_passes`), and stops after 8 such passes in a
    row without it (`alias_stale_passes`). Also open: that pinned and
    on-air subjects follow the evidence like any other rather than being
    exempt (the engine only keeps the shots an on-air subject already
    holds); that confirmation counts sightings within the stale window
    rather than in consecutive passes, so two sightings 7 missed passes
    apart confirm; that the primary query chosen at admission is outside
    the rule and always frames, and whether a stale primary should yield
    to a confirmed alias; and that a protected subject keeps every shot it
    holds, not only the shots on program and preview, until it leaves the
    air.
