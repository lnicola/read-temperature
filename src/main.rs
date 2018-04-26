#![feature(alloc_system, global_allocator, allocator_api)]

extern crate alloc_system;
extern crate bytes;
#[macro_use]
extern crate failure;
extern crate futures;
extern crate http;
extern crate hyper;
extern crate tokio;
extern crate tokio_io;
extern crate tokio_serial;
extern crate tokio_service;
extern crate tokio_timer;

use alloc_system::System;
use bytes::{BufMut, BytesMut};
use futures::{future, Future, Sink, Stream};
use hyper::client::Connect;
use hyper::{Body, Client, Request, Response, Uri};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, io, str};
use tokio_io::codec::{Decoder, Encoder};
use tokio_io::AsyncRead;
use tokio_serial::{Serial, SerialPortSettings};
use tokio_service::Service;
use tokio_timer::{Deadline, Interval};

#[global_allocator]
static A: System = System;

enum SensorCommand {
    Measure,
}

struct SensorReading {
    temperature: f32,
    humidity: f32,
}

struct SensorCodec;

impl Decoder for SensorCodec {
    type Item = SensorReading;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let newline = src.as_ref().iter().position(|b| *b == b'\n');
        if let Some(n) = newline {
            let line = src.split_to(n + 1);
            let r = str::from_utf8(line.as_ref()).ok().and_then(|s| {
                use std::str::FromStr;
                let mut it = s.split_whitespace();
                let humidity = it.next().and_then(|s| f32::from_str(s).ok());
                let temperature = it.next().and_then(|s| f32::from_str(s).ok());
                match (humidity, temperature) {
                    (Some(humidity), Some(temperature)) => Some(SensorReading {
                        temperature,
                        humidity,
                    }),
                    (_, _) => None,
                }
            });
            return match r {
                Some(r) => Ok(Some(r)),
                _ => Err(io::Error::new(io::ErrorKind::Other, "Invalid string")),
            };
        }
        Ok(None)
    }
}

impl Encoder for SensorCodec {
    type Item = SensorCommand;
    type Error = std::io::Error;

    fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            SensorCommand::Measure => {
                dst.reserve(1);
                dst.put(b'M')
            }
        }
        Ok(())
    }
}

struct Sensor {
    path: PathBuf,
    serial_settings: SerialPortSettings,
}

#[derive(Debug, Fail)]
enum Error {
    #[fail(display = "an IO error has occurred: {}", _0)]
    Io(#[cause] io::Error),
    #[fail(display = "an HTTP error has occurred: {}", _0)]
    Hyper(#[cause] hyper::Error),
    #[fail(display = "an timer error has occurred: {}", _0)]
    Timer(#[cause] tokio_timer::Error),
    #[fail(display = "an operation has timed out")]
    Timeout,
}

impl Service for Sensor {
    type Request = SensorCommand;
    type Response = SensorReading;
    type Error = Error;

    type Future = Box<Future<Item = Self::Response, Error = Self::Error> + Send>;

    fn call(&self, req: Self::Request) -> Self::Future {
        let serial = Serial::from_path(&self.path, &self.serial_settings);

        Box::new(
            future::result(serial)
                .map(|port| port.framed(SensorCodec))
                .and_then(|transport| transport.send(req))
                .and_then(|transport| transport.into_future().map_err(|(e, _)| e))
                .and_then(|(reading, _)| match reading {
                    Some(r) => Ok(r),
                    _ => Err(io::Error::new(io::ErrorKind::Other, "Read failed")),
                })
                .map_err(Error::Io),
        )
    }
}

struct InfluxData {
    temperature: f32,
    humidity: f32,
}

struct Influx<C: Connect> {
    url: Uri,
    client: Client<C>,
}

impl<C: Connect + 'static> Service for Influx<C> {
    type Request = InfluxData;
    type Response = Response<Body>;
    type Error = Error;

    type Future = Box<Future<Item = Self::Response, Error = Self::Error> + Send>;

    fn call(&self, req: Self::Request) -> Self::Future {
        let msg = format!(
            "temperature,host=ubik value={}\nhumidity,host=ubik value={}\n",
            req.temperature, req.humidity
        );
        let request = Request::post(&self.url)
            .header(
                "Content-Length",
                http::header::HeaderValue::from_str(&msg.len().to_string()).unwrap(),
            )
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(Body::from(msg))
            .unwrap();

        Box::new(self.client.request(request).map_err(Error::Hyper))
    }
}

fn main() {
    let mut args = env::args();
    let tty_path = args.nth(1).unwrap_or_else(|| String::from("/dev/ttyACM0"));
    let influx_url =
        Uri::from_str("http://127.0.0.1:8086/write?db=temperature&precision=s").unwrap();

    let sensor = Sensor {
        path: PathBuf::from(&tty_path),
        serial_settings: SerialPortSettings::default(),
    };

    let influx = Arc::new(Influx {
        url: influx_url,
        client: Client::new(),
    });

    let wakeups = Interval::new(Instant::now(), Duration::from_secs(10));
    let reads = wakeups
        .for_each(move |_| {
            let influx = Arc::clone(&influx);
            let reading = sensor.call(SensorCommand::Measure).and_then(move |r| {
                let data = InfluxData {
                    temperature: r.temperature,
                    humidity: r.humidity,
                };
                influx.call(data).map(|r| {
                    if !r.status().is_success() {
                        eprintln!("{:?}", r);
                    }
                })
            });
            let reading = Deadline::new(reading, Instant::now() + Duration::from_secs(6))
                .map_err(|e| {
                    if e.is_timer() {
                        Error::Timer(e.into_timer().unwrap())
                    } else if e.is_inner() {
                        e.into_inner().unwrap()
                    } else if e.is_elapsed() {
                        Error::Timeout
                    } else {
                        unreachable!()
                    }
                })
                .map_err(|e| eprintln!("{}", e));

            tokio::spawn(reading);
            Ok(())
        })
        .map_err(|e| eprintln!("{}", e));

    tokio::run(reads);
}
