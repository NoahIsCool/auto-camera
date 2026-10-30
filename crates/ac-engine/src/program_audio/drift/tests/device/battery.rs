//! The device input's scenario battery: every case of a CoreAudio board's
//! timestamps that a review has found the [`Stamper`] to take for what it
//! was not, as families of scenarios swept over IO buffer sizes, sample
//! rates, jitter and phases, each played through the real chain (the
//! hand-off, the [`Stamper`], [`ProgramAudio`]'s drift correction and the
//! [`AudioPacer`]) and judged against a twin. A design of the stamper is
//! judged on all of them at once, not review by review.
//!
//! # Why a battery
//!
//! Host times alone cannot tell a lost buffer from a late stamp, nor a
//! lasting rewind from a glitch on its way home, until the blocks after
//! them do; the stamper decides by rules over a window, and every rule
//! that settled one case has moved another. Five review rounds each found
//! new mis-classifications: a loss inside a rewind's window folded into
//! it, a second buffer's glitch taken for a first buffer off, a loss right
//! after a first buffer off cut short by that buffer's error, jitter
//! walking the map forward, and then the steadiness rule that stopped that
//! walk turning lasting rewinds under jitter above ±0.125 ms into cuts, and
//! host times that go late in small steps walking it again. Each was fixed
//! with a test of its own case. The battery keeps all of them, and the
//! neighbours a fix is likeliest to break, in one place, so that a change
//! is measured on the lot.
//!
//! # How a case is judged
//!
//! A case plays the parent module's device model (its [`Device`], with
//! seeded white jitter, [`Bursts`] and events) and a *twin*: for a fault,
//! the same device with the same jitter (the same seed, so every buffer
//! jitters alike) and without the fault; for jitter alone, the same device
//! without jitter. What is measured:
//!
//! * the device input's notices (gaps, rewinds, glitches), and their sizes;
//! * drift correction's steps and breaks against the twin's: drift
//!   correction cuts audio only for a step it made up or at a break (the
//!   frames it edited, silence put in less audio cut plus what resampling
//!   changed, are printed);
//! * the pacer's silence and drops;
//! * *net edit*: drift correction's inserted frames, plus the pacer's
//!   silence less its drops, against the twin's. For a real loss this
//!   must equal the known frames lost, independently of the stamped PTS;
//! * *apart*: the largest difference of the pacer's lead from the twin's,
//!   in ms, over the run or from some time on: what was put in or cut;
//! * *walk*: how far the lead's difference from the twin's moved, the
//!   mean over the last [`WALK_WINDOW`] less the mean over the first (from
//!   [`WALK_FROM`]): the board's audio sliding against the video, judged
//!   by its size and printed with its sign (positive for audio stretched,
//!   which plays later);
//! * *lasting offset*: where the board's audio ends up against its twin's,
//!   from the offsets of the blocks pushed over the last second (the
//!   encoder's position at a block less its PTS), so that a loss which
//!   moves the callbacks after it to other ticks is compared rightly;
//! * the *PTS walk*, also judged for jitter-only families: how far the
//!   blocks' PTS error moved over the run, less the twin's. It is the
//!   stamper's map itself moving, which the lead shows only once drift
//!   correction takes it for a slow or fast clock, past its dead band.
//!
//! # Families
//!
//! Must (the test of each asserts):
//!
//! * **M1** no fault, white jitter up to ±0.5 ms peak, 128 to 4096 frames,
//!   44.1, 48 and 96 kHz: no notice, no drift step, no pacer edit, apart
//!   within 1.5 ms, no walk (1.5 ms).
//! * **M2** white jitter of ±0.75 to ±2 ms at 128 to 1024 frames: no
//!   rewind, no gap, no walk (2 ms).
//! * **M3** bursts of ±1.25 to ±2 ms jitter, two-sided and late-only,
//!   0.3 s long every 5 to 60 s: no rewind, no walk (2 ms).
//! * **M4** ramp then snap every 10 s (four buffers late by 0.5, 1, 1.5 and
//!   2 ms, then back; and twelve by 0.25 ms more each): no rewind, no walk.
//! * **M5** a lasting rewind of 2.5, 15 or 30 ms under white jitter up to
//!   ±0.5 ms at 256 to 1024 frames, many phases: one rewind of about its
//!   size, no cut, apart within 1.5 ms; with a cycle lost 3, 10 or 30
//!   buffers after it: the rewind and one gap, the loss made up once, apart
//!   from the loss alone within 1.5 ms.
//! * **M6** glitches ahead and back of 3 to 30 ms for 1 to 3 buffers (the
//!   steps home among them), coming back in one step or in steps, jitter
//!   0.03 ms: at most one glitch, no drift step, no pacer edit, no lasting
//!   offset (0.5 ms); for 4 buffers or more: no lasting offset, at most two
//!   drift edits.
//! * **M7** real losses, of one IO cycle at 128 to 1024 frames and of up to
//!   99.9 ms (the near-slack cases): one gap of the loss, made up once, the
//!   lasting offset within 0.5 ms; the net edit equals the known loss to
//!   within a 0.5 ms resampler and sample-rounding budget.
//! * **M8** the first buffer off by ±3 to 30 ms at 128 to 4096 frames, its
//!   first frame captured before the open and after it: nothing reported,
//!   no step, apart within 1 ms; with a cycle lost before callback 2 to 5:
//!   the loss's gap alone, made up once (no step for a cycle shorter than
//!   drift correction's), apart from the loss alone within 1 ms. The full
//!   grid has 1416 identifiable cases checked this way. Its 24 remaining
//!   cases also describe a later-starting device with ordinary startup
//!   noise and a short glitch: a separate M8 policy family requires one
//!   glitch, no inferred gap or drift edit, a bounded initial offset and
//!   no subsequent walk. Exact input-pair tests prove that ambiguity (see
//!   [`start`]); those 24 are not counted as exact-loss recoveries.
//! * **M9** a one-buffer glitch of the second buffer, ±3 to 30 ms: no
//!   rewind and no gap; one glitch beyond the tolerance, none within it;
//!   no lasting lead offset beyond 0.5 ms or the resampler's half kernel
//!   and one sample's rounding, whichever is larger.
//! * **M10** a sample clock ±200 ppm off the host's, white jitter ±0.5 ms:
//!   no notice, the correction settles where it cancels the drift, no walk.
//!
//! Should (asserted too, their tests named `should_*`):
//!
//! * **S1** a lasting rewind of 15 or 30 ms under white jitter of ±0.75 to
//!   ±1.5 ms: one rewind, no cut.
//! * **S2** white jitter of ±1 to ±2 ms at 2048 and 4096 frames: no walk
//!   (2 ms).
//! * **S3** a glitch back of 4 buffers or more that comes back, under
//!   ±0.5 ms jitter: no lasting offset (0.5 ms).
//!
//! # Grids and scorecards
//!
//! Each family is one test over a default grid that runs in a few seconds
//! in a debug build: short runs, a few sizes, phases and seeds, among them
//! the cases found so far. With [`SWEEP_VAR`] set to `full` it sweeps the
//! full grid instead: every size, rate and phase, and minutes-long runs
//! where walking matters. With [`SWEEP_VAR`] set at all, each family
//! prints its scorecard line: its name, cases passed of all, and its worst
//! case (most criteria failed, then furthest past or closest to a limit)
//! with its numbers. A failing family lists its worst failing cases. For
//! the full battery's scorecards (a few minutes in a release build):
//!
//! ```text
//! AC_STAMPER_SWEEP=full cargo test --release -p ac-engine --lib battery -- --nocapture
//! ```
//!
//! # Invariants
//!
//! * A case and its twin differ only by the fault (or by the jitter): their
//!   buffers jitter by their first frames and the seed, and come at the
//!   same ticks unless the fault moves them.
//! * Everything is deterministic: a grid gives the same verdicts on every
//!   run, and no case needs a clock, a device or the network.

