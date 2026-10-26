//! One error type for the whole crate. Messages are written for the person in
//! front of the PC (they end up in the app's status line), not for a log parser.

use std::fmt;

/// A user-readable failure, e.g. "Virtual Display Driver not found".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayError(String);

impl DisplayError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DisplayError {}

pub(crate) type Result<T, E = DisplayError> = std::result::Result<T, E>;
