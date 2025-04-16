use fs2::FileExt;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::fs::{self, File};
use std::result::Result::{Err, Ok};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{process::Command, str::FromStr, thread, time::Duration};

use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate, NaiveTime, Utc};
use sunrise::{Coordinates, SolarDay, SolarEvent};

// Coordinates of Nairobi, Kenya
const LAT: f64 = -1.2921;
const LON: f64 = 36.8219;

#[derive(PartialEq, Debug)]
enum ParOfDay {
    BeforeDaytime,
    Daytime,
    AfterDaytime,
}

fn get_sunrise_and_sunset(
    latitude: f64,
    longitude: f64,
    year: i32,
    month: u32,
    day: u32,
) -> (NaiveTime, NaiveTime) {
    let date = NaiveDate::from_ymd_opt(year, month, day).unwrap();
    let coord = Coordinates::new(latitude, longitude).unwrap();

    let solarday = SolarDay::new(coord, date);

    let sunrise = solarday.event_time(SolarEvent::Sunrise).time();
    let sunset = solarday.event_time(SolarEvent::Sunset).time();

    (sunrise, sunset)
}

fn get_part_of_day(time: NaiveTime, sunrise: NaiveTime, sunset: NaiveTime) -> ParOfDay {
    if time < sunrise {
        ParOfDay::BeforeDaytime
    } else if time < sunset {
        ParOfDay::Daytime
    } else {
        ParOfDay::AfterDaytime
    }
}

fn get_duration_to_next_event(time: NaiveTime, sunrise: NaiveTime, sunset: NaiveTime) -> Duration {
    let num_sec = match get_part_of_day(time, sunrise, sunset) {
        ParOfDay::BeforeDaytime => sunrise - time,
        ParOfDay::Daytime => sunset - time,
        ParOfDay::AfterDaytime => NaiveTime::from_str("23:59:59").unwrap() - time,
    };

    let result = u64::try_from(num_sec.num_seconds());

    match result {
        Ok(secs) => Duration::from_secs(secs),
        Err(_) => Duration::from_secs(0),
    }
}

#[test]
fn test_get_sunrise_and_sunset() {
    let (sunrise, sunset) = get_sunrise_and_sunset(0., 0., 1970, 1, 1);
    assert_eq!(sunrise, NaiveTime::from_str("05:59:54").unwrap());
    assert_eq!(sunset, NaiveTime::from_str("18:07:08").unwrap());
}

#[test]
fn test_get_part_of_day() {
    let (sunrise, sunset) = get_sunrise_and_sunset(0., 0., 1970, 1, 1);

    let before_daytime: NaiveTime = NaiveTime::from_str("01:30:00").unwrap();
    let daytime: NaiveTime = NaiveTime::from_str("10:30:00").unwrap();
    let after_daytime: NaiveTime = NaiveTime::from_str("23:30:00").unwrap();

    assert_eq!(
        get_part_of_day(before_daytime, sunrise, sunset),
        ParOfDay::BeforeDaytime
    );

    assert_eq!(get_part_of_day(daytime, sunrise, sunset), ParOfDay::Daytime);

    assert_eq!(
        get_part_of_day(after_daytime, sunrise, sunset),
        ParOfDay::AfterDaytime
    );
}

#[test]
fn test_duration_to_next_event() {
    let (sunrise, sunset) = get_sunrise_and_sunset(0., 0., 1970, 1, 1);

    let before_daytime: NaiveTime = NaiveTime::from_str("01:30:00").unwrap();
    let daytime: NaiveTime = NaiveTime::from_str("10:30:00").unwrap();
    let after_daytime: NaiveTime = NaiveTime::from_str("23:30:00").unwrap();

    assert_eq!(
        get_duration_to_next_event(before_daytime, sunrise, sunset),
        Duration::from_secs(16194)
    );

    assert_eq!(
        get_duration_to_next_event(daytime, sunrise, sunset),
        Duration::from_secs(27428)
    );

    assert_eq!(
        get_duration_to_next_event(after_daytime, sunrise, sunset),
        Duration::from_secs(1799)
    );
}

fn kill_existing_hyprsunset() -> Result<()> {
    println!("Killing existing hyprsunset");

    Command::new("killall")
        .arg("hyprsunset")
        .spawn()
        .context("Failed to kill hyprsunset")?;

    println!("Waiting for hyprsunset to die...");
    thread::sleep(Duration::from_millis(500));

    Ok(())
}

fn enable_blue_light_filter(temperature: u32) -> Result<()> {
    kill_existing_hyprsunset().unwrap();

    Command::new("hyprsunset")
        .arg("--temperature")
        .arg(temperature.to_string())
        .spawn()
        .context("Failed to start hyprsunset")?;

    Ok(())
}

fn disable_blue_light_filter() -> Result<()> {
    kill_existing_hyprsunset().unwrap();

    Command::new("hyprsunset")
        .arg("--identity")
        .spawn()
        .context("Failed to start hyprsunset")?;

    Ok(())
}

fn main() {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    let mut signals = match Signals::new([SIGINT, SIGTERM]) {
        Ok(signals) => signals,
        Err(e) => {
            println!("Failed to create signal handler: {}", e);
            return;
        }
    };

    thread::spawn(move || {
        for signal in signals.forever() {
            println!("Shutdown signal received: {:?}", signal);
            r.store(false, Ordering::SeqCst);
        }
    });

    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let lock_path = format!("{}/hyprsunset-overdrive.lock", runtime_dir);
    let lock_file = match File::create(&lock_path) {
        Ok(file) => file,
        Err(_) => {
            println!("Failed to create lock file");
            return;
        }
    };

    match lock_file.try_lock_exclusive() {
        Ok(_) => {
            println!("Lock acquired");

            while running.load(Ordering::SeqCst) {
                let now = Utc::now();
                let (sunrise, sunset) =
                    get_sunrise_and_sunset(LAT, LON, now.year(), now.month(), now.day());

                println!("Sunrise: {:?}, Sunset: {:?}", sunrise, sunset);

                let op_result = match get_part_of_day(now.time(), sunrise, sunset) {
                    ParOfDay::BeforeDaytime => enable_blue_light_filter(3000),
                    ParOfDay::Daytime => disable_blue_light_filter(),
                    ParOfDay::AfterDaytime => enable_blue_light_filter(3000),
                };

                match op_result {
                    Ok(_) => println!("Successfully updated hyprsunset!"),
                    Err(e) => println!("Failed to update hyprsunset!: {}", e),
                }

                let sleep_duration = get_duration_to_next_event(now.time(), sunrise, sunset);

                let sleep_seconds = sleep_duration.as_secs() as u64;
                println!("Sleeping for {:.2} hours until", sleep_seconds / 3600);

                let mut slept_duration = Duration::from_secs(0);
                while slept_duration < sleep_duration && running.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_secs(1));
                    slept_duration += Duration::from_secs(1);
                }

                if !running.load(Ordering::SeqCst) {
                    break;
                }

                // Small delay to prevent re-triggering due to time drift
                thread::sleep(Duration::from_secs(60));
            }

            // Cleanup

            // Not required, but release early
            drop(lock_file);

            match fs::remove_file(lock_path) {
                Ok(_) => println!("Lock released"),
                Err(e) => println!("Failed to release lock: {}", e),
            };

            println!("Cleanup complete");
            println!("Exiting");
        }
        Err(_) => {
            println!("Failed to acquire lock. Another instance is running.");
            println!("Exiting");
        }
    }
}
