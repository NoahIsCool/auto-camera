# ADR 0003: Shot words in prompts

Status: Proposed (2026-10-15). Noah approves; until then this is the
default the code follows.

## Context

Noah, on why he chose SAM 3 and what the prompts are for:

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
pastor and preacher become "a person speaking" (measured on SAM 3.1 only;
OWLv2 unscored, benchmark B0), guitarist "a person playing guitar"
(measured on both), drummer "a person playing drums", singer "a person
singing" (neither measured). "bass guitarist" is sent as typed.

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
tight 1 Close"), so typing one query per shot works.

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

An untruncated box is always measured on its own proportions, so Full is
the whole box. A box that touches the top or bottom edge of the source, or
is too squat in pixels for a standing body, falls back to the fill rule
unless the shot remembers a head height from an earlier untruncated box;
then its head is the larger of the remembered one and the box height over
7.5, because the visible part of a body bounds the whole from below. Each
shot's framer owns that memory and is its only writer: smoothed with a 5 s
half-life over untruncated boxes, raised (never lowered) toward the visible
bound by truncated ones. A lectern hiding the legs therefore keeps the zoom,
and a person who walks toward the camera until their feet leave the picture
is not sized by the smaller head they had further away. Every other subject
keeps the fill rule, unchanged.

A shot the preset's zoom ceiling keeps from its size is drawn at the
ceiling and marked: a LIMITED badge on its multiview tile (never in the
caption, which a narrow tile clips), "Close (limited)" on the monitors and
in the director's suggestion, and a flag in the headless summary. The
verdict has 5% hysteresis, so tracker jitter near the ceiling does not make
the mark flicker. When two sizes of one subject are both drawn at the
ceiling and look alike, the tighter one's hover text says which shot it
matches.

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
parser's notices, and its size checkboxes follow the parse until the
operator touches one. Each live query shows what its detector is asked for.
The headless summary reports the parse of every query.

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
2. The role lexicon, each entry a hypothesis until benchmarked.
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
