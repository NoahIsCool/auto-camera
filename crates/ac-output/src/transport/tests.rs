//! Unit tests for the shared state and the writer threads (the readers'
//! are in `readers`).

use super::*;
use std::io::Read;
use std::sync::mpsc::sync_channel;

/// A writer that fails after `ok` successful writes.
struct FailAfter {
    /// Writes left before failing.
    ok: usize,
    /// Bytes accepted so far.
    written: Vec<u8>,
}

impl Write for FailAfter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.ok == 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.ok -= 1;
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Runs the video writer over `frames` with `debt` dropped frames owed
/// up front, returning the bytes written and the shared state.
fn run_video(frames: &[&[u8]], debt: u64, stopping: bool) -> (Vec<u8>, Arc<Shared>) {
    let shared = Arc::new(Shared::default());
    let (tx, rx) = sync_channel(frames.len().max(1));
    for f in frames {
        shared.video_queued.fetch_add(1, Ordering::SeqCst);
        tx.send(f.to_vec()).unwrap();
    }
    drop(tx);
    shared.video_debt.store(debt, Ordering::SeqCst);
    shared.stop.store(stopping, Ordering::SeqCst);
    let mut out = Vec::new();
    video_writer(rx, &mut out, shared.clone());
    (out, shared)
}

#[test]
fn video_writer_repays_drops_with_the_previous_frame() {
    let (out, shared) = run_video(&[&[1, 1], &[2, 2]], 2, false);
    // Nothing to repeat before frame 1; the debt is paid before frame 2.
    assert_eq!(out, [1, 1, 1, 1, 1, 1, 2, 2]);
    assert_eq!(shared.frames_written.load(Ordering::SeqCst), 4);
    assert_eq!(shared.frames_repeated.load(Ordering::SeqCst), 2);
    assert_eq!(shared.video_queued.load(Ordering::SeqCst), 0);
    assert!(shared.video_closed.load(Ordering::SeqCst));
    assert_eq!(shared.last_error(), None);
}

#[test]
fn video_writer_settles_debt_at_close_unless_stopping() {
    let (out, _) = run_video(&[&[7]], 2, false);
    assert_eq!(out, [7, 7, 7]);
    let (out, shared) = run_video(&[&[7]], 2, true);
    assert_eq!(out, [7]);
    assert_eq!(shared.frames_repeated.load(Ordering::SeqCst), 0);
    // No frame at all: nothing written, nothing to repeat.
    let (out, _) = run_video(&[], 3, false);
    assert!(out.is_empty());
}

#[test]
fn video_writer_records_pipe_errors() {
    let shared = Arc::new(Shared::default());
    let (tx, rx) = sync_channel(4);
    shared.video_queued.store(2, Ordering::SeqCst);
    tx.send(vec![1u8]).unwrap();
    tx.send(vec![2u8]).unwrap();
    let sink = FailAfter {
        ok: 1,
        written: Vec::new(),
    };
    video_writer(rx, sink, shared.clone());
    assert!(shared.last_error().unwrap().starts_with("video pipe:"));
    assert!(shared.video_closed.load(Ordering::SeqCst));
    // The receiver is gone, so further sends fail.
    assert!(tx.try_send(vec![3]).is_err());
}

