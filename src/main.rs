use bb8::Pool;
use bb8_postgres::PostgresConnectionManager;
use chrono::{DateTime, Local};
use error::Error;
use std::io::{self, ErrorKind};
use std::str::FromStr;
use std::time::Duration;
use std::{env, str};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::runtime::Builder;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{self, Instant};
use tokio_postgres::{Config, NoTls};
use tokio_serial::SerialPortBuilderExt;

mod error;

struct SensorReading {
    temperature: f32,
    humidity: f32,
}

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
    let mut interval = time::interval_at(Instant::now(), Duration::from_secs(10));
    loop {
        interval.tick().await;
        match sensor.read() {
            Ok(reading) => {
                let reading = Reading::Co2Meter {
                    time: Local::now(),
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
        time: DateTime<Local>,
        temperature: f32,
        humidity: f32,
    },
    Co2Meter {
        time: DateTime<Local>,
        temperature: f32,
        co2: u16,
    },
}

async fn save_reading(
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
            conn.query(
                "call insert_stats($1, $2, $3);",
                &[&time, &temperature, &humidity],
            )
            .await?;
        }
        Reading::Co2Meter {
            time,
            temperature,
            co2,
        } => {
            let co2 = co2 as i16;
            conn.query(
                "call insert_stats2($1, $2, $3);",
                &[&time, &temperature, &co2],
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
        if let Err(e) = save_reading(&pool, reading).await {
            eprintln!("{}", e);
        }
    }
}

struct DbConfig {
    host: String,
    user: String,
    password: String,
    dbname: String,
}

impl DbConfig {
    pub fn from_env() -> Self {
        Self {
            host: env::var("DB_HOST").unwrap_or_default(),
            user: env::var("DB_USER").unwrap_or_default(),
            password: env::var("DB_PASS").unwrap_or_default(),
            dbname: env::var("DB_NAME").unwrap_or_default(),
        }
    }
}

impl Into<Config> for DbConfig {
    fn into(self) -> Config {
        let mut config = Config::new();
        config
            .host(&self.host)
            .user(&self.user)
            .password(&self.password)
            .dbname(&self.dbname);
        config
    }
}
async fn run(tty_path: String) {
    let temperature_sensor = Sensor {
        path: tty_path,
        baud_rate: 9600,
    };

    let config = DbConfig::from_env().into();
    let (tx, rx) = mpsc::unbounded_channel();
    let manager = PostgresConnectionManager::new(config, NoTls);
    let pool = Pool::builder().max_size(1).build(manager).await.unwrap();

    tokio::spawn(async move {
        db_thread(rx, pool).await;
    });

    match co2mon::Sensor::open_default() {
        Ok(sensor) => {
            tokio::spawn(co2_thread(sensor, tx.clone()));
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
            let reading = temperature_sensor.read().await?;
            let reading = Reading::Thermometer {
                time: Local::now(),
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

    let rt = Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    rt.block_on(async move { run(tty_path).await });
    Ok(())
}
