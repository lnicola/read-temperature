extern crate bytes;
extern crate futures;
extern crate http;
extern crate hyper;
extern crate tokio;
extern crate tokio_codec;
extern crate tokio_serial;
extern crate tokio_timer;

use bytes::{BufMut, BytesMut};
use error::Error;
use futures::{future, Future, Sink, Stream};
use http::header::CONTENT_TYPE;
use hyper::client::connect::Connect;
use hyper::client::{Builder, HttpConnector};
use hyper::{Body, Client, Request, Response, Uri};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, io, str};
use tokio::codec::{Decoder, Encoder};
use tokio::runtime::current_thread;
use tokio::timer::{Interval, Timeout};
use tokio_serial::{Serial, SerialPortSettings};

mod error;

enum SensorCommand {
    Measure,
}

struct SensorReading {
    temperature: f32,
    humidity: f32,
}

struct SensorCodec;

fn parse_response(s: &str) -> Option<SensorReading> {
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
}

impl Decoder for SensorCodec {
    type Item = SensorReading;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let newline = src.as_ref().iter().position(|b| *b == b'\n');
        if let Some(n) = newline {
            let line = src.split_to(n + 1);
            let r = str::from_utf8(line.as_ref()).ok().and_then(parse_response);
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
    type Error = io::Error;

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

impl Sensor {
    fn call(&self, req: SensorCommand) -> impl Future<Item = SensorReading, Error = Error> {
        let serial = Serial::from_path(&self.path, &self.serial_settings)
            .map(|port| SensorCodec.framed(port));

        future::result(serial)
            .and_then(|transport| transport.send(req))
            .and_then(|transport| transport.into_future().map_err(|(e, _)| e))
            .and_then(|(reading, _)| match reading {
                Some(r) => Ok(r),
                _ => Err(io::Error::new(io::ErrorKind::Other, "Read failed")),
            })
            .map_err(|e| e.into())
    }
}

struct Influx<C: Connect> {
    url: Uri,
    client: Client<C>,
}

impl<C: Connect + 'static> Influx<C> {
    fn call(&self, message: String) -> impl Future<Item = Response<Body>, Error = Error> {
        let request = Request::post(&self.url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(message))
            .unwrap();

        self.client.request(request).map_err(|e| e.into())
    }
}

fn co2_thread<C: Connect + 'static>(influx: Arc<Influx<C>>) {
    let sensor = co2mon::Sensor::open_default().unwrap();
    let reads = Interval::new(Instant::now(), Duration::from_secs(10))
        .for_each(move |_| {
            match sensor.read() {
                Ok(reading) => {
                    let msg = format!(
                        "temperature2,host=ubik value={}\nco2,host=ubik value={}\n",
                        reading.temperature(),
                        reading.co2()
                    );
                    let f = influx
                        .call(msg)
                        .map(|r| {
                            if !r.status().is_success() {
                                eprintln!("{:?}", r);
                            }
                        })
                        .map_err(|e| eprintln!("{}", e));
                    tokio::spawn(f);
                }
                Err(e) => eprintln!("{}", e),
            }
            Ok(())
        })
        .map_err(|e| eprintln!("{}", e));
    current_thread::block_on_all(reads).unwrap();
}

fn main() {
    let mut args = env::args();
    let arg = args.nth(1);
    let tty_path = arg.as_ref().map(String::as_str).unwrap_or("/dev/ttyACM0");
    let url = Uri::from_str("http://127.0.0.1:8086/write?db=temperature&precision=s").unwrap();

    let temperature_sensor = Sensor {
        path: PathBuf::from(tty_path),
        serial_settings: SerialPortSettings::default(),
    };
    let connector = HttpConnector::new_with_tokio_threadpool_resolver();
    let client = Builder::default().build(connector);
    let influx = Arc::new(Influx { url, client });

    let influx_ = influx.clone();
    std::thread::spawn(move || co2_thread(influx_));

    let reads = Interval::new(Instant::now(), Duration::from_secs(10))
        .for_each(move |_| {
            let influx = Arc::clone(&influx);
            let reading = temperature_sensor
                .call(SensorCommand::Measure)
                .and_then(move |r| {
                    let msg = format!(
                        "temperature,host=ubik value={}\nhumidity,host=ubik value={}\n",
                        r.temperature, r.humidity
                    );
                    influx.call(msg).map(|r| {
                        if !r.status().is_success() {
                            eprintln!("{:?}", r);
                        }
                    })
                });
            let reading = Timeout::new(reading, Duration::from_secs(6)).map_err(|e| {
                eprintln!("{}", e);
            });

            tokio::spawn(reading);

            Ok(())
        })
        .map_err(|e| eprintln!("{}", e));

    current_thread::block_on_all(reads).unwrap();
}
