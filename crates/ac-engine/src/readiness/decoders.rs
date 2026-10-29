//! Who runs the decoders' ffmpeg tools (`AC_FFMPEG`, `AC_FFPROBE`), so the
//! readiness check runs each tool only when something uses it and names
//! those users in its finding.
//!
//! File sources and audio inputs decode with both tools (ffprobe reads the
//! stream, ffmpeg decodes it); capture sources list and capture their
//! cameras with ffmpeg alone. A label names only the kinds the studio has,
//! so a cameras-plus-board studio reads "capture sources' and audio
//! inputs' ffmpeg" and "audio inputs' ffprobe", never a file source it
//! does not have.

/// Which kinds of input the studio has that run the decoders' tools; see
/// the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct DecoderUsers {
    /// There is a file source (ffmpeg and ffprobe).
    pub(super) file_sources: bool,
    /// There is a capture source (ffmpeg only).
    pub(super) capture_sources: bool,
    /// There is an audio input read from a file (ffmpeg and ffprobe).
    pub(super) audio_inputs: bool,
}

impl DecoderUsers {
    /// Who runs the decoders' ffmpeg, as a possessive label ("capture
    /// sources' and audio inputs'"), or `None` when nothing does.
    pub(super) fn ffmpeg(self) -> Option<String> {
        let sources = match (self.file_sources, self.capture_sources) {
            (true, true) => Some("file and capture sources'"),
            (true, false) => Some("file sources'"),
            (false, true) => Some("capture sources'"),
            (false, false) => None,
        };
        with_audio_inputs(sources, self.audio_inputs)
    }

    /// Who runs the decoders' ffprobe, labelled as by
    /// [`DecoderUsers::ffmpeg`], or `None` when nothing does. Its users are
    /// always some of ffmpeg's.
    pub(super) fn ffprobe(self) -> Option<String> {
        with_audio_inputs(
            self.file_sources.then_some("file sources'"),
            self.audio_inputs,
        )
    }
}

/// `sources`, then "audio inputs'" when `audio_inputs`, joined by "and";
/// `None` when neither is there.
fn with_audio_inputs(sources: Option<&str>, audio_inputs: bool) -> Option<String> {
    let parts: Vec<&str> = sources
        .into_iter()
        .chain(audio_inputs.then_some("audio inputs'"))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" and "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The users with file sources `f`, capture sources `c` and audio
    /// inputs `a`.
    fn users(f: bool, c: bool, a: bool) -> DecoderUsers {
        DecoderUsers {
            file_sources: f,
            capture_sources: c,
            audio_inputs: a,
        }
    }

    #[test]
    fn labels_name_only_the_kinds_present() {
        let cases = [
            (users(false, false, false), None, None),
            (
                users(true, false, false),
                Some("file sources'"),
                Some("file sources'"),
            ),
            (users(false, true, false), Some("capture sources'"), None),
            (
                users(true, true, false),
                Some("file and capture sources'"),
                Some("file sources'"),
            ),
            (
                users(false, false, true),
                Some("audio inputs'"),
                Some("audio inputs'"),
            ),
            (
                users(false, true, true),
                Some("capture sources' and audio inputs'"),
                Some("audio inputs'"),
            ),
            (
                users(true, false, true),
                Some("file sources' and audio inputs'"),
                Some("file sources' and audio inputs'"),
            ),
            (
                users(true, true, true),
                Some("file and capture sources' and audio inputs'"),
                Some("file sources' and audio inputs'"),
            ),
        ];
        for (u, ffmpeg, ffprobe) in cases {
            assert_eq!(u.ffmpeg().as_deref(), ffmpeg, "{u:?}");
            assert_eq!(u.ffprobe().as_deref(), ffprobe, "{u:?}");
        }
    }
}
