//! adb executable resolver.
//!
//! Mirrors the MaaFramework pattern: try `adb` on `PATH` first; if that
//! fails, fall back to the `adb.exe` bundled with a running MuMu or LDPlayer
//! emulator. The emulator install paths themselves are discovered by the
//! host process (see `ruler_app::target_discovery::discover_emulator_adb_paths`)
//! and pushed into the resolver before the first adb call.
//!
//! The resolution is cached process-globally so every capture backend,
//! Android settings guard, and the first-launch wizard agree on the same
//! executable. The wizard can re-run the resolution on refresh in case the
//! user started an emulator after the wizard opened.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// A successfully resolved adb executable.
#[derive(Clone, Debug)]
pub struct AdbExecutable {
    path: PathBuf,
    /// `true` if `path` is the literal string `"adb"` and the actual
    /// executable is being looked up via `PATH` on every invocation.
    from_path: bool,
}

impl AdbExecutable {
    /// Returns the path string to pass to `Command::new`. For PATH lookups
    /// this is the literal `"adb"`; for emulator bundles it is the absolute
    /// path to the bundled `adb.exe`.
    #[must_use]
    pub fn path(&self) -> &str {
        self.path.to_str().unwrap_or("adb")
    }

    /// `true` if the executable was found via `PATH` (rather than from an
    /// emulator install directory).
    #[must_use]
    pub fn from_path(&self) -> bool {
        self.from_path
    }

    /// Build a fresh `Command` preconfigured to invoke this adb executable.
    /// On Windows the command is given `CREATE_NO_WINDOW` so background adb
    /// invocations don't flash a console.
    #[must_use]
    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.path);
        configure_hidden_command(&mut command);
        command
    }
}

static RESOLVED: OnceLock<Mutex<Option<AdbExecutable>>> = OnceLock::new();

fn resolved_cell() -> &'static Mutex<Option<AdbExecutable>> {
    RESOLVED.get_or_init(|| Mutex::new(None))
}

/// Probe `adb` on `PATH` and then each supplied emulator-bundled candidate.
/// Updates the process-global cache and returns the freshly resolved value.
///
/// Pass an empty slice to probe only `PATH`. On non-Windows the slice is
/// effectively always empty because MuMu / LDPlayer are Windows-only.
pub fn resolve_adb_with(candidates: &[PathBuf]) -> Option<AdbExecutable> {
    let resolved = resolve_adb_inner(candidates);
    if let Ok(mut guard) = resolved_cell().lock() {
        *guard = resolved.clone();
    }
    resolved
}

/// Return the cached resolution. Returns `None` if `resolve_adb_with` has
/// not been called yet, or the last resolution failed.
pub fn resolved_adb() -> Option<AdbExecutable> {
    resolved_cell()
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
}

/// `true` if adb is currently resolved.
pub fn adb_available() -> bool {
    resolved_adb().is_some()
}

/// Convenience for callers that want either a `Command` or an error string
/// suitable for surfacing to the user (e.g. the wizard's target probe).
pub fn adb_command() -> Result<Command, String> {
    resolved_adb()
        .map(|exe| exe.to_command())
        .ok_or_else(|| {
            "adb is not available: install Android platform-tools on PATH \
             or start MuMu / LDPlayer emulator with its bundled adb"
                .to_string()
        })
}

fn resolve_adb_inner(candidates: &[PathBuf]) -> Option<AdbExecutable> {
    if try_run_adb(Path::new("adb")) {
        log::info!("adb resolved via PATH");
        return Some(AdbExecutable {
            path: PathBuf::from("adb"),
            from_path: true,
        });
    }

    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        if try_run_adb(candidate) {
            log::info!("adb resolved from emulator bundle: {}", candidate.display());
            return Some(AdbExecutable {
                path: candidate.clone(),
                from_path: false,
            });
        }
    }

    log::warn!(
        "adb could not be resolved: PATH lookup failed and {} emulator candidate(s) were tried",
        candidates.len()
    );
    None
}

/// Run `<program> version` to confirm the executable exists and is callable.
fn try_run_adb(program: &Path) -> bool {
    let mut command = Command::new(program);
    configure_hidden_command(&mut command);
    let output = command
        .arg("version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(output) => output.status.success(),
        Err(error) => {
            log::trace!(
                "adb probe failed for '{}': {error}",
                program.display()
            );
            false
        }
    }
}

fn configure_hidden_command(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    // On non-Windows there is no console-window flag to set; the parameter
    // is still consumed by the function signature for API symmetry.
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn from_path_executable_reports_from_path_true() {
        let exe = AdbExecutable {
            path: PathBuf::from("adb"),
            from_path: true,
        };
        assert!(exe.from_path());
        assert_eq!(exe.path(), "adb");
    }

    #[test]
    fn bundled_executable_reports_from_path_false() {
        let exe = AdbExecutable {
            path: PathBuf::from("C:\\MuMu\\shell\\adb.exe"),
            from_path: false,
        };
        assert!(!exe.from_path());
        assert_eq!(exe.path(), "C:\\MuMu\\shell\\adb.exe");
    }
}
