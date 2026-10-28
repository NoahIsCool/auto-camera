//! Decoder stderr in the app's log.
//!
//! # Theory
//!
//! Decoders run with `-loglevel error`, so whatever an ffmpeg decoder
//! writes to stderr is a complaint worth keeping: a corrupt packet, a file
//! that went away. [`forward`] pipes each child's stderr to a short-lived
//! thread (`ac-src-log`) that reads it line by line and hands every line
//! to `tracing` as a warning of target [`TARGET`], with the child's pid and
//! its input, so the app's log says which file a line is about. The source
//! takes no decision from these lines: its health still comes from the
//! decoder's output and exit. This crate installs no subscriber; the app
//! decides where the lines go.
//!
//! # Invariants
//!
//! * The thread owns the pipe and ends at its end of file, which the
//!   child's exit or kill brings; [`crate::process::Children`] kills every
//!   child it owns, so no drain outlives its source by more than that.
//! * The pipe is always drained, so a child never blocks on a full pipe,
//!   and memory is bounded: a line is cut at [`MAX_LINE`] bytes (the rest
//!   of it is skipped and the line marked).
//! * Empty lines are not logged; line endings are trimmed.

use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, Command};

/// The target of decoder stderr lines in the app's log.
pub(crate) const TARGET: &str = "ac_sources::ffmpeg";

/// Longest line logged, in bytes; the rest of a longer line is skipped.
pub(crate) const MAX_LINE: usize = 2000;

/// Marker appended to a line that was cut at [`MAX_LINE`].
const CUT: &str = " [...]";

/// Starts the thread that sends `child`'s stderr to the log, if `child`
/// has a stderr pipe; `command` is what started it. Fails only when the
/// thread cannot be spawned (the caller then kills the child, whose
/// writes would otherwise hit a closed pipe).
pub(crate) fn forward(child: &mut Child, command: &Command) -> io::Result<()> {
    let Some(stderr) = child.stderr.take() else {
        return Ok(());
    };
    let (pid, input) = (child.id(), input_of(command));
    std::thread::Builder::new()
        .name("ac-src-log".into())
        .spawn(move || {
            drain(BufReader::new(stderr), |line| {
                tracing::warn!(target: TARGET, pid, input = %input, "{line}");
            });
        })?;
    Ok(())
}

/// What `command` decodes: the argument after its last `-i`, else its
/// program.
pub(crate) fn input_of(command: &Command) -> String {
    let args: Vec<_> = command.get_args().collect();
    let input = args
        .iter()
        .rposition(|a| *a == "-i")
        .and_then(|i| args.get(i + 1))
        .copied()
        .unwrap_or_else(|| command.get_program());
    input.to_string_lossy().into_owned()
}

/// Calls `each` with every non-empty line of `input` (cut at
/// [`MAX_LINE`]) until it ends or fails.
pub(crate) fn drain(mut input: impl BufRead, mut each: impl FnMut(&str)) {
    while let Some(line) = read_line(&mut input, MAX_LINE) {
        if !line.trim().is_empty() {
            each(&line);
        }
    }
}

/// Reads one line of at most `cap` bytes, lossily decoded and without its
/// line ending; the rest of a longer line is skipped and the line marked.
/// `None` at the end of input or on a read error.
fn read_line(input: &mut impl BufRead, cap: usize) -> Option<String> {
    let mut line = Vec::new();
    let read = input
        .by_ref()
        .take(cap as u64)
        .read_until(b'\n', &mut line)
        .ok()?;
    if read == 0 {
        return None;
    }
    // A full buffer without its newline: skip the rest of the line. A
    // skip of the newline alone means the line just fitted.
    let cut = line.last() != Some(&b'\n')
        && line.len() == cap
        && input.skip_until(b'\n').is_ok_and(|skipped| skipped > 1);
    let mut text = String::from_utf8_lossy(&line)
        .trim_end_matches(['\n', '\r'])
        .to_owned();
    if cut {
        text.push_str(CUT);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines `drain` reports for `bytes`.
    fn lines(bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        drain(bytes, |l| out.push(l.to_owned()));
        out
    }

    #[test]
    fn lines_are_split_trimmed_and_blank_ones_skipped() {
        assert_eq!(
            lines(b"first error\r\n\n  \nsecond\nlast without newline"),
            ["first error", "second", "last without newline"]
        );
        assert!(lines(b"").is_empty());
    }

    #[test]
    fn long_lines_are_cut_and_marked() {
        let mut input = &b"abcdefgh\nabc\nij"[..];
        assert_eq!(read_line(&mut input, 3).as_deref(), Some("abc [...]"));
        // A line that just fits is not marked.
        assert_eq!(read_line(&mut input, 3).as_deref(), Some("abc"));
        assert_eq!(read_line(&mut input, 3).as_deref(), Some("ij"));
        assert_eq!(read_line(&mut input, 3), None);
        let long = vec![b'x'; 3 * MAX_LINE];
        let got = lines(&long);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), MAX_LINE + CUT.len());
    }

    #[test]
    fn invalid_utf8_is_replaced_and_read_errors_end_the_drain() {
        assert_eq!(lines(b"caf\xe9\n"), ["caf\u{fffd}"]);
        /// A reader that always fails.
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
        }
        let mut n = 0;
        drain(BufReader::new(Broken), |_| n += 1);
        assert_eq!(n, 0);
    }

    #[test]
    fn the_input_is_the_last_dash_i_argument() {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-nostdin", "-i", "/media/a.mp4", "-f", "rawvideo", "pipe:1"]);
        assert_eq!(input_of(&cmd), "/media/a.mp4");
        cmd.args(["-i", "/media/b.mp4"]);
        assert_eq!(input_of(&cmd), "/media/b.mp4");
        assert_eq!(input_of(&Command::new("sleep")), "sleep");
        let mut dangling = Command::new("ffmpeg");
        dangling.arg("-i");
        assert_eq!(input_of(&dangling), "ffmpeg");
    }

    #[cfg(unix)]
    #[test]
    fn a_childs_stderr_is_drained_until_it_exits() {
        use std::process::Stdio;
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo oops >&2; echo again >&2"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        forward(&mut child, &cmd).unwrap();
        assert!(child.stderr.is_none(), "the drain owns the pipe");
        assert!(child.wait().unwrap().success());
        // Without a stderr pipe there is nothing to forward.
        let mut quiet = Command::new("true");
        let mut child = quiet.stderr(Stdio::null()).spawn().unwrap();
        forward(&mut child, &quiet).unwrap();
        child.wait().unwrap();
    }
}