use std::fmt::Display;

use super::*;

mod jitter;
mod start;
mod steps;

/// The environment variable that widens the battery to its full grid (set
/// to `full`) and makes each family print its scorecard (set at all).
const SWEEP_VAR: &str = "AC_STAMPER_SWEEP";

/// Seconds of the lead taken at each end of a run for its walk.
const WALK_WINDOW: f64 = 10.0;

/// Seconds at a run's start left out of its walk, as its first blocks
/// arrive.
const WALK_FROM: f64 = 1.0;

/// Failing cases listed when a family fails, worst first.
const LISTED: usize = 12;

/// How wide the battery sweeps, and whether it prints its scorecards; read
/// from [`SWEEP_VAR`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sweep {
    /// The full grid, not the default one.
    full: bool,
    /// Each family prints its scorecard line.
    print: bool,
}

impl Sweep {
    /// The sweep [`SWEEP_VAR`] asks for.
    fn from_env() -> Self {
        match std::env::var(SWEEP_VAR) {
            Ok(value) => Self {
                full: value == "full",
                print: true,
            },
            Err(std::env::VarError::NotUnicode(_)) => Self {
                full: false,
                print: true,
            },
            Err(std::env::VarError::NotPresent) => Self {
                full: false,
                print: false,
            },
        }
    }

