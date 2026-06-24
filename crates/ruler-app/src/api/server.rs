use std::{
    collections::VecDeque,
    io::Write,
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use serde_json::Value;
use tungstenite::{accept, error::Error as WebSocketError, protocol::WebSocket, Message};

use super::response::{error_response, handle_client_text, snapshot_json};
use crate::{commands::UiCommand, worker::SharedAppState};

pub(super) fn spawn_api_thread(
    bind_address: String,
    state: Arc<SharedAppState>,
    command_tx: Sender<UiCommand>,
    running: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    match thread::Builder::new()
        .name("ruler-app-api".to_string())
        .spawn(move || run_api_server(bind_address, state, command_tx, running))
    {
        Ok(handle) => Some(handle),
        Err(error) => {
            log::error!("failed to start local API thread: {error}");
            None
        }
    }
}

fn run_api_server(
    bind_address: String,
    state: Arc<SharedAppState>,
    command_tx: Sender<UiCommand>,
    running: Arc<AtomicBool>,
) {
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
                            log::debug!(
                                "failed to respond to local HTTP API client {peer_addr}: {error}"
                            );
                        }
                    }
                    Err(error) => {
                        log::debug!("failed to classify local API client {peer_addr}: {error}");
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => {
                log::error!("local API accept loop failed: {error}");
                thread::sleep(Duration::from_millis(100));
            }
        }

        process_websocket_clients(&mut websocket_clients, &state, &command_tx);

        let payload = snapshot_json(&state);
        let should_broadcast = recent_payloads.back() != Some(&payload);
        if should_broadcast {
            broadcast_snapshot(&mut websocket_clients, &payload);
            remember_payload(&mut recent_payloads, payload);
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

fn process_websocket_clients(
    clients: &mut Vec<WebSocket<TcpStream>>,
    state: &SharedAppState,
    command_tx: &Sender<UiCommand>,
) {
    clients.retain_mut(|client| drain_client_messages(client, state, command_tx));
}

fn drain_client_messages(
    client: &mut WebSocket<TcpStream>,
    state: &SharedAppState,
    command_tx: &Sender<UiCommand>,
) -> bool {
    loop {
        match client.read() {
            Ok(Message::Text(text)) => {
                let response = handle_client_text(state, command_tx, text.as_str());
                if send_response(client, &response).is_err() {
                    return false;
                }
            }
            Ok(Message::Binary(_)) => {
                let response = error_response(
                    None,
                    "invalid_message",
                    "websocket requests must be UTF-8 JSON text".to_string(),
                );
                if send_response(client, &response).is_err() {
                    return false;
                }
            }
            Ok(Message::Close(_)) => return false,
            Ok(Message::Ping(bytes)) => {
                if client.send(Message::Pong(bytes)).is_err() {
                    return false;
                }
            }
            Ok(_) => {}
            Err(WebSocketError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return true;
            }
            Err(WebSocketError::AlreadyClosed) | Err(WebSocketError::ConnectionClosed) => {
                return false;
            }
            Err(error) => {
                log::debug!("dropping websocket API client after read failure: {error}");
                return false;
            }
        }
    }
}

fn send_response(client: &mut WebSocket<TcpStream>, response: &Value) -> tungstenite::Result<()> {
    client.send(Message::Text(response.to_string().into()))
}

fn broadcast_snapshot(clients: &mut Vec<WebSocket<TcpStream>>, payload: &str) {
    clients.retain_mut(
        |client| match client.send(Message::Text(payload.to_string().into())) {
            Ok(()) => true,
            Err(WebSocketError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {
                true
            }
            Err(error) => {
                log::debug!("dropping websocket API client after send failure: {error}");
                false
            }
        },
    );
}

fn remember_payload(recent_payloads: &mut VecDeque<String>, payload: String) {
    if recent_payloads.len() == recent_payloads.capacity() {
        recent_payloads.pop_front();
    }
    recent_payloads.push_back(payload);
}
