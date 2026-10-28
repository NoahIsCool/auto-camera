//! Tests of the presentation buffer: unit cases on hand-made frames, then
//! simulated sources on a [`VirtualClock`], read the way the engine thread
//! reads them: a latest-frame mailbox per source, collected whenever the
//! thread wakes (every [`SLICE`] at most, and at every tick), and one
//! presentation per tick.
//!
//! The simulated program ticks at exact multiples of 1/30 s after
//! [`START`]. (The engine's tick period is 1/30 s rounded down to whole
//! nanoseconds, which moves a 29.97 fps source's repeat by one tick about
//! once an hour; that is the tick schedule's rounding, not the buffer's.)

use std::sync::Arc;

use ac_sources::{Clock, FrameRate, VirtualClock};

use super::*;

/// Longest wait of the simulated engine thread between ticks, as the
/// runtime's `WAIT_SLICE`.
const SLICE: Duration = Duration::from_millis(10);

/// Clock reading at which the simulated program's first tick is due; the
/// sources start at clock zero, so they are running by then.
const START: Duration = Duration::from_secs(1);

/// `v` milliseconds.
fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

/// The program's frame rate, 30 fps.
fn program() -> FrameRate {
    FrameRate::new(30, 1).unwrap()
}

/// The program's tick period, 1/30 s.
fn tick_period() -> Duration {
    program().frame_interval()
}

/// A rate of `num / den` fps.
fn rate(num: u32, den: u32) -> FrameRate {
    FrameRate::new(num, den).unwrap()
}

/// A 2×2 frame of source `id`.
fn frame(id: &str, epoch: u64, seq: u64, pts: Duration) -> VideoFrame {
    VideoFrame::new(
        SourceId(id.into()),
        seq,
        pts,
        2,
        2,
        Arc::from(vec![0u8; 16]),
    )
    .unwrap()
    .with_epoch(epoch)
}

/// A pseudo-random delay of up to `max` for frame `seq` (splitmix64 of
/// `seed` and `seq`), the same on every run.
fn jitter(seed: u64, seq: u64, max: Duration) -> Duration {
    let mix = |mut z: u64| {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let range = u64::try_from(max.as_nanos()).unwrap() + 1;
    Duration::from_nanos(mix(seed ^ mix(seq)) % range)
}

/// When a simulated source delivers one frame.
#[derive(Debug, Clone, Copy)]
struct Planned {
    /// Connection epoch.
    epoch: u64,
    /// PTS.
    pts: Duration,
    /// Clock reading at which the frame is published; never decreases from
    /// one frame to the next, as with a real source.
    arrival: Duration,
}

impl Planned {
    /// An epoch-0 frame at `pts`, published `delay` after its PTS.
    fn at(pts: Duration, delay: Duration) -> Self {
        Self {
            epoch: 0,
            pts,
            arrival: pts + delay,
        }
    }
}

/// A simulated source publishing frames into a latest-frame mailbox.
struct SimSource {
    /// Source id.
    id: String,
    /// When each sequence number is delivered.
    plan: Box<dyn Fn(u64) -> Planned>,
    /// Next sequence number to publish.
    next: u64,
    /// The newest published frame.
    mailbox: Option<VideoFrame>,
}

impl SimSource {
    /// A source `id` delivering by `plan`.
    fn new(id: &str, plan: impl Fn(u64) -> Planned + 'static) -> Self {
        Self {
            id: id.into(),
            plan: Box::new(plan),
            next: 0,
            mailbox: None,
        }
    }

    /// A source `id` at `fps`, each frame published `delay` after its
    /// PTS, give or take half of `spread` (seeded by `seed`).
    fn steady(id: &str, fps: FrameRate, delay: Duration, spread: Duration, seed: u64) -> Self {
        Self::new(id, move |seq| {
            let pts = fps.pts(seq);
            Planned {
                epoch: 0,
                pts,
                arrival: (pts + delay + jitter(seed, seq, spread)).saturating_sub(spread / 2),
            }
        })
    }

    /// Publishes every frame due by clock reading `now`; the mailbox keeps
    /// the newest.
    fn publish_until(&mut self, now: Duration) {
        loop {
            let p = (self.plan)(self.next);
            if p.arrival > now {
                return;
            }
            self.mailbox = Some(frame(&self.id, p.epoch, self.next, p.pts));
            self.next += 1;
        }
    }
}

/// How a buffer answered at one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// [`Presentation::Empty`].
    Empty,
    /// [`Presentation::New`].
    New,
    /// [`Presentation::Repeat`].
    Repeat,
    /// [`Presentation::Stale`].
    Stale,
}

