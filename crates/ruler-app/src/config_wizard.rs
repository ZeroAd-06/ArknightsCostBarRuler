use std::sync::Arc;

use ruler_core::RulerConfig;

use crate::{i18n::I18n, resources::ResourceLocator, worker::SharedAppState};

pub fn run_config_wizard(
    _resources: &ResourceLocator,
    i18n: &I18n,
    state: Arc<SharedAppState>,
    previous_config: Option<&RulerConfig>,
    debug: bool,
) -> Option<RulerConfig> {
    platform::run_config_wizard(i18n, state, previous_config, debug)
}

#[cfg(not(windows))]
mod platform {
    use ruler_core::RulerConfig;
    use std::sync::Arc;

    use crate::{i18n::I18n, worker::SharedAppState};

    pub fn run_config_wizard(
        _: &I18n,
        _: Arc<SharedAppState>,
        _: Option<&RulerConfig>,
        _debug: bool,
    ) -> Option<RulerConfig> {
        None
    }
}

#[cfg(windows)]
mod platform;
