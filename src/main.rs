use error::Error;
use reqwest::Client;
use std::io::{self, ErrorKind};
use std::str::FromStr;
use std::time::Duration;
use std::{env, str};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::runtime::Builder;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::Instant;
use tokio_serial::SerialPortBuilderExt;

mod error;

struct SensorReading {
    temperature: f32,
    humidity: f32,
}

fn parse_response(s: &str) -> Option<SensorReading> {
    let mut it = s.split_whitespace();
    let humidity = f32::from_str(it.next()?).ok()?;
    let temperature = f32::from_str(it.next()?).ok()?;
    Some(SensorReading {
        temperature,
        humidity,
    })
}

#[derive(Clone)]
struct Sensor {
    path: String,
    baud_rate: u32,
}

impl Sensor {
    async fn read(&self) -> Result<SensorReading, Error> {
        let mut serial = tokio_serial::new(&self.path, self.baud_rate).open_native_async()?;
        serial.write_u8(b'M').await?;
        let mut reader = BufReader::new(serial);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        if let Some(reading) = parse_response(&line) {
            Ok(reading)
        } else {
            Err(io::Error::new(ErrorKind::Other, "Read failed").into())
        }
    }
}

async fn co2_thread(
    sensor: co2mon::Sensor,
    tx: UnboundedSender<Reading>,
) -> Result<(), Box<dyn std::error::Error + Send>> {
    let mut interval = tokio::time::interval_at(Instant::now(), Duration::from_secs(10));
    loop {
        interval.tick().await;
        match sensor.read() {
            Ok(reading) => {
                let reading = Reading::Co2Meter {
                    time: OffsetDateTime::now_utc(),
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
        time: OffsetDateTime,
        temperature: f32,
        humidity: f32,
    },
    Co2Meter {
        time: OffsetDateTime,
        temperature: f32,
        co2: u16,
    },
}

async fn save_reading(
    client: &Client,
    api_config: &ApiConfig,
    reading: Reading,
) -> Result<(), Error> {
    match reading {
        Reading::Thermometer {
            time,
            temperature,
            humidity,
        } => {
            client
                .post(format!(
                    "{}stats?time={}&temperature={}&humidity={}",
                    api_config.api_url,
                    time.unix_timestamp(),
                    temperature,
                    humidity
                ))
                .bearer_auth(&api_config.access_token)
                .send()
                .await?
                .error_for_status()?;
        }
        Reading::Co2Meter {
            time,
            temperature,
            co2,
        } => {
            client
                .post(format!(
                    "{}stats2?time={}&temperature={}&co2={}",
                    api_config.api_url,
                    time.unix_timestamp(),
                    temperature,
                    co2
                ))
                .bearer_auth(&api_config.access_token)
                .send()
                .await?
                .error_for_status()?;
        }
    }
    Ok(())
}

async fn db_thread(api_config: ApiConfig, mut rx: UnboundedReceiver<Reading>) {
    let client = Client::new();
    while let Some(reading) = rx.recv().await {
        if let Err(e) = save_reading(&client, &api_config, reading).await {
            eprintln!("{}", e);
        }
    }
}

async fn run(api_config: ApiConfig, tty_path: String) {
    let temperature_sensor = Sensor {
        path: tty_path,
        baud_rate: 9600,
    };

    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        db_thread(api_config, rx).await;
    });

    match co2mon::Sensor::open_default() {
        Ok(sensor) => {
            tokio::spawn(co2_thread(sensor, tx.clone()));
        }
        Err(e) => {
            eprintln!("{}", e);
        }
    };

    let mut interval = tokio::time::interval_at(Instant::now(), Duration::from_secs(10));
    loop {
        interval.tick().await;
        let temperature_sensor = temperature_sensor.clone();
        let tx = tx.clone();
        let one = async move {
            let reading = temperature_sensor.read().await?;
            let reading = Reading::Thermometer {
                time: OffsetDateTime::now_utc(),
                temperature: reading.temperature,
                humidity: reading.humidity,
            };
            tx.send(reading).unwrap();
            Ok::<(), Error>(())
        };
        if let Err(e) = tokio::time::timeout(Duration::from_secs(6), one).await {
            eprintln!("{}", e);
        }
    }
}

struct ApiConfig {
    api_url: String,
    access_token: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args();
    let arg = args.nth(1);
    let tty_path = arg.as_deref().unwrap_or("/dev/ttyACM0").to_string();
    let api_url = env::var("API_URL")?;
    let access_token = env::var("ACCESS_TOKEN")?;

    let api_config = ApiConfig {
        api_url,
        access_token,
    };

    let rt = Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    rt.block_on(async move { run(api_config, tty_path).await });
    Ok(())
}
