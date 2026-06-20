use std::{
    env,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct ResourceLocator {
    project_root: PathBuf,
    exe_dir: Option<PathBuf>,
    config_path: PathBuf,
    calibration_dir: PathBuf,
    data_dir: PathBuf,
    log_dir_override: Option<PathBuf>,
}

impl ResourceLocator {
    #[must_use]
    pub fn new() -> Self {
        let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let project_root = manifest_root.canonicalize().unwrap_or(manifest_root);
        let exe_dir = env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.to_path_buf()));
        let working_dir = env::current_dir()
            .unwrap_or_else(|_| exe_dir.clone().unwrap_or_else(|| project_root.clone()));
        let config_dir =
            env_path("ARKNIGHTS_RULER_CONFIG_DIR").unwrap_or_else(|| working_dir.clone());
        let config_path = env_path("ARKNIGHTS_RULER_CONFIG_PATH")
            .unwrap_or_else(|| config_dir.join("config.json"));
        let config_root = path_parent(&config_path).unwrap_or_else(|| config_dir.clone());
        let calibration_dir = env_path("ARKNIGHTS_RULER_CALIBRATION_DIR")
            .unwrap_or_else(|| config_root.join("calibration"));
        let data_dir = env_path("ARKNIGHTS_RULER_DATA_DIR").unwrap_or_else(|| config_root.clone());
        let log_dir_override = env_path("ARKNIGHTS_RULER_LOG_DIR")
            .or_else(|| env_path("ARKNIGHTS_RULER_RECORDINGS_DIR"));

        Self {
            project_root,
            exe_dir,
            config_path,
            calibration_dir,
            data_dir,
            log_dir_override,
        }
    }

    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.config_path.clone()
    }

    #[must_use]
    pub fn calibration_dir(&self) -> PathBuf {
        self.calibration_dir.clone()
    }

    #[must_use]
    pub fn log_root_dir(&self, configured: Option<&str>) -> PathBuf {
        if let Some(configured) = configured.map(str::trim).filter(|value| !value.is_empty()) {
            return self.resolve_data_path(configured);
        }

        self.log_dir_override
            .clone()
            .unwrap_or_else(|| self.data_dir.join("log"))
    }

    #[must_use]
    pub fn icon_path(&self, name: &str) -> PathBuf {
        self.first_existing(&[
            self.project_root.join("icons").join(name),
            self.exe_relative(&["icons", name]),
            self.exe_relative(&["_internal", "icons", name]),
        ])
        .unwrap_or_else(|| self.project_root.join("icons").join(name))
    }

    #[must_use]
    pub fn locale_path(&self, locale: &str) -> PathBuf {
        let filename = format!("{locale}.json");
        self.first_existing(&[
            self.project_root
                .join("ruler")
                .join("locales")
                .join(&filename),
            self.exe_relative(&["ruler", "locales", &filename]),
            self.exe_relative(&["locales", &filename]),
            self.exe_relative(&["_internal", "ruler", "locales", &filename]),
        ])
        .unwrap_or_else(|| {
            self.project_root
                .join("ruler")
                .join("locales")
                .join(filename)
        })
    }

    fn exe_relative(&self, parts: &[&str]) -> PathBuf {
        let mut path = self
            .exe_dir
            .clone()
            .unwrap_or_else(|| self.project_root.clone());
        for part in parts {
            path.push(part);
        }
        path
    }

    fn first_existing(&self, candidates: &[PathBuf]) -> Option<PathBuf> {
        candidates.iter().find(|path| path.exists()).cloned()
    }

    fn resolve_data_path(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = path.as_ref();
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.data_dir.join(path)
        }
    }
}

impl Default for ResourceLocator {
    fn default() -> Self {
        Self::new()
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    let value = env::var_os(key)?;
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn path_parent(path: &Path) -> Option<PathBuf> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::ResourceLocator;
    use std::path::PathBuf;

    fn locator_for_test() -> ResourceLocator {
        ResourceLocator {
            project_root: PathBuf::from("C:/repo"),
            exe_dir: Some(PathBuf::from("C:/dist")),
            config_path: PathBuf::from("C:/state/config.json"),
            calibration_dir: PathBuf::from("C:/state/calibration"),
            data_dir: PathBuf::from("C:/data"),
            log_dir_override: None,
        }
    }

    #[test]
    fn config_and_calibration_paths_use_config_root() {
        let locator = locator_for_test();

        assert_eq!(locator.config_path(), PathBuf::from("C:/state/config.json"));
        assert_eq!(
            locator.calibration_dir(),
            PathBuf::from("C:/state/calibration")
        );
    }

    #[test]
    fn log_root_defaults_under_data_root() {
        let locator = locator_for_test();

        assert_eq!(locator.log_root_dir(None), PathBuf::from("C:/data/log"));
    }

    #[test]
    fn relative_log_root_resolves_under_data_root() {
        let locator = locator_for_test();

        assert_eq!(
            locator.log_root_dir(Some("captures")),
            PathBuf::from("C:/data/captures")
        );
    }

    #[cfg(windows)]
    #[test]
    fn absolute_log_root_is_preserved() {
        let locator = locator_for_test();

        assert_eq!(
            locator.log_root_dir(Some("D:/captures")),
            PathBuf::from("D:/captures")
        );
    }
}