    /// `quick` in the default grid, `full` in the full one.
    fn of<T>(self, quick: T, full: T) -> T {
        if self.full { full } else { quick }
    }

    /// The values of `quick` in the default grid, of `full` in the full one.
    fn pick<T: Clone>(self, quick: &[T], full: &[T]) -> Vec<T> {
        self.of(quick, full).to_vec()
    }
}

/// What one case's criteria found: those it failed, and how close it came
/// to its limits.
#[derive(Debug, Clone, Default, PartialEq)]
struct Judge {
    /// The criteria failed, each with what was measured.
    failed: Vec<String>,
    /// The largest of its measures over their limits: above 1 past one.
    worst: f64,
}

impl Judge {
    /// Requires `ok`, a criterion named `what`.
    fn require(&mut self, what: impl Display, ok: bool) {
        if !ok {
            self.failed.push(what.to_string());
        }
    }

    /// Requires `value` to be at most `limit` (above zero), a measure named
    /// `what`.
    fn at_most(&mut self, what: &str, value: f64, limit: f64) {
        self.worst = self.worst.max(value / limit);
        // A measure that is not a number fails too.
        if value.is_nan() || value > limit {
            self.failed.push(format!("{what} {value:.3} > {limit}"));
        }
    }
}

/// One case's verdict: what it was, what its criteria found, and its
/// numbers.
#[derive(Debug, Clone, PartialEq)]
struct Verdict {
    /// What the case was.
    case: String,
    /// What its criteria found.
    judge: Judge,
    /// Its numbers against its twin.
    numbers: String,
}

impl Verdict {
    /// How badly it did, for the scorecard's worst case: more criteria
    /// failed first, then the measure furthest past (or closest to) its
    /// limit.
    fn badness(&self) -> (usize, f64) {
        (self.judge.failed.len(), self.judge.worst)
    }

    /// The case, the criteria it failed if any, and its numbers.
    fn describe(&self) -> String {
        if self.judge.failed.is_empty() {
            format!("{}: {}", self.case, self.numbers)
        } else {
            let failed = self.judge.failed.join("; ");
            format!("{}: FAILED {failed} | {}", self.case, self.numbers)
        }
    }
}

/// A family's scorecard: the verdicts of its cases, printed as one line
/// when the sweep asks for it, and asserted.
struct Card {
    /// The family, as its scorecard names it.
    family: &'static str,
    /// The sweep it runs.
    sweep: Sweep,
    /// Its cases' verdicts so far.
    verdicts: Vec<Verdict>,
}

