use std::{collections::HashMap, fs};

use crate::resources::ResourceLocator;

#[derive(Clone, Debug)]
pub struct I18n {
    locale: String,
    values: HashMap<String, String>,
}

impl I18n {
    #[must_use]
    pub fn load(resources: &ResourceLocator, preferred_locale: Option<&str>) -> Self {
        let locale = preferred_locale
            .map(normalize_locale)
            .unwrap_or_else(detect_locale);
        let mut values = load_locale_file(resources, locale);
        let active_locale = if values.is_empty() && locale != "zh_CN" {
            values = load_locale_file(resources, "zh_CN");
            "zh_CN"
        } else {
            locale
        };

        Self {
            locale: active_locale.to_string(),
            values,
        }
    }

    #[must_use]
    pub fn locale(&self) -> &str {
        &self.locale
    }

    #[must_use]
    pub fn tr(&self, key: &str) -> String {
        self.values
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_string())
    }

    #[must_use]
    pub fn tr_with(&self, key: &str, replacements: &[(&str, String)]) -> String {
        let mut text = self.tr(key);
        for (name, value) in replacements {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }
}

fn load_locale_file(resources: &ResourceLocator, locale: &str) -> HashMap<String, String> {
    let path = resources.locale_path(locale);
    match fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|error| {
            log::warn!("failed to parse locale '{}': {error}", path.display());
            HashMap::new()
        }),
        Err(error) => {
            log::warn!("failed to read locale '{}': {error}", path.display());
            HashMap::new()
        }
    }
}

fn detect_locale() -> &'static str {
    let lang = std::env::var("LANG")
        .or_else(|_| std::env::var("LC_ALL"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    if lang.contains("en") {
        "en_US"
    } else {
        "zh_CN"
    }
}

fn normalize_locale(locale: &str) -> &'static str {
    if locale.eq_ignore_ascii_case("en") || locale.eq_ignore_ascii_case("en_US") {
        "en_US"
    } else {
        "zh_CN"
    }
}