/// What one source showed at one tick.
#[derive(Debug, Clone, Copy)]
struct Record {
    /// Clock reading at which the tick was due.
    due: Duration,
    /// The buffer's answer.
    kind: Kind,
    /// Epoch and sequence number of the frame presented.
    shown: Option<(u64, u64)>,
    /// Epoch and sequence number of the newest frame in the mailbox at the
    /// tick: what the engine showed before it had presentation buffers.
    naive: Option<(u64, u64)>,
}

/// Runs `ticks` ticks of the 30 fps program on a virtual clock, as the
/// engine thread does: it waits for each tick in slices of at most
/// [`SLICE`], collecting every source's newest frame whenever it wakes, and
/// presents at the tick. `shift` moves every tick from the given index on
/// later by the given amount, as the phase lock does. Returns each source's
/// records, tick by tick, and the buffers.
fn simulate(
    sources: &mut [SimSource],
    ticks: u64,
    shift: Option<(u64, Duration)>,
) -> (Vec<Vec<Record>>, Vec<PresentationBuffer>) {
    let clock = VirtualClock::new();
    let mut buffers: Vec<PresentationBuffer> = sources
        .iter()
        .map(|s| PresentationBuffer::new(SourceId(s.id.clone())))
        .collect();
    let mut records = vec![Vec::new(); sources.len()];
    for n in 0..ticks {
        let moved = shift
            .filter(|&(from, _)| n >= from)
            .map_or(Duration::ZERO, |s| s.1);
        let due = START + program().pts(n) + moved;
        loop {
            let now = clock.now();
            for (s, b) in sources.iter_mut().zip(&mut buffers) {
                s.publish_until(now);
                if let Some(f) = &s.mailbox {
                    b.offer(f.clone(), now);
                }
            }
            if now >= due {
                break;
            }
            clock.sleep((due - now).min(SLICE));
        }
        for ((s, b), out) in sources.iter().zip(&mut buffers).zip(&mut records) {
            let p = b.present(due);
            let kind = match &p {
                Presentation::Empty => Kind::Empty,
                Presentation::New(_) => Kind::New,
                Presentation::Repeat(_) => Kind::Repeat,
                Presentation::Stale => Kind::Stale,
            };
            let tag = |f: &VideoFrame| (f.epoch, f.seq);
            out.push(Record {
                due,
                kind,
                shown: p.frame().map(tag),
                naive: s.mailbox.as_ref().map(tag),
            });
        }
    }
    (records, buffers)
}

/// The sequence numbers presented at `records`, which must all show one.
fn shown(records: &[Record]) -> Vec<u64> {
    records
        .iter()
        .map(|r| r.shown.expect("a frame at every tick").1)
        .collect()
}

/// The sequence numbers in the mailbox at `records`.
fn naive(records: &[Record]) -> Vec<u64> {
    records
        .iter()
        .map(|r| r.naive.expect("a frame at every tick").1)
        .collect()
}

/// Differences between consecutive sequence numbers: 1 is one frame per
/// tick, 0 a repeat, 2 or more a skip, below 0 a step backwards.
fn steps(seqs: &[u64]) -> Vec<i64> {
    seqs.windows(2)
        .map(|w| i64::try_from(w[1]).unwrap() - i64::try_from(w[0]).unwrap())
        .collect()
}

