use bytes::{BufMut, BytesMut};
use error::Error;
use futures::future::TryFutureExt;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use hyper;
use hyper::client::HttpConnector;
use hyper::header::CONTENT_TYPE;
use hyper::{Body, Client, Request, Response, Uri};
use std::io;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::{env, str};
use tokio::runtime::Runtime;
use tokio::time::{self, Instant};
use tokio_serial::SerialPortBuilderExt;
use tokio_util::codec::{Decoder, Encoder};

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

impl Encoder<SensorCommand> for SensorCodec {
    type Error = io::Error;

    fn encode(&mut self, item: SensorCommand, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            SensorCommand::Measure => dst.put_u8(b'M'),
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Sensor {
    path: String,
    baud_rate: u32,
}

impl Sensor {
    async fn call(&self, req: SensorCommand) -> Result<SensorReading, Error> {
        let mut serial = tokio_serial::new(&self.path, self.baud_rate)
            .open_native_async()
            .map(|port| SensorCodec.framed(port))?;
        serial.send(req).await?;
        if let (Some(reading), _) = serial.into_future().await {
            let reading = reading?;
            Ok(reading)
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "Read failed").into())
        }
    }
}

struct Influx {
    url: Uri,
    client: Client<HttpConnector>,
}

impl Influx {
    async fn call(&self, message: String) -> Result<Response<Body>, Error> {
        let request = Request::post(&self.url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(message))
            .unwrap();
        let response = self.client.request(request).await?;
        Ok(response)
    }
}

async fn co2_thread(
    influx: Arc<Influx>,
    sensor: co2mon::Sensor,
) -> Result<(), Box<dyn std::error::Error + Send>> {
    let mut interval = time::interval_at(Instant::now(), Duration::from_secs(10));
    loop {
        interval.tick().await;
        match sensor.read() {
            Ok(reading) => {
                let msg = format!(
                    "temperature2,host=ubik value={}\nco2,host=ubik value={}\n",
                    reading.temperature(),
                    reading.co2()
                );
                let influx = influx.clone();
                let f = async move {
                    let r = influx.call(msg).await?;
                    if !r.status().is_success() {
                        eprintln!("{:?}", r);
                    }
                    Ok::<(), Error>(())
                }
                .map_err(|e| eprintln!("{}", e));
                tokio::spawn(f);
            }
            Err(e) => eprintln!("{}", e),
        }
    }
}

async fn run(tty_path: String) {
    let url = Uri::from_str("http://127.0.0.1:8086/write?db=temperature&precision=s").unwrap();
    let temperature_sensor = Sensor {
        path: tty_path,
        baud_rate: 9600,
    };
    let client = Client::new();
    let influx = Influx { url, client };
    let influx = Arc::new(influx);

    let influx_ = influx.clone();
    match co2mon::Sensor::open_default() {
        Ok(sensor) => {
            tokio::spawn(co2_thread(influx_, sensor));
        }
        Err(e) => {
            eprintln!("{}", e);
        }
    };

    let mut interval = time::interval_at(Instant::now(), Duration::from_secs(10));
    loop {
        interval.tick().await;
        let temperature_sensor = temperature_sensor.clone();
        let influx = Arc::clone(&influx);
        let one = async move {
            let reading = temperature_sensor.call(SensorCommand::Measure).await?;
            let msg = format!(
                "temperature,host=ubik value={}\nhumidity,host=ubik value={}\n",
                reading.temperature, reading.humidity
            );
            let r = influx.call(msg).await?;
            if !r.status().is_success() {
                eprintln!("{:?}", r);
            }
            Ok::<(), Error>(())
        };
        if let Err(e) = time::timeout(Duration::from_secs(6), one).await {
            eprintln!("{}", e);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    let arg = args.nth(1);
    let tty_path = arg
        .as_ref()
        .map(String::as_str)
        .unwrap_or("/dev/ttyACM0")
        .to_string();

    let rt = Runtime::new()?;
    rt.block_on(async move { run(tty_path).await });
    Ok(())
}
