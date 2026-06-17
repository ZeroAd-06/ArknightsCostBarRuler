use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

const SYSTEM_NAMESPACE: &str = "system";
const INPUT_OVERLAY_SETTINGS: [&str; 2] = ["show_touches", "pointer_location"];

#[derive(Debug)]
pub(crate) struct AndroidInputOverlayGuard {
    device_id: String,
    active: bool,
}

impl AndroidInputOverlayGuard {
    pub(crate) fn disable_for_device(device_id: &str) -> Result<Self, String> {
        let device_id = normalize_device_id(device_id)?;
        let mut backend = AdbSettingsBackend;
        acquire_guard(&device_id, &mut backend)?;
        Ok(Self {
            device_id,
            active: true,
        })
    }

    fn restore(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        let mut backend = AdbSettingsBackend;
        release_guard(&self.device_id, &mut backend)?;
        self.active = false;
        Ok(())
    }

    #[cfg(test)]
    fn disable_with_backend<B: SettingsBackend>(
        device_id: &str,
        backend: &mut B,
    ) -> Result<Self, String> {
        let device_id = normalize_device_id(device_id)?;
        acquire_guard(&device_id, backend)?;
        Ok(Self {
            device_id,
            active: true,
        })
    }

    #[cfg(test)]
    fn restore_with_backend<B: SettingsBackend>(&mut self, backend: &mut B) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        release_guard(&self.device_id, backend)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for AndroidInputOverlayGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SavedSetting {
    key: &'static str,
    original_value: Option<String>,
}

#[derive(Debug)]
struct GuardEntry {
    refs: usize,
    saved_settings: Vec<SavedSetting>,
}

trait SettingsBackend {
    fn get(&mut self, device_id: &str, key: &str) -> Result<Option<String>, String>;
    fn put(&mut self, device_id: &str, key: &str, value: &str) -> Result<(), String>;
    fn delete(&mut self, device_id: &str, key: &str) -> Result<(), String>;
}

struct AdbSettingsBackend;

impl SettingsBackend for AdbSettingsBackend {
    fn get(&mut self, device_id: &str, key: &str) -> Result<Option<String>, String> {
        let output = run_adb_text(&[
            "-s",
            device_id,
            "shell",
            "settings",
            "get",
            SYSTEM_NAMESPACE,
            key,
        ])?;
        Ok(parse_settings_value(&output))
    }

    fn put(&mut self, device_id: &str, key: &str, value: &str) -> Result<(), String> {
        run_adb_text(&[
            "-s",
            device_id,
            "shell",
            "settings",
            "put",
            SYSTEM_NAMESPACE,
            key,
            value,
        ])
        .map(|_| ())
    }

    fn delete(&mut self, device_id: &str, key: &str) -> Result<(), String> {
        run_adb_text(&[
            "-s",
            device_id,
            "shell",
            "settings",
            "delete",
            SYSTEM_NAMESPACE,
            key,
        ])
        .map(|_| ())
    }
}

fn registry() -> &'static Mutex<HashMap<String, GuardEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, GuardEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn acquire_guard<B: SettingsBackend>(device_id: &str, backend: &mut B) -> Result<(), String> {
    let mut registry = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(entry) = registry.get_mut(device_id) {
        entry.refs += 1;
        return Ok(());
    }

    let saved_settings = disable_input_overlay_settings(device_id, backend)?;
    registry.insert(
        device_id.to_string(),
        GuardEntry {
            refs: 1,
            saved_settings,
        },
    );
    Ok(())
}

fn release_guard<B: SettingsBackend>(device_id: &str, backend: &mut B) -> Result<(), String> {
    let mut registry = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let Some(entry) = registry.get_mut(device_id) else {
        return Ok(());
    };
    if entry.refs > 1 {
        entry.refs -= 1;
        return Ok(());
    }

    let entry = registry.remove(device_id).expect("entry exists");
    restore_input_overlay_settings(device_id, backend, &entry.saved_settings)
}

fn disable_input_overlay_settings<B: SettingsBackend>(
    device_id: &str,
    backend: &mut B,
) -> Result<Vec<SavedSetting>, String> {
    let mut saved = Vec::with_capacity(INPUT_OVERLAY_SETTINGS.len());
    for key in INPUT_OVERLAY_SETTINGS {
        let original_value = match backend.get(device_id, key) {
            Ok(value) => value,
            Err(error) => {
                let _ = restore_input_overlay_settings(device_id, backend, &saved);
                return Err(format!("failed to read Android setting '{key}': {error}"));
            }
        };
        saved.push(SavedSetting {
            key,
            original_value,
        });
        if let Err(error) = backend.put(device_id, key, "0") {
            let _ = restore_input_overlay_settings(device_id, backend, &saved);
            return Err(format!(
                "failed to disable Android setting '{key}': {error}"
            ));
        }
    }
    Ok(saved)
}