/// Ticks of `records` with `kind`.
fn ticks_with(records: &[Record], kind: Kind) -> Vec<usize> {
    (0..records.len())
        .filter(|&n| records[n].kind == kind)
        .collect()
}

#[test]
fn nothing_is_presented_before_the_first_frame() {
    let mut b = PresentationBuffer::new(SourceId("cam".into()));
    assert!(b.is_empty() && !b.is_locked());
    assert!(matches!(b.present(START), Presentation::Empty));
    assert!(Presentation::Empty.frame().is_none());
    assert!(Presentation::Stale.into_frame().is_none());
}

#[test]
fn until_it_holds_two_frames_the_buffer_presents_the_newest() {
    let mut b = PresentationBuffer::new(SourceId("cam".into()));
    b.offer(frame("cam", 0, 7, ms(700)), ms(700));
    let p = b.present(ms(710));
    assert!(matches!(&p, Presentation::New(f) if f.seq == 7));
    assert_eq!(p.into_frame().map(|f| f.seq), Some(7));
    assert!(matches!(b.present(ms(743)), Presentation::Repeat(f) if f.seq == 7));
    assert!(!b.is_locked(), "one frame gives no period");
}

#[test]
fn frames_of_other_sources_and_frames_not_newer_are_ignored() {
    let mut b = PresentationBuffer::new(SourceId("cam".into()));
    assert_eq!(b.source(), &SourceId("cam".into()));
    b.offer(frame("other", 0, 1, ms(0)), ms(0));
    assert!(b.is_empty());
    b.offer(frame("cam", 0, 5, ms(500)), ms(500));
    b.offer(frame("cam", 0, 5, ms(500)), ms(510)); // the same, read again
    b.offer(frame("cam", 0, 3, ms(300)), ms(520)); // older
    assert_eq!(b.len(), 1);
    assert_eq!(b.held[0].at, ms(500), "the first reading is kept");
}

#[test]
fn a_newer_epoch_drops_the_held_frames_and_an_older_one_is_ignored() {
    let mut b = PresentationBuffer::new(SourceId("cam".into()));
    for seq in 0..3 {
        b.offer(frame("cam", 0, seq, ms(seq * 40)), ms(seq * 40));
    }
    assert!(matches!(b.present(ms(100)), Presentation::New(_)));
    assert!(b.is_locked());
    b.offer(frame("cam", 1, 3, ms(2000)), ms(2000));
    assert_eq!((b.len(), b.is_locked()), (1, false), "a new connection");
    b.offer(frame("cam", 0, 4, ms(160)), ms(2010)); // the old connection
    assert_eq!(b.len(), 1);
    assert!(matches!(b.present(ms(2010)), Presentation::New(f) if f.epoch == 1));
}

#[test]
fn a_pts_that_does_not_increase_breaks_the_timeline() {
    let mut b = PresentationBuffer::new(SourceId("cam".into()));
    b.offer(frame("cam", 0, 0, ms(0)), ms(0));
    b.offer(frame("cam", 0, 1, ms(40)), ms(40));
    assert!(matches!(b.present(ms(50)), Presentation::New(_)));
    assert!(b.is_locked());
    b.offer(frame("cam", 0, 2, ms(40)), ms(80));
    assert_eq!((b.len(), b.is_locked()), (1, false));
    assert!(matches!(b.present(ms(90)), Presentation::New(f) if f.seq == 2));
}

#[test]
fn the_ring_never_grows_past_its_capacity() {
    let mut b = PresentationBuffer::new(SourceId("cam".into()));
    let capacity = b.held.capacity();
    assert!(capacity >= CAPACITY);
    for seq in 0..10_000 {
        b.offer(frame("cam", 0, seq, ms(seq * 33)), ms(seq * 33));
        assert!(b.len() <= CAPACITY);
        if seq % 7 == 0 {
            b.present(ms(seq * 33));
        }
    }
    assert_eq!(b.len(), CAPACITY);
    assert_eq!(b.held.capacity(), capacity, "never reallocated");
    let seqs: Vec<u64> = b.held.iter().map(|h| h.frame.seq).collect();
    assert_eq!(seqs, vec![9996, 9997, 9998, 9999], "the newest are kept");
}