impl Card {
    /// The scorecard of `family`, for the sweep [`SWEEP_VAR`] asks for.
    fn new(family: &'static str) -> Self {
        Self {
            family,
            sweep: Sweep::from_env(),
            verdicts: Vec::new(),
        }
    }

    /// Adds the verdict of `case`, which `judge` judged, with its `numbers`.
    fn add(&mut self, case: String, judge: Judge, numbers: String) {
        self.verdicts.push(Verdict {
            case,
            judge,
            numbers,
        });
    }

    /// Its scorecard line: the family, cases passed of all, and the worst.
    fn line(&self) -> String {
        let passed = self.verdicts.iter().filter(|v| v.judge.failed.is_empty());
        let worst = self
            .verdicts
            .iter()
            .max_by(|a, b| a.badness().partial_cmp(&b.badness()).expect("no NaN"));
        format!(
            "battery {}: {}/{} passed; worst: {}",
            self.family,
            passed.count(),
            self.verdicts.len(),
            worst.map_or_else(|| "no cases".into(), Verdict::describe)
        )
    }

    /// Prints its line when the sweep asks for it, and fails, listing the
    /// worst failing cases, when any case failed.
    fn finish(self) {
        let line = self.line();
        if self.sweep.print {
            println!("{line}");
        }
        let mut failing: Vec<_> = self
            .verdicts
            .iter()
            .filter(|v| !v.judge.failed.is_empty())
            .collect();
        failing.sort_by(|a, b| b.badness().partial_cmp(&a.badness()).expect("no NaN"));
        let listed: Vec<_> = failing.iter().take(LISTED).map(|v| v.describe()).collect();
        assert!(
            failing.is_empty() && !self.verdicts.is_empty(),
            "{line}\n{} failed, worst first:\n{}",
            failing.len(),
            listed.join("\n")
        );
    }
}

/// A device at `rate` with IO buffers of `frames`, its host times jittering
/// white by up to `peak` seconds either way, in the sequence of `seed`.
fn jittered(rate: u32, frames: u64, peak: f64, seed: u64) -> Device {
    Device {
        jitter: peak,
        seed,
        ..Device::new(rate, frames)
    }
}

/// Seconds of the device's clock at which IO buffer `n` of `frames` at
/// `rate` starts, with no audio lost before it: an event at that time comes
/// just before callback `n`.
fn buffer_at(n: u64, frames: u64, rate: u32) -> f64 {
    (n * frames) as f64 / f64::from(rate)
}

/// The IO buffer of `frames` at `rate` under way at `t` seconds.
fn buffer_index(t: f64, frames: u64, rate: u32) -> u64 {
    (t * f64::from(rate) / frames as f64) as u64
}

/// The tolerance a jump after a block of a device with IO buffers of
/// `frames` at `rate` is reported by, in seconds: the hand-off cuts longer
/// buffers into blocks of [`MAX_FRAMES`].
fn block_tolerance(frames: u64, rate: u32) -> f64 {
    tolerance(frames.min(MAX_FRAMES as u64), rate)
}

/// The mean of `run`'s lead less `twin`'s over the ticks from `from` to
/// `to` seconds of the output, in ms.
fn mean_apart(run: &Run, twin: &Run, from: f64, to: f64) -> f64 {
    let ticks = run.lead.len().min(twin.lead.len());
    let at = |s: f64| ((s * FPS as f64).max(0.0) as usize).min(ticks);
    let (first, last) = (at(from), at(to));
    let sum: f64 = run.lead[first..last]
        .iter()
        .zip(&twin.lead[first..last])
        .map(|(a, b)| a - b)
        .sum();
    if first == last {
        f64::NAN
    } else {
        sum / (last - first) as f64
    }
}

