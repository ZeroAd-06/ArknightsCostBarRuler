use std::{
    net::TcpStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread::JoinHandle,
};

use crate::{commands::UiCommand, worker::SharedAppState};

mod payload;
mod protocol;
mod response;
mod server;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

const API_HOST: &str = "127.0.0.1";
const API_PORT: u16 = 2606;

#[derive(Debug)]
pub struct ApiRuntime {
    bind_address: String,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ApiRuntime {
    #[must_use]
    pub fn new(state: Arc<SharedAppState>, command_tx: Sender<UiCommand>) -> Self {
        Self::new_for_bind_address(format!("{API_HOST}:{API_PORT}"), state, command_tx)
    }

    fn new_for_bind_address(
        bind_address: String,
        state: Arc<SharedAppState>,
        command_tx: Sender<UiCommand>,
    ) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let handle = server::spawn_api_thread(
            bind_address.clone(),
            state,
            command_tx,
            Arc::clone(&running),
        );

        Self {
            bind_address,
            running,
            handle,
        }
    }

    #[must_use]
    pub fn startup_note(&self) -> String {
        format!(
            "local API runtime serving JSON snapshots and interactive WebSocket commands on ws://{}/ with HTTP snapshot fallback on http://{}/",
            self.bind_address, self.bind_address
        )
    }
}

impl Drop for ApiRuntime {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);

        let _ = TcpStream::connect(&self.bind_address);

        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
