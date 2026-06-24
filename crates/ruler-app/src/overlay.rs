use std::{
    fmt,
    sync::{mpsc::Sender, Arc},
};

use crate::{commands::UiCommand, i18n::I18n, icons::IconSet, worker::SharedAppState};

/// Initial overlay window placement, sourced from the persisted config.
#[derive(Clone, Copy, Debug, Default)]
pub struct OverlayPlacement {
    pub pos: Option<(i32, i32)>,
    pub scale_mult: f32,
}

impl OverlayPlacement {
    #[must_use]
    pub fn scale_or_default(self) -> f32 {
        if self.scale_mult > 0.1 {
            self.scale_mult
        } else {
            1.0
        }
    }
}

pub struct OverlayRuntime {
    state: Arc<SharedAppState>,
    command_tx: Sender<UiCommand>,
    i18n: Arc<I18n>,
    icons: Arc<IconSet>,
    placement: OverlayPlacement,
}

impl fmt::Debug for OverlayRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OverlayRuntime")
            .field("state", &self.state.snapshot())
            .finish_non_exhaustive()
    }
}

impl OverlayRuntime {
    #[must_use]
    pub fn new(
        state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
        i18n: Arc<I18n>,
        icons: Arc<IconSet>,
        placement: OverlayPlacement,
    ) -> Self {
        Self {
            state,
            command_tx,
            i18n,
            icons,
            placement,
        }
    }

    #[must_use]
    pub fn startup_note(&self) -> String {
        let snapshot = self.state.snapshot();
        format!(
            "slint overlay runtime registered with mode={:?}",
            snapshot.ui.mode
        )
    }

    pub fn run(&self) -> Result<(), OverlayError> {
        platform::run(
            Arc::clone(&self.state),
            self.command_tx.clone(),
            Arc::clone(&self.i18n),
            Arc::clone(&self.icons),
            self.placement,
        )
    }
}

#[derive(Debug)]
pub struct OverlayError {
    message: String,
}

impl OverlayError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OverlayError {}

#[cfg(not(windows))]
mod platform {
    use std::sync::{mpsc::Sender, Arc};

    use crate::{commands::UiCommand, i18n::I18n, icons::IconSet, worker::SharedAppState};

    use super::OverlayError;

    pub fn run(
        _: Arc<SharedAppState>,
        _: Sender<UiCommand>,
        _: Arc<I18n>,
        _: Arc<IconSet>,
        _: super::OverlayPlacement,
    ) -> Result<(), OverlayError> {
        Err(OverlayError::new(
            "native overlay window is currently implemented for Windows only",
        ))
    }
}

#[cfg(windows)]
mod platform;
