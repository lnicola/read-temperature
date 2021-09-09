use std::error;
use std::fmt::{self, Display, Formatter};
use std::io;

use bb8::RunError;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Serial(tokio_serial::Error),
    Postgres(tokio_postgres::Error),
    Timeout,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "Input/Output error: {}.", e),
            Error::Serial(e) => write!(f, "Serial port error: {}.", e),
            Error::Postgres(e) => write!(f, "Database error: {}.", e),
            Error::Timeout => write!(f, "Timeout."),
        }
    }
}

impl error::Error for Error {}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<tokio_serial::Error> for Error {
    fn from(err: tokio_serial::Error) -> Self {
        Error::Serial(err)
    }
}

impl From<tokio_postgres::Error> for Error {
    fn from(err: tokio_postgres::Error) -> Self {
        Error::Postgres(err)
    }
}

impl From<RunError<tokio_postgres::Error>> for Error {
    fn from(err: RunError<tokio_postgres::Error>) -> Self {
        match err {
            RunError::User(err) => Error::Postgres(err),
            RunError::TimedOut => Error::Timeout,
        }
    }
}
