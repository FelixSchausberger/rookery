// The core never calls std::process::exit; the CLI owns the exit code. Every
// message that reaches the user is built here so the CLI and the TUI report
// the same text.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Refused(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }

    pub fn usage(text: impl Into<String>) -> Self {
        Self::Usage(text.into())
    }

    pub fn invalid(text: impl Into<String>) -> Self {
        Self::Invalid(text.into())
    }

    pub fn refused(text: impl Into<String>) -> Self {
        Self::Refused(text.into())
    }

    /// Usage errors and store refusals use exit 2, matching the shell.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::Invalid(_) | Self::Refused(_) => 2,
            Self::Message(_) => 1,
        }
    }
}
