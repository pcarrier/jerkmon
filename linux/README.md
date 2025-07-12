# Jerkmon Linux Version

Linux implementation of jerkmon using HIDRAW for raw mouse input capture.

## Building

```bash
cargo build --release
```

## Running

The application requires access to hidraw devices and DRM devices, so you'll need to run with appropriate permissions:

```bash
sudo ./target/release/jerkmon
```

Or add your user to the `input` and `video` groups:

```bash
sudo usermod -a -G input,video $USER
# Log out and back in for changes to take effect
```

## Features

- Raw mouse input capture using HIDRAW devices
- Display refresh monitoring using DRM vblank events
- WebSocket server on `127.0.0.1:12345` for event streaming
- Binary protocol compatible with Windows version

## Event Protocol

Events are sent as binary messages:

### Display Event
- Byte 0: `0x00` (EVENT_TYPE_DISPLAY)
- Bytes 1-8: Delta time in nanoseconds (big-endian u64)

### Mouse Event  
- Byte 0: `0x04` (EVENT_TYPE_MOUSE)
- Bytes 1-8: Delta time in nanoseconds (big-endian u64)
- Bytes 9-12: X movement (big-endian f32)
- Bytes 13-16: Y movement (big-endian f32)

## Notes

- The Linux version uses HIDRAW to read raw HID reports from mouse devices
- Display refresh uses real DRM vblank events from `/dev/dri/card0`
- If DRM device cannot be opened or vblank monitoring fails, display events will not be reported
- Multiple hidraw devices are monitored simultaneously
- Ctrl+C for graceful shutdown