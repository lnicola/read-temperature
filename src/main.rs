use bb8::Pool;
use bb8_postgres::PostgresConnectionManager;
use bytes::{BufMut, BytesMut};
use chrono::{DateTime, SubsecRound, Utc};
use error::Error;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use std::io;
use std::str::FromStr;
use std::time::Duration;
use std::{env, str};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{self, Instant};
use tokio_postgres::{Config, NoTls};
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

async fn co2_thread(
    sensor: co2mon::Sensor,
    tx: UnboundedSender<Reading>,
) -> Result<(), Box<dyn std::error::Error + Send>> {
    let mut interval = time::interval_at(Instant::now(), Duration::from_secs(10));
    loop {
        interval.tick().await;
        match sensor.read() {
            Ok(reading) => {
                let time = Utc::now().round_subsecs(0);
                let reading = Reading::Co2Meter {
                    time,
                    temperature: reading.temperature(),
                    co2: reading.co2(),
                };
                tx.send(reading).unwrap();
            }
            Err(e) => eprintln!("{}", e),
        }
    }
}

#[derive(Debug)]
enum Reading {
    Thermometer {
        time: DateTime<Utc>,
        temperature: f32,
        humidity: f32,
    },
    Co2Meter {
        time: DateTime<Utc>,
        temperature: f32,
        co2: u16,
    },
}

async fn send_one(
    pool: &Pool<PostgresConnectionManager<NoTls>>,
    reading: Reading,
) -> Result<(), Error> {
    let conn = pool.get().await?;
    match reading {
        Reading::Thermometer {
            time,
            temperature,
            humidity,
        } => {
            let temperature = (temperature * 100.0).round() as i16;
            let humidity = (humidity * 100.0).round() as i16;
            conn.query(
                "insert into stats (time, temperature, humidity) values ($1, $2, $3);",
                &[&time.naive_utc(), &temperature, &humidity],
            )
            .await?;
        }
        Reading::Co2Meter {
            time,
            temperature,
            co2,
        } => {
            let temperature = (temperature * 100.0).round() as i16;
            let co2 = co2 as i16;
            conn.query(
                "insert into stats2 (time, temperature, co2) values ($1, $2, $3);",
                &[&time.naive_utc(), &temperature, &co2],
            )
            .await?;
        }
    }
    Ok(())
}

async fn db_thread(
    mut rx: UnboundedReceiver<Reading>,
    pool: Pool<PostgresConnectionManager<NoTls>>,
) {
    while let Some(reading) = rx.recv().await {
        if let Err(e) = send_one(&pool, reading).await {
            eprintln!("{}", e);
        }
    }
}

async fn run(tty_path: String) {
    let temperature_sensor = Sensor {
        path: tty_path,
        baud_rate: 9600,
    };
    let mut config = Config::new();

    let host = env::var("DB_HOST").unwrap();
    let user = env::var("DB_USER").unwrap();
    let pass = env::var("DB_PASS").unwrap();
    let name = env::var("DB_NAME").unwrap();
    config.host(&host).user(&user).password(&pass).dbname(&name);
    let (tx, rx) = mpsc::unbounded_channel();
    let tx_ = tx.clone();
    let manager = PostgresConnectionManager::new(config, NoTls);
    let pool = Pool::builder().max_size(1).build(manager).await.unwrap();

    tokio::spawn(async move {
        db_thread(rx, pool).await;
    });

    match co2mon::Sensor::open_default() {
        Ok(sensor) => {
            tokio::spawn(co2_thread(sensor, tx_));
        }
        Err(e) => {
            eprintln!("{}", e);
        }
    };

    let mut interval = time::interval_at(Instant::now(), Duration::from_secs(10));
    loop {
        interval.tick().await;
        let temperature_sensor = temperature_sensor.clone();
        let tx = tx.clone();
        let one = async move {
            let reading = temperature_sensor.call(SensorCommand::Measure).await?;
            let time = Utc::now().round_subsecs(0);
            let reading = Reading::Thermometer {
                time,
                temperature: reading.temperature,
                humidity: reading.humidity,
            };
            tx.send(reading).unwrap();
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
    let tty_path = arg.as_deref().unwrap_or("/dev/ttyACM0").to_string();

    let rt = Runtime::new()?;
    rt.block_on(async move { run(tty_path).await });
    Ok(())
}
