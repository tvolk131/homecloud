use std::fmt;

/// Shown when a failure has no authored explanation.
pub const GENERIC: &str = "Application setup failed. Please try again.";

/// Text that reaches the owner is always authored in this crate, never raw
/// provider output, which can contain identities, URLs or tokens.
#[derive(Debug)]
pub enum Error {
    Authored(String),
    /// Diagnostic detail for the guest journal only.
    Internal(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Authored(message.into()))
}

impl Error {
    pub fn user_message(&self) -> &str {
        match self {
            Self::Authored(message) => message,
            Self::Internal(_) => GENERIC,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authored(message) => f.write_str(message),
            Self::Internal(detail) => write!(f, "{GENERIC} ({detail})"),
        }
    }
}

impl std::error::Error for Error {}

macro_rules! internal {
    ($($source:ty),*) => {$(
        impl From<$source> for Error {
            fn from(error: $source) -> Self {
                Self::Internal(error.to_string())
            }
        }
    )*};
}
internal!(
    std::io::Error,
    serde_json::Error,
    nix::Error,
    uuid::Error,
    tempfile::PersistError
);
