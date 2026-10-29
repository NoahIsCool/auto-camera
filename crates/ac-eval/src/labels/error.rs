//! Errors that name the field at fault.
//!
//! Both the shape check of reading
//! ([`LabelFile::from_json`](super::LabelFile::from_json)) and the rules
//! ([`LabelFile::validate`](super::LabelFile::validate)) report problems
//! as [`FieldError`]s: a path to the field in the JSON document and a
//! message. A path is the chain of field names from the root joined by
//! `.`, with array indexes in brackets (`keyframes[2].people[0].bbox.x`),
//! and `.` alone is the whole document. That is the notation of
//! `serde_path_to_error`, which the shape check uses, so a path names the
//! same field whichever step found the problem. [`FieldErrors`] holds every
//! error found, in document order.

use std::fmt;

use thiserror::Error;

/// One broken rule, at the field it concerns.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{path}: {message}")]
pub struct FieldError {
    /// Where, such as `keyframes[2].people[0].bbox.x`; `.` for the whole
    /// document.
    pub path: String,
    /// What is wrong, in words.
    pub message: String,
}

impl FieldError {
    /// An error at `path`.
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Every broken rule found in a label file, in document order; never
/// empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldErrors(pub Vec<FieldError>);

impl FieldErrors {
    /// The first error at exactly `path`.
    pub fn at(&self, path: &str) -> Option<&FieldError> {
        self.0.iter().find(|e| e.path == path)
    }

    /// The path of every error, in order.
    pub fn paths(&self) -> Vec<&str> {
        self.0.iter().map(|e| e.path.as_str()).collect()
    }
}

impl From<FieldError> for FieldErrors {
    /// A list of one.
    fn from(error: FieldError) -> Self {
        Self(vec![error])
    }
}

impl fmt::Display for FieldErrors {
    /// One error per line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, error) in self.0.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for FieldErrors {}
