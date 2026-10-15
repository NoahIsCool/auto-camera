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
apostrophes; the longest phrase wins. A shot word right after an article or
preposition is a noun ("a face"). A prompt that is only shot words
("close-up", "upper body") is invalid: config load and `AddQuery` refuse it
with the parser's reason, and the window disables Add. `literal = true` (a
query's config key, a command flag, a checkbox in the window) sends the text
as typed.

### Presets

First rule that applies: explicit presets (a query's `presets`, the
window's ticked sizes) always win; else one standard preset per shot size
named, in typed order; else medium and close, as before. Prompts without
shot or role words therefore behave exactly as they did.

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

A box that touches the top or bottom edge of the source, or is too squat in
pixels for a standing body, falls back to the fill rule unless the shot
remembers a head height from an earlier untruncated box (each shot's framer
owns that memory, with a 5 s half-life). Every other subject keeps the fill
rule, unchanged. A shot the preset's zoom ceiling keeps from its size is
drawn at the ceiling and labelled, for example "Close (limited)", in the
multiview, and flagged in the headless summary.

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
- The parser is a heuristic. Known misreadings follow the table as written:
  a leading "ms" ("Ms Smith close-up" asks for Medium and Close) and "close
  friend". `literal = true` is the escape hatch.
- The design document numbers "Model lanes and capabilities" as ADR 0003;
  that ADR takes the next free number when it is written.

### For Noah to approve

1. The shot-word table above, and "upper body" giving both Medium and
   Medium close.
2. The role lexicon, each entry a hypothesis until benchmarked.
3. Explicit presets winning over shot words.
4. "wide" giving a Full shot until the kit-inclusive version exists.
5. Adding shot words in prompts to spec M2 and cut lines from the head to
   M7.
