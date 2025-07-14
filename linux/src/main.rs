#![cfg(target_os = "linux")]

//! Linux implementation of jerkmon - a high-performance input and display event monitor.
//!
//! This program captures raw mouse input events via evdev and display refresh events
//! via Wayland frame callbacks, then streams them over WebSocket in a binary protocol.

mod evdev;
mod utils;
mod wayland;
mod websocket;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Event type identifier for display refresh events
const EVENT_TYPE_DISPLAY: u8 = 0x00;

/// Event type identifier for mouse movement events  
const EVENT_TYPE_MOUSE: u8 = 0x04;

/// WebSocket server bind address
const WS_BIND_ADDR: &str = "127.0.0.1:12345";

static EVENT_TX: OnceLock<crossbeam_channel::Sender<Vec<u8>>> = OnceLock::new();
static START_TIME: OnceLock<Instant> = OnceLock::new();
static LAST_DISPLAY_NS: AtomicU64 = AtomicU64::new(0);
static LAST_MOUSE_NS: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicBool = AtomicBool::new(true);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set high process priority
    set_high_priority();

    // Initialize global state
    let (tx, rx) = crossbeam_channel::unbounded();
    EVENT_TX
        .set(tx)
        .map_err(|_| "EVENT_TX already initialized")?;
    START_TIME
        .set(Instant::now())
        .map_err(|_| "START_TIME already initialized")?;

    // Start WebSocket server
    let ws_handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
        rt.block_on(async {
            let server_handle = tokio::spawn(websocket::run_server(rx, WS_BIND_ADDR));

            // Check for shutdown signal periodically
            while RUNNING.load(Ordering::Relaxed) {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }

            // Abort the server task when shutting down
            server_handle.abort();
            let _ = server_handle.await;
        });
    });

    let wayland_handle = std::thread::spawn(|| {
        let _ = wayland::monitor_wayland_frames();
    });

    let evdev_handles = evdev::start_monitors();

    ctrlc::set_handler(|| {
        RUNNING.store(false, Ordering::Relaxed);
    })?;

    for handle in evdev_handles {
        let _ = handle.join();
    }
    let _ = wayland_handle.join();
    let _ = ws_handle.join();

    Ok(())
}

pub fn send_event(event_type: u8, delta_ns: u64, data: Option<(f32, f32)>) {
    if let Some(tx) = EVENT_TX.get() {
        let mut buf = vec![event_type];
        buf.extend_from_slice(&delta_ns.to_be_bytes());

        if let Some((x, y)) = data {
            buf.extend_from_slice(&x.to_be_bytes());
            buf.extend_from_slice(&y.to_be_bytes());
        }

        let _ = tx.send(buf);
    }
}

fn set_high_priority() {
    unsafe {
        // Try to set nice value to -20 (highest priority)
        libc::setpriority(libc::PRIO_PROCESS, 0, -20);

        // Also try to set real-time scheduling
        let param = libc::sched_param { sched_priority: 1 };
        libc::sched_setscheduler(0, libc::SCHED_FIFO, &param);
    }
}
