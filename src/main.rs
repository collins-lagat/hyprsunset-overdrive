use std::fs::{self, File};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::result::Result::{Err, Ok};
use std::sync::mpsc::channel;
use std::{str::FromStr, thread, time::Duration};

use anyhow::{Context, Result, anyhow};
use chrono::{Datelike, NaiveDate, NaiveTime, Utc};
use fs2::FileExt;
use log::{error, info};
use serde::Deserialize;
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use simplelog::{
    ColorChoice, CombinedLogger, Config as LogConfig, LevelFilter, TermLogger, TerminalMode,
    WriteLogger,
};
use sunrise::{Coordinates, SolarDay, SolarEvent};
use tray_icon::Icon;

const ENABLED_ICON_BYTES: &[u8] = include_bytes!("../assets/enabled.png");
const DISABLED_ICON_BYTES: &[u8] = include_bytes!("../assets/disabled.png");

#[derive(Debug, Deserialize)]
struct Config {
    temperature: i32,
    latitude: f64,
    longitude: f64,
    altitude: f64,
}

impl Config {
    fn load() -> Result<Self> {
        let config_path = match dirs::config_dir() {
            Some(dir) => dir.join("hypr").join("hyprsunset-overdrive.toml"),
            None => {
                return Err(anyhow!("Failed to find config directory"));
            }
        };

        if !config_path.exists() {
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent).context("Failed to create config directory")?;
            };

            let default_config = r#"temperature = 3000
# Coordinates for Nairobi, Kenya
latitude = -1.2921
longitude = 36.8219
# Altitude of Nairobi, Kenya in meters. You can set it as 0.
altitude = 1795
            "#;

            match fs::write(&config_path, default_config) {
                Ok(_) => info!("Created default config file"),
                Err(e) => return Err(e).context("Failed to create default config file"),
            };
        }

        let config_contents = match fs::read_to_string(&config_path) {
            Ok(config_str) => config_str,
            Err(_) => return Err(anyhow!("Failed to read config file")),
        };

        let config: Config = match toml::from_str(&config_contents) {
            Ok(config) => config,
            Err(_) => return Err(anyhow!("Failed to parse config file")),
        };

        info!("Config loaded");

        Ok(config)
    }
}

#[derive(Debug, PartialEq)]
enum Message {
    Day,
    Night,
    Shutdown,
}

#[derive(PartialEq, Debug)]
enum ParOfDay {
    BeforeDaytime,
    Daytime,
    AfterDaytime,
}

fn get_sunrise_and_sunset(
    latitude: f64,
    longitude: f64,
    altitude: f64,
    year: i32,
    month: u32,
    day: u32,
) -> (NaiveTime, NaiveTime) {
    let date = NaiveDate::from_ymd_opt(year, month, day).unwrap();
    let coord = Coordinates::new(latitude, longitude).unwrap();

    let solarday = SolarDay::new(coord, date).with_altitude(altitude);

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
    let (sunrise, sunset) = get_sunrise_and_sunset(0., 0., 0., 1970, 1, 1);
    assert_eq!(sunrise, NaiveTime::from_str("05:59:54").unwrap());
    assert_eq!(sunset, NaiveTime::from_str("18:07:08").unwrap());
}

#[test]
fn test_get_part_of_day() {
    let (sunrise, sunset) = get_sunrise_and_sunset(0., 0., 0., 1970, 1, 1);

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
    let (sunrise, sunset) = get_sunrise_and_sunset(0., 0., 0., 1970, 1, 1);

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

struct HyprsunsetClient {
    sock_path: PathBuf,
}

impl HyprsunsetClient {
    fn new(sock_path: PathBuf) -> Self {
        Self { sock_path }
    }

    fn create_socket(&self, socket_path: &PathBuf) -> Result<UnixStream> {
        let sock = match UnixStream::connect(socket_path) {
            Ok(sock) => sock,
            Err(e) => {
                return Err(e)
                    .context(format!("Failed to connect to socket at: {:?}", socket_path));
            }
        };
        Ok(sock)
    }

    fn send_command(&mut self, command: &str) -> Result<()> {
        let mut sock = self.create_socket(&self.sock_path)?;

        // Set short timeout to prevent hanging
        if let Err(e) = sock.set_read_timeout(Some(Duration::from_millis(500))) {
            return Err(e).context("Failed to set read timeout");
        };

        match sock.write_all(command.as_bytes()) {
            Ok(_) => Ok(()),
            Err(e) => Err(e).context("Failed to send command to hyprsunset"),
        }
    }

    fn enable(&mut self, temperature: i32) -> Result<()> {
        self.send_command(format!("temperature {}", temperature).as_str())
    }

    fn disable(&mut self) -> Result<()> {
        self.send_command("identity")
    }
}

fn get_hyprsunset_socket_path() -> Result<PathBuf> {
    let his = match std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok() {
        Some(env) => env,
        None => return Err(anyhow!("HYPRSUNSET_INSTANCE_SIGNATURE not set")),
    };

    let runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) => dir,
        Err(_) => return Err(anyhow!("XDG_RUNTIME_DIR not set")),
    };

    let socket_path = PathBuf::from(format!("{}/hypr/{}/.hyprsunset.sock", runtime_dir, his));

    match wait_for_hyprsunset_socket(&socket_path) {
        Ok(_) => Ok(socket_path),
        Err(e) => Err(e).context("Failed to wait for hyprsunset socket"),
    }
}