#[test]
fn the_presented_sequence_never_steps_backwards() {
    // Unlocked, the newest frame is presented; the lock then puts the
    // target one period behind it, on a frame already passed.
    let mut b = PresentationBuffer::new(SourceId("cam".into()));
    b.offer(frame("cam", 0, 4, ms(160)), ms(160));
    assert!(matches!(b.present(ms(161)), Presentation::New(f) if f.seq == 4));
    b.offer(frame("cam", 0, 5, ms(200)), ms(200));
    // The lock: a 40 ms period, and frame 5 was collected at its PTS, so
    // the target at 201 ms is 161 ms, nearest frame 4.
    assert!(matches!(b.present(ms(201)), Presentation::Repeat(f) if f.seq == 4));
    assert!(matches!(b.present(ms(241)), Presentation::New(f) if f.seq == 5));
    b.offer(frame("cam", 0, 6, ms(240)), ms(240));
    // A target that moved back (a tick due earlier, which the runtime never
    // asks for) still repeats instead of going back.
    assert!(matches!(b.present(ms(150)), Presentation::Repeat(f) if f.seq == 5));
}

#[test]
fn a_target_on_a_midpoint_takes_the_older_frame_whatever_the_rounding() {
    // Frames 40 ms apart, each collected at its PTS: the lock puts the
    // target 40 ms before the due time, so a tick due at 140 ms aims at
    // 100 ms, midway between frames 2 and 3. A nanosecond either way must
    // not decide.
    let ns = Duration::from_nanos(1);
    for due in [ms(140) - ns, ms(140), ms(140) + ns] {
        let mut b = PresentationBuffer::new(SourceId("cam".into()));
        for seq in 0..4 {
            b.offer(frame("cam", 0, seq, ms(seq * 40)), ms(seq * 40));
        }
        assert!(
            matches!(b.present(due), Presentation::New(f) if f.seq == 2),
            "due at {due:?}"
        );
    }
}

#[test]
fn buffers_are_made_for_every_source_but_the_reference() {
    let ids = ["a", "b", "c"].map(|s| SourceId(s.into()));
    let made = buffers_for(ids.clone(), 1);
    let sources: Vec<Option<&SourceId>> = made
        .iter()
        .map(|b| b.as_ref().map(PresentationBuffer::source))
        .collect();
    assert_eq!(sources, vec![Some(&ids[0]), None, Some(&ids[2])]);
    assert!(buffers_for(ids.clone(), 9).iter().all(Option::is_some));
    assert!(buffers_for(Vec::new(), 0).is_empty());
}

#[test]
fn a_29_97_fps_source_into_30_fps_repeats_exactly_one_frame_per_1001_ticks() {
    let cycles = 12;
    let mut sources = [SimSource::steady(
        "ntsc",
        rate(30_000, 1001),
        ms(0),
        ms(3),
        1,
    )];
    let (records, _) = simulate(&mut sources, cycles * 1001, None);
    let records = &records[0];
    let seqs = shown(records);
    let steps = steps(&seqs);
    let odd: Vec<(usize, i64)> = (0..steps.len())
        .map(|n| (n + 1, steps[n]))
        .filter(|&(_, s)| s != 0 && s != 1)
        .collect();
    assert!(odd.is_empty(), "never backwards, never a skip: {odd:?}");
    let repeats = ticks_with(records, Kind::Repeat);
    assert!(
        repeats.windows(2).all(|w| w[1] - w[0] == 1001),
        "{repeats:?}"
    );
    assert!(repeats.len() as u64 >= cycles - 1, "{repeats:?}");
    assert_eq!(
        repeats.len() + ticks_with(records, Kind::New).len(),
        records.len(),
        "a frame at every tick, never stale"
    );
    // Reading the newest frame at each tick instead stutters each time the
    // frames drift across the ticks.
    let naive = self::steps(&naive(records));
    assert!(naive.iter().any(|&s| s >= 2), "the naive reading skips");
    let naive_repeats = naive.iter().filter(|&&s| s == 0).count();
    assert!(naive_repeats as u64 > cycles, "{naive_repeats} repeats");
}