fn restore_input_overlay_settings<B: SettingsBackend>(
    device_id: &str,
    backend: &mut B,
    saved_settings: &[SavedSetting],
) -> Result<(), String> {
    let mut first_error = None;
    for setting in saved_settings.iter().rev() {
        let result = match setting.original_value.as_deref() {
            Some(value) => backend.put(device_id, setting.key, value),
            None => backend.delete(device_id, setting.key),
        };
        if let Err(error) = result {
            first_error.get_or_insert_with(|| {
                format!(
                    "failed to restore Android setting '{}': {error}",
                    setting.key
                )
            });
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn normalize_device_id(device_id: &str) -> Result<String, String> {
    let device_id = device_id.trim();
    if device_id.is_empty() {
        return Err("Android input overlay guard requires a non-empty device id".to_string());
    }
    Ok(device_id.to_string())
}

fn parse_settings_value(output: &str) -> Option<String> {
    let value = output.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("null") {
        None
    } else {
        Some(value.to_string())
    }
}

fn run_adb_text(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("adb");
    configure_hidden_command(&mut command);
    let output = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("failed to run adb: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "adb {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn configure_hidden_command(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeSettingsBackend {
        values: HashMap<(String, String), Option<String>>,
        writes: Vec<String>,
    }

    impl FakeSettingsBackend {
        fn set(&mut self, device_id: &str, key: &str, value: Option<&str>) {
            self.values.insert(
                (device_id.to_string(), key.to_string()),
                value.map(str::to_string),
            );
        }
    }

    impl SettingsBackend for FakeSettingsBackend {
        fn get(&mut self, device_id: &str, key: &str) -> Result<Option<String>, String> {
            Ok(self
                .values
                .get(&(device_id.to_string(), key.to_string()))
                .cloned()
                .flatten())
        }

        fn put(&mut self, device_id: &str, key: &str, value: &str) -> Result<(), String> {
            self.writes.push(format!("put:{device_id}:{key}:{value}"));
            self.values.insert(
                (device_id.to_string(), key.to_string()),
                Some(value.to_string()),
            );
            Ok(())
        }

        fn delete(&mut self, device_id: &str, key: &str) -> Result<(), String> {
            self.writes.push(format!("delete:{device_id}:{key}"));
            self.values
                .insert((device_id.to_string(), key.to_string()), None);
            Ok(())
        }
    }

    #[test]
    fn parses_settings_get_null_as_absent() {
        assert_eq!(parse_settings_value("null\r\n"), None);
        assert_eq!(parse_settings_value("\r\n"), None);
        assert_eq!(parse_settings_value("1\r\n"), Some("1".to_string()));
    }

    #[test]
    fn disables_and_restores_original_android_input_overlay_settings() {
        let device_id = "unit-android-overlay-restore";
        let mut backend = FakeSettingsBackend::default();
        backend.set(device_id, "show_touches", Some("1"));
        backend.set(device_id, "pointer_location", None);

        let mut guard =
            AndroidInputOverlayGuard::disable_with_backend(device_id, &mut backend).unwrap();

        assert_eq!(
            backend
                .values
                .get(&(device_id.to_string(), "show_touches".to_string())),
            Some(&Some("0".to_string()))
        );
        assert_eq!(
            backend
                .values
                .get(&(device_id.to_string(), "pointer_location".to_string())),
            Some(&Some("0".to_string()))
        );

        guard.restore_with_backend(&mut backend).unwrap();

        assert_eq!(
            backend
                .values
                .get(&(device_id.to_string(), "show_touches".to_string())),
            Some(&Some("1".to_string()))
        );
        assert_eq!(
            backend
                .values
                .get(&(device_id.to_string(), "pointer_location".to_string())),
            Some(&None)
        );
    }

    #[test]
    fn nested_guards_restore_only_after_last_release() {
        let device_id = "unit-android-overlay-nested";
        let mut backend = FakeSettingsBackend::default();
        backend.set(device_id, "show_touches", Some("1"));
        backend.set(device_id, "pointer_location", Some("1"));

        let mut first =
            AndroidInputOverlayGuard::disable_with_backend(device_id, &mut backend).unwrap();
        let mut second =
            AndroidInputOverlayGuard::disable_with_backend(device_id, &mut backend).unwrap();

        second.restore_with_backend(&mut backend).unwrap();
        assert_eq!(
            backend
                .values
                .get(&(device_id.to_string(), "show_touches".to_string())),
            Some(&Some("0".to_string()))
        );

        first.restore_with_backend(&mut backend).unwrap();
        assert_eq!(
            backend
                .values
                .get(&(device_id.to_string(), "show_touches".to_string())),
            Some(&Some("1".to_string()))
        );
        assert_eq!(
            backend
                .values
                .get(&(device_id.to_string(), "pointer_location".to_string())),
            Some(&Some("1".to_string()))
        );
    }
}