fn wait_for_hyprsunset_socket(socket_path: &Path) -> Result<()> {
    let mut tries = 0;
    while tries < 10 {
        if socket_path.exists() {
            info!("Socket path exists");
            return Ok(());
        }
        tries += 1;
        info!("Socket path does not exist. Waiting 1 second");
        thread::sleep(Duration::from_secs(1));
    }

    anyhow::bail!("hyprsunset did not create socket");
}

fn verify_hyprsunset_is_installed() -> Result<()> {
    match Command::new("which").arg("hyprsunset").output() {
        Ok(output) => {
            if !output.status.success() {
                anyhow::bail!("hyprsunset is not installed");
            };
            info!("hyprsunset is installed");
            Ok(())
        }
        Err(e) => anyhow::bail!("Failed to check if hyprsunset is installed: {}", e),
    }
}

fn wait_for_hyprsunset_to_start() -> Result<()> {
    let mut tries = 0;
    while tries < 10 {
        match Command::new("hyprsunset").arg("--help").output() {
            Ok(output) => {
                if output.status.success() {
                    info!("hyprsunset is running");
                    return Ok(());
                }
            }
            Err(e) => anyhow::bail!("Failed to check if hyprsunset is running: {}", e),
        }
        tries += 1;
        info!("hyprsunset is not running. Waiting 1 second");
        thread::sleep(Duration::from_secs(1));
    }
    anyhow::bail!("hyprsunset failed to start");
}

fn setup_logging() {
    let runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) => dir,
        Err(_) => {
            println!("Failed to get XDG_RUNTIME_DIR when setting up logging");
            return;
        }
    };

    let log_path = format!("{}/hyprsunset-overdrive.log", runtime_dir);
    let log_file = match File::create(log_path) {
        Ok(file) => file,
        Err(_) => {
            println!("Failed to create log file when setting up logging");
            return;
        }
    };

    if let Err(e) = CombinedLogger::init(vec![
        TermLogger::new(
            LevelFilter::Info,
            LogConfig::default(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        ),
        WriteLogger::new(LevelFilter::Info, LogConfig::default(), log_file),
    ]) {
        println!("Failed to initialize logging: {}", e);
    };
}

fn convert_bytes_to_icon(bytes: &[u8]) -> Result<Icon> {
    let image_buff = match image::load_from_memory(bytes) {
        Ok(image_dyn) => image_dyn.into_rgba8(),
        Err(e) => return Err(e).context("Failed to load icon"),
    };

    let (width, height) = image_buff.dimensions();
    let icon_rgba = image_buff.into_raw();

    let icon = match Icon::from_rgba(icon_rgba, width, height) {
        Ok(icon) => icon,
        Err(e) => return Err(e).context("Failed to create icon"),
    };

    Ok(icon)
}

