use hyper;
use std::io;
use std::error;
use std::fmt::{self, Display, Formatter};

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Hyper(hyper::Error),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "Input/Output error: {}.", e),
            Error::Hyper(e) => write!(f, "HTTP error: {}.", e),
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