#[test]
fn two_jittered_sources_at_the_tick_rate_show_one_frame_per_tick() {
    // `edge` publishes its frames on the ticks, give or take 3 ms: the
    // collision the phase lock cures for the reference only. `mid`
    // publishes them 12 ms after the ticks, give or take 4 ms.
    let fps = rate(30, 1);
    let mut sources = [
        SimSource::steady("edge", fps, ms(0), ms(6), 2),
        SimSource::steady("mid", fps, ms(12), ms(8), 3),
    ];
    let (records, _) = simulate(&mut sources, 1800, None);
    for r in &records {
        let seqs = shown(r);
        let steps = steps(&seqs);
        let odd: Vec<(usize, i64)> = (0..steps.len())
            .map(|n| (n + 1, steps[n]))
            .filter(|&(_, s)| s != 1)
            .collect();
        assert!(
            odd.is_empty(),
            "no repeat, no skip, no double skip: {odd:?}"
        );
        let behind: Vec<u64> = r
            .iter()
            .map(|x| x.naive.unwrap().1 - x.shown.unwrap().1)
            .collect();
        assert!(behind.iter().all(|&b| b <= 3), "{behind:?}");
    }
    // The newest frame at each tick: the edge source repeats and skips.
    let naive = steps(&naive(&records[0]));
    let (repeats, skips) = (
        naive.iter().filter(|&&s| s == 0).count(),
        naive.iter().filter(|&&s| s >= 2).count(),
    );
    assert!(
        repeats > 100 && skips > 100,
        "{repeats} repeats, {skips} skips"
    );
}

#[test]
fn faster_and_slower_sources_are_decimated_and_held_evenly() {
    let mut sources = [
        SimSource::steady("sixty", rate(60, 1), ms(3), ms(4), 4),
        SimSource::steady("fifteen", rate(15, 1), ms(7), ms(8), 5),
    ];
    let (records, _) = simulate(&mut sources, 900, None);
    let sixty = steps(&shown(&records[0]));
    assert!(
        sixty.iter().all(|&s| s == 2),
        "every other frame: {sixty:?}"
    );
    let fifteen = shown(&records[1]);
    for seq in fifteen[2]..fifteen[fifteen.len() - 3] {
        let times = fifteen.iter().filter(|&&s| s == seq).count();
        assert_eq!(times, 2, "frame {seq} shown on two ticks");
    }
}

#[test]
fn a_frame_late_once_is_repeated_and_skipped_without_locking_again() {
    // A 40 ms hiccup at 3 s: the frames due in it are published together
    // when it ends, so the mailbox keeps only the newer of them.
    let fps = rate(30, 1);
    let hiccup = (ms(3000), ms(3040));
    let plan = move |seq| {
        let mut p = Planned::at(fps.pts(seq), jitter(6, seq, ms(4)));
        if (hiccup.0..hiccup.1).contains(&p.arrival) {
            p.arrival = hiccup.1;
        }
        p
    };
    // The lock taken at the first tick, long before the hiccup.
    let (_, first) = simulate(&mut [SimSource::new("cam", plan)], 30, None);
    let before = first[0].lock;
    assert!(before.is_some());
    let (records, buffers) = simulate(&mut [SimSource::new("cam", plan)], 180, None);
    let steps = steps(&shown(&records[0]));
    assert_eq!(steps.iter().filter(|&&s| s == 0).count(), 1, "{steps:?}");
    assert_eq!(steps.iter().filter(|&&s| s == 2).count(), 1, "{steps:?}");
    assert!(steps.iter().all(|&s| (0..=2).contains(&s)));
    assert_eq!(buffers[0].lock, before, "the lock did not move");
}

