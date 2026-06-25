use ruler_core::RulerConfig;

use crate::{i18n::I18n, resources::ResourceLocator};

pub fn run_config_wizard(
    _resources: &ResourceLocator,
    i18n: &I18n,
    previous_config: Option<&RulerConfig>,
    debug: bool,
) -> Option<RulerConfig> {
    platform::run_config_wizard(i18n, previous_config, debug)
}

#[cfg(not(windows))]
mod platform {
    use ruler_core::RulerConfig;

    use crate::i18n::I18n;

    pub fn run_config_wizard(
        _: &I18n,
        _: Option<&RulerConfig>,
        _debug: bool,
    ) -> Option<RulerConfig> {
        None
    }
}

#[cfg(windows)]
mod platform;