/// `run`'s walk against `twin`: how far its lead's difference from the
/// twin's moved from the first [`WALK_WINDOW`] (from [`WALK_FROM`]) to the
/// last, in ms, positive for a lead that grew; `None` for a run too short
/// to have both. Judges take its size, scorecards print its sign.
fn walk(run: &Run, twin: &Run) -> Option<f64> {
    let end = run.lead.len().min(twin.lead.len()) as f64 / FPS as f64;
    (end >= WALK_FROM + 2.0 * WALK_WINDOW).then(|| {
        let first = mean_apart(run, twin, WALK_FROM, WALK_FROM + WALK_WINDOW);
        mean_apart(run, twin, end - WALK_WINDOW, end) - first
    })
}

/// The size of `run`'s walk against `twin` (see [`walk`]) for a judge,
/// not a number for a run too short to have one, so that it fails.
fn walk_size(run: &Run, twin: &Run) -> f64 {
    walk(run, twin).map_or(f64::NAN, f64::abs)
}

/// The mean PTS error of `run`'s blocks from `from` to `to` seconds of the
/// device's clock, in ms.
fn pts_mean(run: &Run, from: f64, to: f64) -> f64 {
    let (sum, n) = run
        .pts_error
        .iter()
        .filter(|(t, _)| (from..to).contains(t))
        .fold((0.0, 0u32), |(sum, n), (_, e)| (sum + e, n + 1));
    if n == 0 {
        f64::NAN
    } else {
        sum * 1000.0 / f64::from(n)
    }
}

/// `run`'s PTS walk against `twin`: how far its blocks' PTS error moved
/// over the run, less how far the twin's did, in ms; `None` for a run too
/// short.
fn pts_walk(run: &Run, twin: &Run) -> Option<f64> {
    let moved = |r: &Run| {
        let end = r.pts_error.last().map_or(0.0, |&(t, _)| t);
        (end >= WALK_FROM + 2.0 * WALK_WINDOW).then(|| {
            let first = pts_mean(r, WALK_FROM, WALK_FROM + WALK_WINDOW);
            pts_mean(r, end - WALK_WINDOW, end) - first
        })
    };
    Some(moved(run)? - moved(twin)?)
}

/// Requires both the audible lead and the Stamper's PTS map to stay
/// within `limit_ms`; a map error must fail before drift correction's
/// dead band can turn it into an audible offset.
fn no_walk(judge: &mut Judge, run: &Run, twin: &Run, limit_ms: f64) {
    judge.at_most("walk (ms)", walk_size(run, twin), limit_ms);
    judge.at_most(
        "PTS walk (ms)",
        pts_walk(run, twin).map_or(f64::NAN, f64::abs),
        limit_ms,
    );
}

/// Where `run`'s board audio ends up against `twin`'s, in ms: the mean
/// offset of its pushed blocks over the twin's last second of PTS, less
/// the twin's.
fn lasting_offset(run: &Run, twin: &Run) -> f64 {
    let last = twin.offsets.last().map_or(0.0, |&(pts, _)| pts) - 1.0;
    let mean = |r: &Run| {
        let (sum, n) = r
            .offsets
            .iter()
            .filter(|(pts, _)| *pts >= last)
            .fold((0.0, 0u32), |(sum, n), (_, o)| (sum + o, n + 1));
        if n == 0 { f64::NAN } else { sum / f64::from(n) }
    };
    (mean(run) - mean(twin)) * 1000.0
}

/// Frames inserted into the encoder's audio by drift correction and the
/// pacer together. Unlike a PTS-relative offset this does not trust the
/// Stamper's map, so a wrong timestamp and an equal wrong edit cannot hide
/// one another.
fn net_edit(run: &Run) -> i128 {
    i128::from(run.edited) + i128::from(run.pacer.silence) - i128::from(run.pacer.dropped)
}

/// What the device input reported in a run, by kind, with sizes in ms.
#[derive(Debug, Clone, Default, PartialEq)]
struct Notices {
    /// Each gap's size.
    gaps: Vec<f64>,
    /// Each rewind's size.
    rewinds: Vec<f64>,
    /// Each glitch's furthest offset.
    glitches: Vec<f64>,
    /// Overflows.
    overflows: usize,
}