#[test]
fn samples_are_little_endian_and_sanitised() {
    let mut out = Vec::new();
    let mut bytes = Vec::new();
    write_samples(&mut out, &[1.0, f32::NAN, -0.5, f32::INFINITY], &mut bytes).unwrap();
    let back: Vec<f32> = out
        .chunks(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(back, [1.0, 0.0, -0.5, 0.0]);
}

#[test]
fn audio_debt_becomes_silence() {
    let shared = Shared::default();
    shared
        .audio_debt
        .store(SILENCE_CHUNK as u64 + 3, Ordering::SeqCst);
    let mut out = Vec::new();
    repay_audio(&mut out, &shared, &mut Vec::new()).unwrap();
    assert_eq!(out.len(), (SILENCE_CHUNK + 3) * 4);
    assert!(out.iter().all(|&b| b == 0));
    assert_eq!(
        shared.audio_silence_samples.load(Ordering::SeqCst),
        SILENCE_CHUNK as u64 + 3
    );
    assert_eq!(shared.audio_debt.load(Ordering::SeqCst), 0);
}

#[test]
fn debt_saturates_at_cap() {
    let d = AtomicU64::new(0);
    add_debt(&d, 3, 5);
    add_debt(&d, 3, 5);
    assert_eq!(d.load(Ordering::SeqCst), 5);
    add_debt(&d, u64::MAX, u64::MAX);
    assert_eq!(d.load(Ordering::SeqCst), u64::MAX);
}

#[test]
fn audio_writer_streams_over_loopback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let shared = Arc::new(Shared::default());
    let (tx, rx) = sync_channel(4);
    tx.send(vec![0.25f32, -0.25]).unwrap();
    drop(tx);
    let s = shared.clone();
    let writer = thread::spawn(move || audio_writer(rx, listener, s, Duration::from_secs(5)));
    let mut client = TcpStream::connect(addr).unwrap();
    let mut got = Vec::new();
    client.read_to_end(&mut got).unwrap();
    writer.join().unwrap();
    assert_eq!(got.len(), 8);
    assert_eq!(f32::from_le_bytes(got[..4].try_into().unwrap()), 0.25);
    assert!(shared.audio_connected.load(Ordering::SeqCst));
    assert_eq!(shared.audio_samples_written.load(Ordering::SeqCst), 2);
    assert_eq!(shared.last_error(), None);
}

#[test]
fn audio_accept_times_out_or_aborts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let shared = Arc::new(Shared::default());
    let (_tx, rx) = sync_channel::<Vec<f32>>(1);
    audio_writer(rx, listener, shared.clone(), Duration::from_millis(20));
    assert!(shared.last_error().unwrap().contains("did not connect"));
    assert!(shared.audio_closed.load(Ordering::SeqCst));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let shared = Arc::new(Shared::default());
    shared.stdout_closed.store(true, Ordering::SeqCst);
    let (_tx, rx) = sync_channel::<Vec<f32>>(1);
    audio_writer(rx, listener, shared.clone(), Duration::from_secs(30));
    assert!(shared.last_error().unwrap().contains("exited before"));
}

#[test]
fn pipe_errors_never_hide_ffmpeg_errors() {
    let shared = Shared::default();
    shared.record_pipe_error("video pipe: broken");
    assert_eq!(shared.last_error().as_deref(), Some("video pipe: broken"));
    shared.record_error("ffmpeg: the cause");
    shared.record_pipe_error("audio pipe: broken");
    assert_eq!(shared.last_error().as_deref(), Some("ffmpeg: the cause"));
    shared.record_stream_error("fifo: Error opening");
    shared.record_pipe_error("audio pipe: broken");
    assert_eq!(shared.last_error().as_deref(), Some("fifo: Error opening"));
}

#[test]
fn a_reconnection_clears_only_the_streams_error() {
    let shared = Shared::default();
    // Nothing to clear.
    shared.clear_stream_error();
    assert_eq!(shared.last_error(), None);
    shared.record_stream_error("fifo: Error opening rtmp://host/app/<redacted>");
    shared.clear_stream_error();
    assert_eq!(shared.last_error(), None, "the stream recovered");
    // An older error of something else shows again.
    shared.record_error("aac: something else");
    shared.record_stream_error("fifo: Error opening rtmp://host/app/<redacted>");
    shared.clear_stream_error();
    assert_eq!(shared.last_error().as_deref(), Some("aac: something else"));
    // An error after the stream's is not the stream's to clear.
    shared.record_stream_error("tcp: Connection refused");
    shared.record_error("ffmpeg: a newer cause");
    shared.clear_stream_error();
    assert_eq!(
        shared.last_error().as_deref(),
        Some("ffmpeg: a newer cause")
    );
}

#[test]
fn errors_are_truncated_and_buffers_pooled() {
    let shared = Shared::default();
    shared.record_error(&"x".repeat(10 * MAX_ERROR_CHARS));
    assert_eq!(shared.last_error().unwrap().len(), MAX_ERROR_CHARS);
    assert!(shared.take_buffer().is_empty());
    for _ in 0..POOL_FRAMES + 3 {
        shared.recycle(vec![0u8; 4]);
    }
    assert_eq!(lock(&shared.pool).len(), POOL_FRAMES);
    assert_eq!(shared.take_buffer().len(), 4);
}
