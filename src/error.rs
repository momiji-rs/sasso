//! The compiler error type.

use std::fmt;

use crate::scanner::Pos;

/// A compilation error, carrying a human-readable message and a 1-based
/// source position (`line`/`col` are `0` when the position is unknown).
///
/// Implements [`std::error::Error`], so it composes with `?` and the
/// wider error ecosystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Human-readable description of what went wrong.
    pub message: String,
    /// 1-based line number, or `0` if unknown.
    pub line: usize,
    /// 1-based column number, or `0` if unknown.
    pub col: usize,
}

impl Error {
    pub(crate) fn at(message: impl Into<String>, pos: Pos) -> Self {
        Error {
            message: message.into(),
            line: pos.line,
            col: pos.col,
        }
    }

    pub(crate) fn unpositioned(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
            line: 0,
            col: 0,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(f, "Error: {} ({}:{})", self.message, self.line, self.col)
        } else {
            write!(f, "Error: {}", self.message)
        }
    }
}

impl std::error::Error for Error {}