fn main() {
    setup_logging();
    match verify_hyprsunset_is_installed() {
        Ok(_) => {}
        Err(e) => {
            error!("Failed to verify hyprsunset is installed: {}", e);
            return;
        }
    };
    match wait_for_hyprsunset_to_start() {
        Ok(_) => {}
        Err(e) => {
            error!("Failed to wait for hyprsunset to start: {}", e);
            return;
        }
    };

    let (tx, rx) = channel::<Message>();

    let mut signals = match Signals::new([SIGINT, SIGTERM]) {
        Ok(signals) => signals,
        Err(e) => {
            error!("Failed to create signal handler: {}", e);
            return;
        }
    };

    let signal_tx = tx.clone();
    thread::spawn(move || {
        for signal in signals.forever() {
            info!("Shutdown signal received: {:?}", signal);
            signal_tx.send(Message::Shutdown).unwrap();
        }
    });

    let runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) => dir,
        Err(_) => {
            error!("Failed to get XDG_RUNTIME_DIR");
            return;
        }
    };
    let lock_path = format!("{}/hyprsunset-overdrive.lock", runtime_dir);
    let lock_file = match File::create(&lock_path) {
        Ok(file) => file,
        Err(_) => {
            error!("Failed to create lock file");
            return;
        }
    };

    if lock_file.try_lock_exclusive().is_err() {
        error!("Failed to acquire lock. Another instance is running.");
        error!("Exiting");
        return;
    }

    info!("Lock acquired");

    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            error!("Failed to load config: {}", e);
            return;
        }
    };

    let (gtk_tx, gtk_rx) = channel::<Message>();

    // We need gtk in order to build the tray icon in linux.
    // Without gtk, the tray icon build will fail. You'll see an error
    // message in the terminal.
    // Also, this will be spawned in a separate thread as calling gtk::main()
    // will block the main thread.
    std::thread::spawn(|| {
        use glib;
        use tray_icon::{TrayIconBuilder, menu::Menu};

        gtk::init().unwrap();

        let icon = match convert_bytes_to_icon(ENABLED_ICON_BYTES) {
            Ok(icon) => icon,
            Err(e) => {
                error!("Failed to convert bytes to icon: {}", e);
                return;
            }
        };

        // Tray icons withoutmenus are not displayed on linux.
        // Therefore, we need to addan empty menu to the tray icon.
        // See: https://github.com/tauri-apps/tray-icon/blob/97723fd207add9c3bb0511cb0e4d04d8652a0027/src/lib.rs#L255
        // See: https://github.com/libsdl-org/SDL/issues/12092

        let menu = Menu::new();

        let tray_icon = match TrayIconBuilder::new().with_menu(Box::new(menu)).build() {
            Ok(tray_icon) => tray_icon,
            Err(e) => {
                error!("Failed to build tray icon: {}", e);
                return;
            }
        };

        if let Err(e) = tray_icon.set_icon(Some(icon)) {
            error!("Failed to set icon: {}", e);
            return;
        };

        // Source: https://github.com/PlugOvr-ai/PlugOvr/blob/273d7ea0f00a725db5b40838e497bd3ecfe2c95e/src/ui/user_interface.rs#L313
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            while let Ok(message) = gtk_rx.try_recv() {
                match message {
                    Message::Night => {
                        let enabled_icon = match convert_bytes_to_icon(ENABLED_ICON_BYTES) {
                            Ok(icon) => icon,
                            Err(e) => {
                                error!("Failed to convert bytes to icon: {}", e);
                                return glib::ControlFlow::Break;
                            }
                        };
                        if let Err(e) = tray_icon.set_icon(Some(enabled_icon)) {
                            error!("Failed to set icon: {}", e);
                            return glib::ControlFlow::Break;
                        };
                    }
                    Message::Day => {
                        let disabled_icon = match convert_bytes_to_icon(DISABLED_ICON_BYTES) {
                            Ok(icon) => icon,
                            Err(e) => {
                                error!("Failed to convert bytes to icon: {}", e);
                                return glib::ControlFlow::Break;
                            }
                        };
                        if let Err(e) = tray_icon.set_icon(Some(disabled_icon)) {
                            error!("Failed to set icon: {}", e);
                            return glib::ControlFlow::Break;
                        };
                    }
                    Message::Shutdown => {
                        return glib::ControlFlow::Break;
                    }
                };
            }
            glib::ControlFlow::Continue
        });

        gtk::main();
    });

    let hyprsunset_sock_path = match get_hyprsunset_socket_path() {
        Ok(path) => path,
        Err(e) => {
            error!("Failed to get hyprsunset socket path: {}", e);
            return;
        }
    };

    let sunset_tx = tx.clone();
    thread::spawn(move || {
        loop {
            let now = Utc::now();
            let (sunrise, sunset) = get_sunrise_and_sunset(
                config.latitude,
                config.longitude,
                config.altitude,
                now.year(),
                now.month(),
                now.day(),
            );

            info!("Sunrise: {:?}, Sunset: {:?}", sunrise, sunset);

            match get_part_of_day(now.time(), sunrise, sunset) {
                ParOfDay::Daytime => {
                    sunset_tx.send(Message::Day).unwrap();
                }
                ParOfDay::BeforeDaytime | ParOfDay::AfterDaytime => {
                    sunset_tx.send(Message::Night).unwrap();
                }
            };

            let sleep_duration = get_duration_to_next_event(now.time(), sunrise, sunset);

            let sleep_seconds = sleep_duration.as_secs() as u64;
            info!("Sleeping for {:.2} hours", sleep_seconds / 3600);

            let mut slept_duration = Duration::from_secs(0);
            while slept_duration < sleep_duration {
                thread::sleep(Duration::from_secs(1));
                slept_duration += Duration::from_secs(1);
            }

            // Small delay to prevent re-triggering due to time drift
            thread::sleep(Duration::from_secs(60));
        }
    });

    let mut client = HyprsunsetClient::new(hyprsunset_sock_path);

    loop {
        let message = match rx.recv() {
            Ok(message) => message,
            Err(e) => {
                error!("Failed to receive message: {}", e);
                return;
            }
        };

        match message {
            Message::Day => {
                match client.disable() {
                    Ok(_) => info!("Successfully disabled blue light filter"),
                    Err(e) => error!("Failed to disable blue light filter: {}", e),
                };
                gtk_tx.send(Message::Day).unwrap();
            }
            Message::Night => {
                match client.enable(config.temperature) {
                    Ok(_) => info!("Successfully set blue light filter"),
                    Err(e) => error!("Failed to set blue light filter: {}", e),
                };
                gtk_tx.send(Message::Night).unwrap();
            }
            Message::Shutdown => {
                break;
            }
        };
    }

    // Cleanup

    // Not required, but release early
    drop(lock_file);

    match fs::remove_file(lock_path) {
        Ok(_) => info!("Lock released"),
        Err(e) => error!("Failed to release lock: {}", e),
    };

    info!("Cleanup complete");
    info!("Exiting");
}
