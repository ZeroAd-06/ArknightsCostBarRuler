use std::{
    collections::VecDeque,
    io::Write,
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use serde::Serialize;
use tungstenite::{
    accept,
    error::Error as WebSocketError,
    protocol::WebSocket,
    Message,
};

use crate::worker::SharedAppState;

const API_HOST: &str = "127.0.0.1";
const API_PORT: u16 = 2606;

#[derive(Debug)]
pub struct ApiRuntime {
    bind_address: String,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiPayload {
    is_running: bool,
    current_frame: Option<i32>,
    total_frames_in_cycle: i32,
    total_elapsed_frames: i32,
    active_profile: Option<String>,
}

impl ApiRuntime {
    #[must_use]
    pub fn new(state: Arc<SharedAppState>) -> Self {
        let bind_address = format!("{API_HOST}:{API_PORT}");
        let running = Arc::new(AtomicBool::new(true));
        let handle = spawn_api_thread(bind_address.clone(), state, Arc::clone(&running));

        Self {
            bind_address,
            running,
            handle,
        }
    }

    #[must_use]
    pub fn startup_note(&self) -> String {
        format!(
            "local API runtime serving JSON snapshots over WebSocket on ws://{}/ with simple HTTP snapshot fallback on http://{}/",
            self.bind_address,
            self.bind_address
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

fn spawn_api_thread(
    bind_address: String,
    state: Arc<SharedAppState>,
    running: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    match thread::Builder::new()
        .name("ruler-app-api".to_string())
        .spawn(move || run_api_server(bind_address, state, running))
    {
        Ok(handle) => Some(handle),
        Err(error) => {
            log::error!("failed to start local API thread: {error}");
            None
        }
    }
}

fn run_api_server(bind_address: String, state: Arc<SharedAppState>, running: Arc<AtomicBool>) {
    let listener = match TcpListener::bind(&bind_address) {
        Ok(listener) => listener,
        Err(error) => {
            log::error!("failed to bind local API listener on {bind_address}: {error}");
            return;
        }
    };

    if let Err(error) = listener.set_nonblocking(true) {
        log::error!("failed to set local API listener nonblocking mode: {error}");
        return;
    }

    log::info!("local API runtime listening on ws://{bind_address}/ with HTTP snapshot fallback");

    let mut websocket_clients = Vec::new();
    let mut recent_payloads = VecDeque::with_capacity(2);

    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, peer_addr)) => {
                if let Err(error) = stream.set_nonblocking(false) {
                    log::debug!("failed to configure local API client {peer_addr}: {error}");
                    continue;
                }

                match classify_connection(&mut stream) {
                    Ok(ConnectionKind::WebSocket) => match accept(stream) {
                        Ok(mut websocket) => {
                            if let Err(error) = websocket.get_mut().set_nonblocking(true) {
                                log::debug!(
                                    "failed to set websocket client {peer_addr} nonblocking mode: {error}"
                                );
                                continue;
                            }

                            let payload = snapshot_json(&state);
                            if let Err(error) = websocket.send(Message::Text(payload.clone())) {
                                log::debug!(
                                    "failed to send initial websocket snapshot to {peer_addr}: {error}"
                                );
                                continue;
                            }

                            websocket_clients.push(websocket);
                            remember_payload(&mut recent_payloads, payload);
                            log::debug!("accepted websocket API client {peer_addr}");
                        }
                        Err(error) => {
                            log::debug!("failed websocket handshake for {peer_addr}: {error}");
                        }
                    },
                    Ok(ConnectionKind::HttpSnapshot) => {
                        if let Err(error) = respond_with_snapshot(&mut stream, &state) {
                            log::debug!("failed to respond to local HTTP API client {peer_addr}: {error}");
                        }
                    }
                    Err(error) => {
                        log::debug!("failed to classify local API client {peer_addr}: {error}");
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                // no-op; broadcast pass below keeps the loop active
            }
            Err(error) => {
                log::error!("local API accept loop failed: {error}");
                thread::sleep(Duration::from_millis(100));
            }
        }

        let payload = snapshot_json(&state);
        let should_broadcast = recent_payloads.back() != Some(&payload);
        if should_broadcast {
            broadcast_snapshot(&mut websocket_clients, &payload);
            remember_payload(&mut recent_payloads, payload);
        } else {
            prune_closed_clients(&mut websocket_clients);
        }

        thread::sleep(Duration::from_millis(8));
    }
}

fn respond_with_snapshot(stream: &mut TcpStream, state: &SharedAppState) -> std::io::Result<()> {
    let json = snapshot_json(state).into_bytes();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        json.len()
    );

    stream.write_all(response.as_bytes())?;
    stream.write_all(&json)?;
    stream.flush()?;

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionKind {
    WebSocket,
    HttpSnapshot,
}

fn classify_connection(stream: &mut TcpStream) -> std::io::Result<ConnectionKind> {
    let mut buffer = [0_u8; 2048];
    let bytes_read = stream.peek(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    if request
        .lines()
        .any(|line| line.eq_ignore_ascii_case("Upgrade: websocket"))
    {
        Ok(ConnectionKind::WebSocket)
    } else {
        Ok(ConnectionKind::HttpSnapshot)
    }
}

fn snapshot_json(state: &SharedAppState) -> String {
    let snapshot = state.snapshot();
    let payload = ApiPayload {
        is_running: snapshot.api.is_running,
        current_frame: snapshot.api.current_frame,
        total_frames_in_cycle: snapshot.api.total_frames_in_cycle,
        total_elapsed_frames: snapshot.api.total_elapsed_frames,
        active_profile: snapshot.api.active_profile,
    };
    serde_json::to_string(&payload).unwrap_or_else(|error| {
        log::error!("failed to encode API payload: {error}");
        "{\"isRunning\":false,\"currentFrame\":null,\"totalFramesInCycle\":0,\"totalElapsedFrames\":0,\"activeProfile\":null}".to_string()
    })
}

fn broadcast_snapshot(clients: &mut Vec<WebSocket<TcpStream>>, payload: &str) {
    clients.retain_mut(|client| match client.send(Message::Text(payload.to_string().into())) {
        Ok(()) => true,
        Err(WebSocketError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(error) => {
            log::debug!("dropping websocket API client after send failure: {error}");
            false
        }
    });
}

fn prune_closed_clients(clients: &mut Vec<WebSocket<TcpStream>>) {
    clients.retain_mut(|client| match client.read() {
        Ok(Message::Close(_)) => false,
        Ok(_) => true,
        Err(WebSocketError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(WebSocketError::AlreadyClosed) | Err(WebSocketError::ConnectionClosed) => false,
        Err(error) => {
            log::debug!("dropping websocket API client after read failure: {error}");
            false
        }
    });
}

fn remember_payload(recent_payloads: &mut VecDeque<String>, payload: String) {
    if recent_payloads.len() == recent_payloads.capacity() {
        recent_payloads.pop_front();
    }
    recent_payloads.push_back(payload);
}
