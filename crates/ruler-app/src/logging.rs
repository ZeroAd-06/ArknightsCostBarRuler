use std::{
    env,
    fs::{self, File},
    io::{self, LineWriter, Write},
    path::{Path, PathBuf},
    process,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use log::{Level, LevelFilter, Log, Metadata, Record};
use ruler_core::RulerConfig;

use crate::{resources::ResourceLocator, ICU_PROVIDER_ERROR_LOG_TARGET};

const DEFAULT_LEVEL: LevelFilter = LevelFilter::Info;
const TRACE_LEVEL: LevelFilter = LevelFilter::Trace;
const RETENTION_DAYS: u64 = 7;
const RETENTION_SECS: u64 = RETENTION_DAYS * 24 * 60 * 60;

#[derive(Clone, Debug)]
pub struct LoggingRuntime {
    session_dir: PathBuf,
    override_level: Option<LevelFilter>,
    effective_level: Arc<AtomicUsize>,
}

impl LoggingRuntime {
    pub fn init(resources: &ResourceLocator) -> Result<Self, String> {
        let existing_config = load_existing_config(resources.config_path());
        let override_level = env_override_level();
        let trace_enabled = existing_config
            .as_ref()
            .map(|config| config.trace_logging_enabled)
            .unwrap_or(false);
        let log_root_dir = resources.log_root_dir(
            existing_config
                .as_ref()
                .and_then(|config| config.log_output_dir.as_deref()),
        );

        fs::create_dir_all(&log_root_dir).map_err(|error| {
            format!(
                "failed to create log root '{}': {error}",
                log_root_dir.display()
            )
        })?;

        let now_secs = now_unix_secs();
        let session_dir = create_session_dir(&log_root_dir, now_secs)?;
        let log_file_path = session_dir.join("app.log");
        let log_file = File::create(&log_file_path).map_err(|error| {
            format!(
                "failed to create session log '{}': {error}",
                log_file_path.display()
            )
        })?;

        let effective_level = Arc::new(AtomicUsize::new(level_to_usize(override_level.unwrap_or(
            if trace_enabled {
                TRACE_LEVEL
            } else {
                DEFAULT_LEVEL
            },
        ))));
        let logger = SessionLogger::new(Arc::clone(&effective_level), log_file);
        log::set_boxed_logger(Box::new(logger))
            .map_err(|error| format!("failed to install logger: {error}"))?;
        log::set_max_level(LevelFilter::Trace);

        let runtime = Self {
            session_dir,
            override_level,
            effective_level,
        };

        runtime.apply_trace_setting(trace_enabled);
        cleanup_old_sessions(&log_root_dir, now_secs, &runtime.session_dir);
        log::info!(
            "file logging initialized: session_dir={}, root={}",
            runtime.session_dir.display(),
            log_root_dir.display()
        );
        if let Some(level) = runtime.override_level {
            log::info!("RUST_LOG developer override active at {level:?}");
        }

        Ok(runtime)
    }

    #[must_use]
    pub fn session_dir(&self) -> PathBuf {
        self.session_dir.clone()
    }

    pub fn apply_trace_setting(&self, trace_enabled: bool) {
        let level = self.override_level.unwrap_or(if trace_enabled {
            TRACE_LEVEL
        } else {
            DEFAULT_LEVEL
        });
        self.effective_level
            .store(level_to_usize(level), Ordering::Relaxed);
    }
}

struct SessionLogger {
    effective_level: Arc<AtomicUsize>,
    file: Mutex<LineWriter<File>>,
}

impl SessionLogger {
    fn new(effective_level: Arc<AtomicUsize>, file: File) -> Self {
        Self {
            effective_level,
            file: Mutex::new(LineWriter::new(file)),
        }
    }

    fn current_level(&self) -> LevelFilter {
        usize_to_level(self.effective_level.load(Ordering::Relaxed))
    }

    fn should_log(&self, metadata: &Metadata<'_>) -> bool {
        should_log(metadata.target(), metadata.level(), self.current_level())
    }
}

impl Log for SessionLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.should_log(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let line = format_log_line(record);
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }

        let mut stderr = io::stderr().lock();
        let _ = stderr.write_all(line.as_bytes());
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

fn load_existing_config(config_path: PathBuf) -> Option<RulerConfig> {
    if !config_path.exists() {
        return None;
    }
    RulerConfig::load_from_path(config_path).ok()
}

fn env_override_level() -> Option<LevelFilter> {
    let raw = env::var("RUST_LOG").ok()?;
    parse_override_level(&raw)
}

fn parse_override_level(raw: &str) -> Option<LevelFilter> {
    raw.split(',')
        .filter_map(|segment| {
            let value = segment.rsplit('=').next().unwrap_or(segment).trim();
            value.parse::<LevelFilter>().ok()
        })
        .max_by_key(|level| level_to_usize(*level))
}

fn create_session_dir(root: &Path, now_secs: u64) -> Result<PathBuf, String> {
    let session_name = session_dir_name(now_secs, process::id());
    let session_dir = root.join(session_name);
    fs::create_dir_all(&session_dir).map_err(|error| {
        format!(
            "failed to create session log directory '{}': {error}",
            session_dir.display()
        )
    })?;
    Ok(session_dir)
}

fn cleanup_old_sessions(root: &Path, now_secs: u64, current_session_dir: &Path) {
    let cutoff = timestamp_prefix(now_secs.saturating_sub(RETENTION_SECS));
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            log::warn!("failed to enumerate log root '{}': {error}", root.display());
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path == current_session_dir {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(prefix) = session_timestamp_prefix(&name) else {
            continue;
        };
        if prefix >= cutoff.as_str() {
            continue;
        }

        match fs::remove_dir_all(&path) {
            Ok(()) => log::info!("removed expired log session '{}'", path.display()),
            Err(error) => log::warn!(
                "failed to remove expired log session '{}': {error}",
                path.display()
            ),
        }
    }
}

fn session_timestamp_prefix(name: &str) -> Option<&str> {
    let prefix = name.get(..15)?;
    let bytes = prefix.as_bytes();
    (bytes.get(8) == Some(&b'_')
        && bytes[..8].iter().all(|byte| byte.is_ascii_digit())
        && bytes[9..15].iter().all(|byte| byte.is_ascii_digit()))
    .then_some(prefix)
}

fn format_log_line(record: &Record<'_>) -> String {
    format!(
        "{} {:<5} [{}] {}\n",
        system_time_with_millis(SystemTime::now()),
        record.level(),
        record.target(),
        record.args()
    )
}

fn system_time_with_millis(time: SystemTime) -> String {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let prefix = timestamp_prefix(duration.as_secs());
    format!("{prefix}.{:03}", duration.subsec_millis())
}

fn session_dir_name(now_secs: u64, pid: u32) -> String {
    format!("{}_{}", timestamp_prefix(now_secs), pid)
}

fn timestamp_prefix(unix_secs: u64) -> String {
    let days = unix_secs / 86_400;
    let seconds_of_day = unix_secs % 86_400;
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = days_since_epoch_to_ymd(days as i64);
    format!("{year:04}{month:02}{day:02}_{hour:02}{minute:02}{second:02}")
}

fn days_since_epoch_to_ymd(days: i64) -> (i64, u32, u32) {
    let mut year = 1970i64;
    let mut remaining_days = days;
    loop {
        let year_days = if is_leap(year) { 366 } else { 365 };
        if remaining_days < year_days {
            break;
        }
        remaining_days -= year_days;
        year += 1;
    }

    let month_days: &[u32] = if is_leap(year) {
        &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u32;
    for &days_in_month in month_days {
        if remaining_days < days_in_month as i64 {
            break;
        }
        remaining_days -= days_in_month as i64;
        month += 1;
    }

    (year, month, (remaining_days + 1) as u32)
}

const fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

const fn level_to_usize(level: LevelFilter) -> usize {
    match level {
        LevelFilter::Off => 0,
        LevelFilter::Error => 1,
        LevelFilter::Warn => 2,
        LevelFilter::Info => 3,
        LevelFilter::Debug => 4,
        LevelFilter::Trace => 5,
    }
}

const fn usize_to_level(value: usize) -> LevelFilter {
    match value {
        0 => LevelFilter::Off,
        1 => LevelFilter::Error,
        2 => LevelFilter::Warn,
        3 => LevelFilter::Info,
        4 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}

fn should_log(target: &str, level: Level, threshold: LevelFilter) -> bool {
    if target == ICU_PROVIDER_ERROR_LOG_TARGET && level > Level::Error {
        return false;
    }

    match threshold {
        LevelFilter::Off => false,
        LevelFilter::Error => level <= Level::Error,
        LevelFilter::Warn => level <= Level::Warn,
        LevelFilter::Info => level <= Level::Info,
        LevelFilter::Debug => level <= Level::Debug,
        LevelFilter::Trace => true,
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_timestamp_prefix_requires_expected_shape() {
        assert_eq!(
            session_timestamp_prefix("20260620_213045_1234"),
            Some("20260620_213045")
        );
        assert_eq!(session_timestamp_prefix("20260620-213045_1234"), None);
        assert_eq!(session_timestamp_prefix("junk"), None);
    }

    #[test]
    fn env_override_parser_takes_most_verbose_match() {
        assert_eq!(
            parse_override_level("warn,ruler_core=trace,icu_provider::error=error"),
            Some(LevelFilter::Trace)
        );
    }

    #[test]
    fn should_log_preserves_icu_warning_suppression() {
        assert!(!should_log(
            ICU_PROVIDER_ERROR_LOG_TARGET,
            Level::Warn,
            LevelFilter::Trace
        ));
        assert!(should_log(
            ICU_PROVIDER_ERROR_LOG_TARGET,
            Level::Error,
            LevelFilter::Trace
        ));
        assert!(should_log(
            "ruler_app::worker",
            Level::Info,
            LevelFilter::Info
        ));
        assert!(!should_log(
            "ruler_app::worker",
            Level::Debug,
            LevelFilter::Info
        ));
    }
}