impl Notices {
    /// The notices of `run`.
    fn of(run: &Run) -> Self {
        let mut notices = Self::default();
        let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
        for d in &run.found {
            match d {
                Discontinuity::Gap { by, .. } => notices.gaps.push(ms(by)),
                Discontinuity::Rewind { by, .. } => notices.rewinds.push(ms(by)),
                Discontinuity::Glitch { by, .. } => notices.glitches.push(ms(by)),
                Discontinuity::Overflow { .. } => notices.overflows += 1,
            }
        }
        notices
    }

    /// How many there were.
    fn len(&self) -> usize {
        self.gaps.len() + self.rewinds.len() + self.glitches.len() + self.overflows
    }
}

/// True when `sizes` (in ms) is one size, within `within` ms of `to`.
fn one_of(sizes: &[f64], to: f64, within: f64) -> bool {
    matches!(sizes, [by] if (by - to).abs() <= within)
}

/// `run`'s notices in short: the first few in order, and how many more.
fn described(run: &Run) -> String {
    let short: Vec<_> = run
        .found
        .iter()
        .take(4)
        .map(|d| match d {
            Discontinuity::Gap { seq, by } => format!("gap {:.2} ms @{seq}", ms_of(by)),
            Discontinuity::Rewind { seq, by } => format!("rewind {:.2} ms @{seq}", ms_of(by)),
            Discontinuity::Glitch { seq, blocks, by } => {
                format!("glitch {blocks}x {:.2} ms @{seq}", ms_of(by))
            }
            Discontinuity::Overflow { seq, blocks, .. } => format!("overflow {blocks} @{seq}"),
        })
        .collect();
    let more = run.found.len().saturating_sub(short.len());
    let more = if more > 0 {
        format!(" +{more} more")
    } else {
        String::new()
    };
    format!("[{}]{more}", short.join(", "))
}

