use std::error;
use std::fmt::{self, Display, Formatter};
use std::io;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Serial(tokio_serial::Error),
    Reqwest(reqwest::Error),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "Input/Output error: {}.", e),
            Error::Serial(e) => write!(f, "Serial port error: {}.", e),
            Error::Reqwest(e) => write!(f, "HTTP error: {}.", e),
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

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Error::Reqwest(err)
    }
}