#[test]
fn a_source_that_stays_later_than_its_lock_is_locked_again() {
    // From 3 s on, every frame is published 50 ms later than before.
    let fps = rate(30, 1);
    let late_from = ms(3000);
    let plan = move |seq| {
        let pts = fps.pts(seq);
        let extra = if pts >= late_from { ms(50) } else { ms(0) };
        Planned::at(pts, extra + jitter(7, seq, ms(4)))
    };
    let (_, first) = simulate(&mut [SimSource::new("cam", plan)], 30, None);
    let first = first[0].lock.unwrap();
    let (records, buffers) = simulate(&mut [SimSource::new("cam", plan)], 240, None);
    let r = &records[0];
    let steps = steps(&shown(r));
    assert!(steps.iter().all(|&s| s == 0 || s == 1), "{steps:?}");
    let repeats = steps.iter().filter(|&&s| s == 0).count();
    assert!((1..=3).contains(&repeats), "{repeats} repeats absorb 50 ms");
    let tail = &steps[steps.len() - 120..];
    assert!(tail.iter().all(|&s| s == 1), "{tail:?}");
    // The target now runs about 50 ms further behind the clock.
    let moved = first.offset - buffers[0].lock.unwrap().offset;
    assert!((30_000_000..=70_000_000).contains(&moved), "{moved} ns");
}

#[test]
fn a_target_left_behind_by_its_frames_is_locked_again_at_once() {
    // A slow start: frames are published 150 ms after their PTS until 2 s,
    // then the decoder catches up (the backlog all at once) and publishes
    // them 5 ms after.
    let fps = rate(30, 1);
    let until = ms(2000);
    let backlog_end = until + ms(150);
    let mut sources = [SimSource::new("cam", move |seq| {
        let pts = fps.pts(seq);
        let arrival = if pts < until {
            pts + ms(150)
        } else {
            (pts + ms(5)).max(backlog_end)
        };
        Planned {
            epoch: 0,
            pts,
            arrival,
        }
    })];
    let (records, buffers) = simulate(&mut sources, 150, None);
    let r = &records[0];
    let steps = steps(&shown(r));
    // One jump forward over the frames the mailbox replaced in the burst,
    // on the first tick after it (not after RELOCK_AFTER late ticks); the
    // target may then wait on that frame for one tick.
    let odd: Vec<(usize, i64)> = (0..steps.len())
        .map(|n| (n + 1, steps[n]))
        .filter(|&(_, s)| s != 1)
        .collect();
    assert!(odd.len() <= 2 && odd[0].1 >= 3, "{odd:?}");
    let jump = odd[0].0;
    assert!(r[jump - 1].due < backlog_end && r[jump].due >= backlog_end);
    assert!(odd[1..].iter().all(|&(_, s)| s == 0), "{odd:?}");
    let tail = &steps[steps.len() - 100..];
    assert!(tail.iter().all(|&s| s == 1), "{tail:?}");
    let behind = r.last().map(|x| x.naive.unwrap().1 - x.shown.unwrap().1);
    assert!(behind.is_some_and(|b| b <= 2), "{behind:?}");
    assert!(buffers[0].is_locked());
}

