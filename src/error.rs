use hyper;
use std::error;
use std::fmt::{self, Display, Formatter};
use std::io;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Hyper(hyper::Error),
    Serial(tokio_serial::Error),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "Input/Output error: {}.", e),
            Error::Hyper(e) => write!(f, "HTTP error: {}.", e),
            Error::Serial(e) => write!(f, "Serial port error: {}.", e),
        }
    }
}

impl error::Error for Error {}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<hyper::Error> for Error {
    fn from(err: hyper::Error) -> Self {
        Error::Hyper(err)
    }
}

impl From<tokio_serial::Error> for Error {
    fn from(err: tokio_serial::Error) -> Self {
        Error::Serial(err)
    }
}
