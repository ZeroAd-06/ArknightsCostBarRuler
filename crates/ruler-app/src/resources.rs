use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct ResourceLocator {
    project_root: PathBuf,
    exe_dir: Option<PathBuf>,
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

        Self {
            project_root,
            exe_dir,
        }
    }

    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.project_root.join("config.json")
    }

    #[must_use]
    pub fn calibration_dir(&self) -> PathBuf {
        self.project_root.join("calibration")
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
}

impl Default for ResourceLocator {
    fn default() -> Self {
        Self::new()
    }
}
