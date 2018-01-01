#![feature(alloc_system)]

extern crate bytes;
extern crate futures;
extern crate tokio_core;
extern crate tokio_io;
extern crate tokio_service;
extern crate tokio_timer;
extern crate tokio_serial;
extern crate hyper;
extern crate alloc_system;

use bytes::{BufMut, BytesMut};
use std::{io, env, str};
use std::path::PathBuf;
use std::time::{self, Instant, Duration, SystemTime};
use std::sync::Arc;
use std::str::FromStr;
use futures::{Future, Stream, Sink};
use tokio_core::reactor::{Core, Handle};
use tokio_io::AsyncRead;
use tokio_io::codec::{Decoder, Encoder};
use tokio_service::Service;
use tokio_timer::Timer;
use tokio_serial::{Serial, SerialPortSettings};
use hyper::{Client, Method, Uri};
use hyper::client::Request;
use hyper::header::{Connection, ContentLength, ContentType};

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
            let r = str::from_utf8(line.as_ref())
                .ok()
                .and_then(|s| {
                    use std::str::FromStr;
                    let mut it = s.split_whitespace();
                    let humidity = it.next().and_then(|s| f32::from_str(s).ok());
                    let temperature = it.next().and_then(|s| f32::from_str(s).ok());
                    match (humidity, temperature) {
                        (Some(humidity), Some(temperature)) => {
                            Some(SensorReading {
                                temperature: temperature,
                                humidity: humidity,
                            })
                        }
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
    handle: Handle,
    path: PathBuf,
    serial_settings: SerialPortSettings,
}

impl Service for Sensor {
    type Request = SensorCommand;
    type Response = SensorReading;
    type Error = io::Error;

    type Future = Box<Future<Item = Self::Response, Error = Self::Error>>;

    fn call(&self, req: Self::Request) -> Self::Future {
        let port = Serial::from_path(&self.path, &self.serial_settings, &self.handle).unwrap();
        let transport = port.framed(SensorCodec);

        Box::new(transport.send(req)
            .and_then(|transport| transport.into_future().map_err(|(e, _)| e))
            .and_then(|(reading, _)| match reading {
                Some(r) => Ok(r),
                _ => Err(io::Error::new(io::ErrorKind::Other, "Read failed")),
            })
            )
    }
}

struct InfluxData {
    temperature: f32,
    humidity: f32,
    timestamp: SystemTime,
}

struct Influx {
    handle: Handle,
    url: Uri,
}

impl Service for Influx {
    type Request = InfluxData;
    type Response = hyper::Response;
    type Error = hyper::Error;

    type Future = Box<Future<Item = Self::Response, Error = Self::Error>>;

    fn call(&self, req: Self::Request) -> Self::Future {
        let timestamp = req.timestamp.duration_since(time::UNIX_EPOCH).unwrap().as_secs();
        let msg = format!("temperature,host=ubik value={} {}\nhumidity,host=ubik value={} {}\n",
                          req.temperature,
                          timestamp,
                          req.humidity,
                          timestamp);
        let client = Client::new(&self.handle);
        let mut request = Request::new(Method::Post, self.url.clone());
        {
            let headers = request.headers_mut();
            headers.set(ContentLength(msg.len() as u64));
            headers.set(Connection::close());
            headers.set(ContentType::form_url_encoded());
        }
        request.set_body(msg);

        Box::new(client.request(request))
    }
}

fn main() {
    let mut args = env::args();
    let tty_path = args.nth(1).unwrap_or_else(|| String::from("/dev/ttyACM0"));
    let influx_url = Uri::from_str("http://127.0.0.1:8086/write?db=temperature&precision=s").unwrap();
    println!("{}", influx_url);

    let mut core = Core::new().unwrap();
    let handle = core.handle();

    let sensor = Sensor {
        handle: handle.clone(),
        path: PathBuf::from(&tty_path),
        serial_settings: SerialPortSettings::default(),
    };

    let influx = Arc::new(Influx {
        handle: handle.clone(),
        url: influx_url,
    });

    let timer = Timer::default();
    let wakeups = timer.interval_at(Instant::now(), Duration::from_secs(10));
    let reads = wakeups.for_each(|_| {
        let influx = Arc::clone(&influx);
        let handle2 = handle.clone();
        let reading = sensor.call(SensorCommand::Measure)
            .and_then(move |r| {
                let data = InfluxData {
                    temperature: r.temperature,
                    humidity: r.humidity,
                    timestamp: SystemTime::now(),
                };
                let sender = influx.call(data).map(|_| ()).map_err(|_| ());
                handle2.spawn(sender);
                Ok(())
            })
            .map_err(|_| ());

        handle.spawn(reading);
        Ok(())
    });

    core.run(reads).unwrap();
}
