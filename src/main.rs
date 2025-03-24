use std::{process::Command, thread, time::Duration};

use anyhow::{Context, Ok, Result};
use chrono::{Datelike, Local, NaiveDate, NaiveTime};
use sunrise::{Coordinates, SolarDay, SolarEvent};

// Coordinates and altitude of Nairobi, Kenya
const LAT: f64 = -1.2921;
const LON: f64 = 36.8219;
const ALT: f64 = 1795.;

fn get_sunrise_and_sunset() -> (NaiveTime, NaiveTime) {
    let today = Local::now().date_naive();
    let date = NaiveDate::from_ymd_opt(today.year(), today.month0(), today.day0()).unwrap();
    let coord = Coordinates::new(LAT, LON).unwrap();

    let solarday = SolarDay::new(coord, date).with_altitude(ALT);

    let sunrise = solarday
        .event_time(SolarEvent::Sunrise)
        .naive_local()
        .time();
    let sunset = solarday.event_time(SolarEvent::Sunset).naive_local().time();

    (sunrise, sunset)
}

fn kill_existing_hyprsunset() -> Result<()> {
    println!("Killing existing hyprsunset");

    Command::new("killall")
        .arg("hyprsunset")
        .spawn()
        .context("Failed to kill hyprsunset")?;

    print!("Waiting for hyprsunset to die...");
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
        .arg("--idenity")
        .spawn()
        .context("Failed to start hyprsunset")?;

    Ok(())
}

fn main() {
    let now = Local::now();
    let (sunrise, sunset) = get_sunrise_and_sunset();

    println!(
        "Sunrise: {:?}, Sunset: {:?}, Now: {:?}",
        sunrise,
        sunset,
        now.to_utc().time()
    );

    let is_sunset = now.to_utc().time() > sunrise && now.to_utc().time() > sunset;

    if is_sunset {
        println!("Enabling blue light filter");
        enable_blue_light_filter(3000).unwrap();
    } else {
        println!("Disabling blue light filter");
        disable_blue_light_filter().unwrap();
    }

    loop {
        let (sunrise, sunset) = get_sunrise_and_sunset();

        println!("Sunrise: {:?}, Sunset: {:?}", sunrise, sunset);

        let now = Local::now();

        let (next_event, is_sunset) = if now.to_utc().time() < sunrise {
            (sunrise, false)
        } else if now.to_utc().time() < sunset {
            (sunset, true)
        } else {
            // If past sunset, wait until the next day's sunrise
            let (sunrise, _) = get_sunrise_and_sunset();
            (sunrise, false)
        };

        let sleep_duration = next_event.signed_duration_since(now.to_utc().time());

        if sleep_duration.num_seconds() > 0 {
            let sleep_seconds = sleep_duration.num_seconds() as u64;
            println!(
                "Sleeping for {:.2} hours until {}",
                sleep_seconds / 3600,
                next_event
            );
            thread::sleep(Duration::from_secs(sleep_seconds));
        }

        if is_sunset {
            println!("Enabling blue light filter");
            enable_blue_light_filter(3000).unwrap();
        } else {
            println!("Disabling blue light filter");
            disable_blue_light_filter().unwrap();
        }

        // Small delay to prevent re-triggering due to time drift
        thread::sleep(Duration::from_secs(60));
    }
}