#[test]
fn a_source_that_stalls_goes_stale_and_resumes() {
    // Frames due from 2.5 s on are published only at 5 s, the newest of
    // them at once, as a decoder that hung catches up.
    let fps = rate(30, 1);
    let (stall, resume) = (ms(2500), ms(5000));
    let mut sources = [SimSource::new("cam", move |seq| {
        let mut p = Planned::at(fps.pts(seq), ms(5) + jitter(8, seq, ms(6)));
        if (stall..resume).contains(&p.arrival) {
            p.arrival = resume;
        }
        p
    })];
    let (records, _) = simulate(&mut sources, 240, None);
    let r = &records[0];
    // The last frame published before the stall.
    let last_arrival = (0..)
        .map(|seq| (sources[0].plan)(seq).arrival)
        .take_while(|&a| a < stall)
        .last()
        .unwrap();
    let stale = ticks_with(r, Kind::Stale);
    let (first, end) = (stale[0], *stale.last().unwrap());
    let waited = r[first].due - last_arrival;
    assert!(
        waited > SOURCE_STALE_AFTER && waited <= SOURCE_STALE_AFTER + SLICE + tick_period(),
        "stale after {waited:?}"
    );
    assert_eq!(
        stale,
        (first..=end).collect::<Vec<_>>(),
        "stale until it resumes"
    );
    assert!(r[end].due < resume && r[end + 1].due >= resume);
    assert_eq!(r[end + 1].kind, Kind::New);
    // Before going stale, the last frame was repeated for at most the bound.
    let held = r[..first]
        .iter()
        .rev()
        .take_while(|x| x.kind == Kind::Repeat)
        .count();
    let bound = SOURCE_STALE_AFTER.as_nanos() / tick_period().as_nanos() + 2;
    assert!(held as u128 <= bound, "repeated {held} times");
    // Never backwards across the stall, and back to one frame per tick.
    let after: Vec<u64> = r.iter().filter_map(|x| x.shown.map(|s| s.1)).collect();
    assert!(steps(&after).iter().all(|&s| s >= 0));
    let tail = steps(&shown(&r[r.len() - 60..]));
    assert!(tail.iter().all(|&s| s == 1), "{tail:?}");
}

#[test]
fn a_reconnected_source_is_shown_from_its_new_epoch_on() {
    // Epoch 0 delivers until 3 s; after 2 s away the new connection's
    // frames carry PTS from the clock and arrive 20 ms after them.
    let fps = rate(30, 1);
    let mut sources = [SimSource::new("cam", move |seq| {
        if seq < 90 {
            Planned::at(fps.pts(seq), ms(5) + jitter(9, seq, ms(4)))
        } else {
            let pts = ms(5000) + fps.pts(seq - 90);
            Planned {
                epoch: 1,
                ..Planned::at(pts, ms(20) + jitter(9, seq, ms(4)))
            }
        }
    })];
    let (records, buffers) = simulate(&mut sources, 240, None);
    let r = &records[0];
    let switch = (0..r.len())
        .find(|&n| r[n].naive.is_some_and(|(epoch, _)| epoch == 1))
        .unwrap();
    assert!(
        r[switch..]
            .iter()
            .all(|x| x.shown.is_some_and(|(epoch, _)| epoch == 1)),
        "no frame of the old connection once the new one delivered"
    );
    assert!(
        r[..switch]
            .iter()
            .all(|x| x.shown.is_none_or(|(e, _)| e == 0))
    );
    assert!(!ticks_with(r, Kind::Stale).is_empty(), "stale while away");
    let tail = steps(&shown(&r[r.len() - 60..]));
    assert!(tail.iter().all(|&s| s == 1), "{tail:?}");
    assert!(buffers[0].is_locked());
}

#[test]
fn moving_the_ticks_costs_at_most_one_skip() {
    // The phase lock delays the schedule by half a period at tick 900.
    let fps = rate(30, 1);
    let mut sources = [
        SimSource::steady("edge", fps, ms(0), ms(6), 10),
        SimSource::steady("mid", fps, ms(12), ms(8), 11),
    ];
    let half = tick_period() / 2;
    let (records, _) = simulate(&mut sources, 1800, Some((900, half)));
    for r in &records {
        let steps = steps(&shown(r));
        let odd: Vec<(usize, i64)> = (0..steps.len())
            .map(|n| (n + 1, steps[n]))
            .filter(|&(_, s)| s != 1)
            .collect();
        assert!(
            odd.is_empty() || odd == [(900, 2)],
            "one frame per tick but at the shift: {odd:?}"
        );
    }
}
