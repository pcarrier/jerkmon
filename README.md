# Jerkmon - Rust Port

A Windows application that monitors mouse and display events using raw input and broadcasts them via WebSocket.

## Building

```bash
cargo build --release
```

## Running

```bash
cargo run --release
```

Then open `viewer.html` in a web browser to view the events.

## Features

- Monitors mouse movement and display refresh events
- WebSocket server on localhost:12345
- Binary protocol: 1 byte event type + 8 bytes timestamp (big-endian nanoseconds)
- Event types:
  - 0x00: Display refresh
  - 0x02: Mouse movement