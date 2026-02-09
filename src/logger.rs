use log::LevelFilter;
use log4rs::{
    append::rolling_file::{
        policy::compound::{
            roll::fixed_window::FixedWindowRoller,
            trigger::size::SizeTrigger,
            CompoundPolicy,
        },
        RollingFileAppender,
    },
    config::{Appender, Config, Root},
    encode::pattern::PatternEncoder,
};
use crate::config::LoggingConfig;
use crate::paths;

/// Initialize the logging system
///
/// Logs to $XDG_STATE_HOME/mux/logs/mux.log in glog format.
/// Rotation size and archive count are controlled by config.
/// Log level is read from RUST_LOG env var, defaults to INFO if unset or invalid.
pub fn init_logger(config: &LoggingConfig) -> Result<(), Box<dyn std::error::Error>> {
    let log_dir = paths::get_log_dir()?;
    let log_file = log_dir.join("mux.log");

    // glog format: Lmmdd hh:mm:ss.uuuuuu threadid file:line] msg
    let pattern = "{l:.1}{d(%m%d %H:%M:%S%.6f)} {T} {f}:{L}] {m}{n}";

    let archive_pattern = log_dir.join("mux.{}.log").display().to_string();

    let window_roller = FixedWindowRoller::builder()
        .build(&archive_pattern, config.max_archives)?;

    let size_trigger = SizeTrigger::new(config.max_file_size_mb * 1024 * 1024);

    let compound_policy = CompoundPolicy::new(
        Box::new(size_trigger),
        Box::new(window_roller),
    );

    let file_appender = RollingFileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(pattern)))
        .build(log_file, Box::new(compound_policy))?;

    let config = Config::builder()
        .appender(Appender::builder().build("file", Box::new(file_appender)))
        .build(
            Root::builder()
                .appender("file")
                .build(log_level_from_env()),
        )?;

    log4rs::init_config(config)?;

    Ok(())
}

/// Read log level from RUST_LOG env var. Defaults to INFO if unset or invalid.
fn log_level_from_env() -> LevelFilter {
    std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| match s.to_lowercase().as_str() {
            "trace" => Some(LevelFilter::Trace),
            "debug" => Some(LevelFilter::Debug),
            "info" => Some(LevelFilter::Info),
            "warn" => Some(LevelFilter::Warn),
            "error" => Some(LevelFilter::Error),
            "off" => Some(LevelFilter::Off),
            _ => None,
        })
        .unwrap_or(LevelFilter::Info)
}
