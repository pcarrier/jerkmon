//! WebSocket server module
//!
//! Provides WebSocket server functionality for streaming events to connected clients.
//! Handles client connections, broadcasts events to all connected clients, and manages
//! client lifecycle including ping/pong heartbeats.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::RUNNING;

pub async fn run_server(rx: crossbeam_channel::Receiver<Vec<u8>>, bind_addr: &str) {
    // Bridge crossbeam channel to tokio channel
    let (tx, rx_async) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(event) => {
                    let _ = tx.send(event);
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if !RUNNING.load(Ordering::Relaxed) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Client management
    let clients = Arc::new(Mutex::new(Vec::<UnboundedSender<Vec<u8>>>::new()));

    // Event broadcaster
    let clients_clone = clients.clone();
    tokio::spawn(async move {
        broadcast_events(rx_async, clients_clone).await;
    });

    // Accept connections
    if let Ok(listener) = TcpListener::bind(bind_addr).await {
        accept_connections(listener, clients).await;
    }
}

async fn broadcast_events(
    mut rx: UnboundedReceiver<Vec<u8>>,
    clients: Arc<Mutex<Vec<UnboundedSender<Vec<u8>>>>>,
) {
    while let Some(event) = rx.recv().await {
        let mut clients = clients.lock().await;
        clients.retain(|tx| tx.send(event.clone()).is_ok());
    }
}

async fn accept_connections(
    listener: TcpListener,
    clients: Arc<Mutex<Vec<UnboundedSender<Vec<u8>>>>>,
) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                if let Ok((stream, _addr)) = result {
                    if let Ok(ws) = accept_async(stream).await {
                        let (ws_tx, ws_rx) = ws.split();
                        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

                        clients.lock().await.push(event_tx);

                        tokio::spawn(handle_client(ws_tx, ws_rx, event_rx));
                    }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                if !RUNNING.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
}

async fn handle_client(
    mut ws_tx: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message,
    >,
    mut ws_rx: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    >,
    mut event_rx: UnboundedReceiver<Vec<u8>>,
) {
    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                if ws_tx.send(Message::Binary(event)).await.is_err() {
                    break;
                }
            }
            result = ws_rx.next() => {
                match result {
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws_tx.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    _ => {}
                }
            }
        }
    }
}
