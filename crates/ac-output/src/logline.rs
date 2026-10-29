//! ffmpeg's log lines as `-loglevel repeat+level+info` prints them.
//!
//! With the `level` flag every line that goes through ffmpeg's logger
//! carries its severity, and most also name the class of the object that
//! logged it:
//!
//! ```text
//! [fifo @ 0x560bd97523c0] [info] Recovery failed: Connection refused
//! [tcp @ 0x7f21e8001340] [error] Connection to tcp://127.0.0.1:36213?tcp_nodelay=0 failed: Connection refused
//! [info] Stream mapping:
//! ```
//!
//! [`LogLine::parse`] splits a line into those parts: the `[name @ address]`
//! contexts (an object with a parent prints the parent's first; the last
//! one is the object that logged), then the `[level]` tag, then the
//! message. Nothing after the level tag is a tag, so a message that starts
//! with a bracket stays whole. The parse is total: a line without the tags,
//! such as one a stand-in ffmpeg prints or anything written around the
//! logger, is all message with no level, and [`LogLine::is_error`] counts
//! it as an error, as every stderr line was while ffmpeg logged errors
//! only.
//!
//! The `repeat` flag matters as much as `level`: without it ffmpeg folds a
//! line equal to the one before into "Last message repeated n times", and
//! the stream monitor counts every reconnection attempt by its line.

/// A message's severity, from ffmpeg's `[level]` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Something went wrong and the process is about to crash.
    Panic,
    /// Something went wrong and ffmpeg cannot go on.
    Fatal,
    /// Something went wrong that cannot be recovered from by itself.
    Error,
    /// Something looks wrong and may lead to problems.
    Warning,
    /// Standard information.
    Info,
    /// Detailed information.
    Verbose,
    /// Debugging output.
    Debug,
    /// Extremely verbose debugging output.
    Trace,
}

impl Level {
    /// The level named by a tag's text (`error`, `info`, ...).
    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "panic" => Level::Panic,
            "fatal" => Level::Fatal,
            "error" => Level::Error,
            "warning" => Level::Warning,
            "info" => Level::Info,
            "verbose" => Level::Verbose,
            "debug" => Level::Debug,
            "trace" => Level::Trace,
            _ => return None,
        })
    }
}

/// One stderr line, split into its parts; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogLine<'a> {
    /// Class of the object that logged the line (`fifo`, `tcp`,
    /// `out#0/tee`), when the line names one.
    pub context: Option<&'a str>,
    /// Severity, when the line is tagged.
    pub level: Option<Level>,
    /// The text after the tags, without surrounding whitespace.
    pub message: &'a str,
}

impl<'a> LogLine<'a> {
    /// Splits `line` into its parts. Never fails; see the module docs.
    pub fn parse(line: &'a str) -> Self {
        let mut rest = line.trim();
        let mut context = None;
        let mut level = None;
        while let Some(inner) = rest.strip_prefix('[')
            && let Some(end) = inner.find(']')
        {
            let tag = &inner[..end];
            let after = inner[end + 1..].trim_start();
            if let Some(l) = Level::from_tag(tag) {
                level = Some(l);
                rest = after;
                break;
            }
            let Some((name, _address)) = tag.split_once(" @ ") else {
                break;
            };
            context = Some(name);
            rest = after;
        }
        LogLine {
            context,
            level,
            message: rest,
        }
    }

    /// True for an error, a fatal error or a panic, and for an untagged
    /// line (see the module docs).
    pub fn is_error(&self) -> bool {
        self.level.is_none_or(|l| l <= Level::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_context_level_and_message() {
        let line =
            LogLine::parse("[fifo @ 0x560bd97523c0] [info] Recovery failed: Connection refused\n");
        assert_eq!(line.context, Some("fifo"));
        assert_eq!(line.level, Some(Level::Info));
        assert_eq!(line.message, "Recovery failed: Connection refused");
        assert!(!line.is_error());

        let line = LogLine::parse("[out#0/tee @ 0x55a4a2406540] [error] All tee outputs failed.");
        assert_eq!(line.context, Some("out#0/tee"));
        assert!(line.is_error());
    }

    #[test]
    fn a_parent_context_comes_first_and_the_last_one_logged() {
        let line = LogLine::parse("[rtmp @ 0x1] [tls @ 0x2] [error] handshake failed");
        assert_eq!(line.context, Some("tls"));
        assert_eq!(line.message, "handshake failed");
    }

    #[test]
    fn lines_without_a_context_or_level() {
        let line = LogLine::parse("[info]   Stream #0:0 -> #0:0 (rawvideo (native) -> h264)");
        assert_eq!(line.context, None);
        assert_eq!(line.level, Some(Level::Info));
        assert_eq!(
            line.message,
            "Stream #0:0 -> #0:0 (rawvideo (native) -> h264)"
        );

        let line = LogLine::parse("fake ffmpeg failed on purpose");
        assert_eq!((line.context, line.level), (None, None));
        assert_eq!(line.message, "fake ffmpeg failed on purpose");
        assert!(line.is_error(), "untagged lines are errors");
    }

    #[test]
    fn brackets_in_the_message_are_not_tags() {
        // After the level tag everything is message.
        let line = LogLine::parse("[info] [f=mp4]file:/rec/a.mp4");
        assert_eq!(line.message, "[f=mp4]file:/rec/a.mp4");
        // A bracket that is neither a level nor a context ends the tags.
        let line = LogLine::parse("[f=flv]rtmp://host/app: refused");
        assert_eq!((line.context, line.level), (None, None));
        assert_eq!(line.message, "[f=flv]rtmp://host/app: refused");
        // An unclosed bracket is message too.
        let line = LogLine::parse("[tcp @ 0x1");
        assert_eq!(line.message, "[tcp @ 0x1");
    }

    #[test]
    fn severities_order_from_panic_to_trace() {
        for (tag, error) in [
            ("panic", true),
            ("fatal", true),
            ("error", true),
            ("warning", false),
            ("info", false),
            ("verbose", false),
            ("debug", false),
            ("trace", false),
        ] {
            let line = format!("[{tag}] x");
            let parsed = LogLine::parse(&line);
            assert!(parsed.level.is_some(), "{tag}");
            assert_eq!(parsed.is_error(), error, "{tag}");
        }
        assert_eq!(LogLine::parse("[notice] x").level, None);
        assert_eq!(LogLine::parse("").message, "");
    }
}