/// `d` in ms.
fn ms_of(d: &Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// A measure in ms for a scorecard, or a dash when there was none.
fn shown(value: Option<f64>) -> String {
    value.map_or_else(|| "-".into(), |v| format!("{v:+.2} ms"))
}

/// `run`'s numbers against `twin`, for its scorecard.
fn numbers(run: &Run, twin: &Run) -> String {
    let (view, pacer) = (&run.view, &run.pacer);
    format!(
        "found {} | steps {} breaks {} | edited {:+} (twin {:+}) | pacer silence {} dropped {} \
         | apart {:.2} ms | walk {} | PTS walk {} | correction {:.1} ppm (twin {:.1})",
        described(run),
        view.drift_steps,
        view.drift_resets,
        run.edited,
        twin.edited,
        pacer.silence,
        pacer.dropped,
        run.apart(twin, 0.0),
        shown(walk(run, twin)),
        shown(pts_walk(run, twin)),
        view.correction_ppm,
        twin.view.correction_ppm
    )
}

/// Judges that `run` cut nothing against `twin`, its fault aside: no drift
/// step made up or break (the only ways drift correction cuts) beyond the
/// twin's, and nothing more dropped by the pacer. Resampling, which the
/// jitter left in the PTS can start, changes a few frames and cuts none.
fn no_cut(judge: &mut Judge, run: &Run, twin: &Run) {
    let (view, twins) = (&run.view, &twin.view);
    judge.require(
        "no drift step or break",
        (view.drift_steps, view.drift_resets) == (twins.drift_steps, twins.drift_resets),
    );
    judge.require("no pacer drop", run.pacer.dropped <= twin.pacer.dropped);
}

/// Judges that `run` left the pacer nothing to do and made up no step.
fn untouched_by(judge: &mut Judge, run: &Run) {
    let (view, pacer) = (&run.view, &run.pacer);
    judge.require(
        "no drift step or break",
        (view.drift_steps, view.drift_resets) == (0, 0),
    );
    judge.require("no pacer edit", (pacer.silence, pacer.dropped) == (0, 0));
}

/// Limits include their boundary, reject NaN and keep all failures.
#[test]
fn a_judge_fails_what_is_past_its_limit_or_not_a_number() {
    let mut judge = Judge::default();
    judge.at_most("apart", 1.0, 2.0);
    judge.require("ok", true);
    assert!(judge.failed.is_empty());
    assert_eq!(judge.worst, 0.5);
    judge.at_most("boundary", 2.0, 2.0);
    assert!(judge.failed.is_empty());
    judge.at_most("walk", 3.0, 2.0);
    judge.at_most("nothing", f64::NAN, 2.0);
    judge.require("notice", false);
    assert_eq!(judge.failed.len(), 3, "{:?}", judge.failed);
    assert_eq!(judge.worst, 1.5);
}

/// A failed criterion outranks passing cases when naming the worst case.
#[test]
fn a_scorecard_names_its_worst_case_and_counts_what_passed() {
    let mut card = Card::new("test family");
    let mut close = Judge::default();
    close.at_most("apart", 1.9, 2.0);
    card.add("close".into(), close, "numbers".into());
    let mut far = Judge::default();
    far.at_most("apart", 0.1, 2.0);
    card.add("far".into(), far, "numbers".into());
    let line = card.line();
    assert!(line.contains("2/2 passed; worst: close"), "{line}");
    let mut failed = Judge::default();
    failed.require("no rewind", false);
    card.add("failed".into(), failed, "numbers".into());
    let line = card.line();
    assert!(
        line.contains("2/3 passed; worst: failed: FAILED no rewind"),
        "{line}"
    );
}

/// A scorecard cannot turn a failing scenario into a passing family.
#[test]
#[should_panic(expected = "1 failed, worst first")]
fn a_scorecard_with_a_failed_case_fails_its_family() {
    let mut card = Card::new("test family");
    let mut judge = Judge::default();
    judge.require("no rewind", false);
    card.add("failed".into(), judge, "numbers".into());
    card.finish();
}

/// Grid selection and buffer times preserve each requested scenario.
#[test]
fn sweeps_pick_their_grid_and_buffers_their_times() {
    let quick = Sweep {
        full: false,
        print: false,
    };
    let full = Sweep {
        full: true,
        print: true,
    };
    assert_eq!(quick.pick(&[1], &[1, 2]), vec![1]);
    assert_eq!(full.pick(&[1], &[1, 2]), vec![1, 2]);
    assert_eq!((quick.of(3, 4), full.of(3, 4)), (3, 4));
    // An event at a buffer's start comes just before its callback.
    for (frames, rate) in [(128u64, 44_100u32), (4096, 48_000), (512, 96_000)] {
        for n in [0u64, 1, 7, 1000] {
            let at = buffer_at(n, frames, rate);
            assert!(at <= (n * frames) as f64 / f64::from(rate));
            assert_eq!(buffer_index(at + 1e-9, frames, rate), n);
        }
    }
    assert_eq!(block_tolerance(4096, 48_000), tolerance(1024, 48_000));
}

/// Missing observations fail their limits instead of looking perfectly aligned.
#[test]
fn an_empty_measurement_does_not_pass_as_zero_error() {
    let mut run = play(Device::new(RATE, 512), 1);
    let twin = play(Device::new(RATE, 512), 1);
    assert!(mean_apart(&run, &twin, 2.0, 3.0).is_nan());
    assert!(pts_mean(&run, 2.0, 3.0).is_nan());
    run.offsets.clear();
    assert!(lasting_offset(&run, &twin).is_nan());
    assert!(walk_size(&run, &twin).is_nan());
}

/// Frame accounting includes both edit stages without trusting their timestamps.
#[test]
fn net_edits_count_fills_and_cuts_independently_of_pts() {
    let mut run = play(Device::new(RATE, 512), 1);
    run.edited = -12;
    run.pacer.silence = 30;
    run.pacer.dropped = 7;
    run.offsets.clear();
    assert_eq!(net_edit(&run), 11);
}
