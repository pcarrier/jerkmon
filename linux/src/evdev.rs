//! evdev mouse input monitoring module
//!
//! Monitors mouse devices via the evdev interface for movement events. This works better
//! than HIDRAW on modern Linux systems, especially under Wayland.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::mem;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{EVENT_TYPE_MOUSE, LAST_MOUSE_NS, RUNNING, START_TIME, send_event, utils};

const EV_REL: u16 = 0x02;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;

// Input event structure matching kernel's input_event
#[repr(C)]
struct InputEvent {
    tv_sec: i64,
    tv_usec: i64,
    type_: u16,
    code: u16,
    value: i32,
}

pub fn start_monitors() -> Vec<JoinHandle<()>> {
    let mut threads = Vec::new();
    let active_devices = Arc::new(Mutex::new(HashSet::new()));

    // Start monitoring existing devices
    let devices = find_mouse_devices();
    for device in devices {
        if let Ok(mut active) = active_devices.lock() {
            active.insert(device.clone());
        }

        let device_path = device.clone();
        let active_devices_clone = active_devices.clone();
        let handle = std::thread::spawn(move || {
            let _ = monitor_device(&device_path, active_devices_clone);
        });
        threads.push(handle);
    }

    // Start device discovery thread to watch for new devices
    let active_devices_clone = active_devices.clone();
    let discovery_handle = std::thread::spawn(move || {
        device_discovery_thread(active_devices_clone);
    });
    threads.push(discovery_handle);

    threads
}

fn find_mouse_devices() -> Vec<String> {
    let mut devices = Vec::new();

    // Read /proc/bus/input/devices to find mouse devices
    if let Ok(content) = std::fs::read_to_string("/proc/bus/input/devices") {
        let mut is_mouse = false;

        for line in content.lines() {
            if line.starts_with("I:") {
                // New device section
                is_mouse = false;
            } else if line.starts_with("N: Name=") {
                // Check if it's a mouse device
                let name = &line[8..].trim_matches('"');
                let name_lower = name.to_lowercase();
                if name_lower.contains("mouse")
                    || name_lower.contains("razer")
                    || name_lower.contains("logitech")
                {
                    is_mouse = true;
                }
            } else if line.starts_with("H: Handlers=") && is_mouse {
                // Extract event device
                let handlers = &line[12..];
                for handler in handlers.split_whitespace() {
                    if handler.starts_with("event") {
                        devices.push(format!("/dev/input/{}", handler));
                    }
                }
            }
        }
    }

    // Fallback: try common event devices if parsing fails
    if devices.is_empty() {
        for i in 0..32 {
            let path = format!("/dev/input/event{}", i);
            if std::path::Path::new(&path).exists() {
                // Try to open it to see if it's a mouse
                if let Ok(file) = File::open(&path) {
                    if is_mouse_device(file.as_raw_fd()) {
                        devices.push(path);
                    }
                }
            }
        }
    }

    devices
}

fn is_mouse_device(fd: i32) -> bool {
    // Check if device has REL_X and REL_Y capabilities
    let mut rel_bits = [0u8; 2]; // 2 bytes = 16 bits, enough for REL_X and REL_Y

    unsafe {
        const EVIOCGBIT_REL: u32 = 0x80044522; // _IOR('E', 0x22, char[2])
        let result = libc::ioctl(fd, EVIOCGBIT_REL as _, rel_bits.as_mut_ptr());

        if result >= 0 {
            // Check if REL_X (bit 0) and REL_Y (bit 1) are set
            return (rel_bits[0] & 0x03) == 0x03;
        }
    }

    false
}

fn monitor_device(device_path: &str, active_devices: Arc<Mutex<HashSet<String>>>) -> io::Result<()> {
    utils::set_thread_priority("evdev");

    let mut file = OpenOptions::new().read(true).open(device_path)?;

    let fd = file.as_raw_fd();

    let event_size = mem::size_of::<InputEvent>();
    let mut buffer = vec![0u8; event_size * 64]; // Read up to 64 events at once

    let result = (|| {
        while RUNNING.load(Ordering::Relaxed) {
        // Use select() to check if data is available with timeout
        let mut readfds = unsafe { mem::zeroed::<libc::fd_set>() };
        unsafe {
            libc::FD_ZERO(&mut readfds);
            libc::FD_SET(fd, &mut readfds);

            let mut timeout = libc::timeval {
                tv_sec: 0,
                tv_usec: 100_000, // 100ms timeout
            };

            let result = libc::select(
                fd + 1,
                &mut readfds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut timeout,
            );

            if result == -1 {
                return Err(io::Error::last_os_error());
            } else if result == 0 {
                // Timeout, check RUNNING flag and continue
                continue;
            }
            // Data is available, proceed with read
        }

        match file.read(&mut buffer) {
            Ok(n) if n >= event_size => {
                let event_count = n / event_size;
                let mut x_movement = 0i32;
                let mut y_movement = 0i32;
                let mut has_movement = false;

                for i in 0..event_count {
                    let offset = i * event_size;
                    let event_bytes = &buffer[offset..offset + event_size];

                    // Safe transmute since we know the alignment and size are correct
                    let event: InputEvent = unsafe {
                        std::ptr::read_unaligned(event_bytes.as_ptr() as *const InputEvent)
                    };

                    if event.type_ == EV_REL {
                        match event.code {
                            REL_X => {
                                x_movement += event.value;
                                has_movement = true;
                            }
                            REL_Y => {
                                y_movement += event.value;
                                has_movement = true;
                            }
                            _ => {}
                        }
                    }
                }

                // Send event if there was any movement in this read
                if has_movement {
                    send_mouse_event(x_movement as f32, y_movement as f32);
                }
            }
            Ok(_) => continue,
            Err(e) => return Err(e),
        }
    }

    Ok(())
    })();

    // Clean up device from active set when monitoring ends
    if let Ok(mut active) = active_devices.lock() {
        active.remove(device_path);
        println!("Mouse removed: {}", device_path);
    }

    result
}

fn send_mouse_event(x: f32, y: f32) {
    if let Some(start) = START_TIME.get() {
        let now_ns = Instant::now().duration_since(*start).as_nanos() as u64;
        let last_ns = LAST_MOUSE_NS.swap(now_ns, Ordering::Relaxed);
        let delta_ns = now_ns.saturating_sub(last_ns);

        send_event(EVENT_TYPE_MOUSE, delta_ns, Some((x, y)));
    }
}

fn device_discovery_thread(active_devices: Arc<Mutex<HashSet<String>>>) {
    // Use inotify to watch for new devices in /dev/input
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK) };
    if fd < 0 {
        eprintln!("Failed to initialize inotify for device monitoring");
        return;
    }

    // Watch /dev/input directory for new files
    let watch_path = b"/dev/input\0";
    let wd = unsafe {
        libc::inotify_add_watch(
            fd,
            watch_path.as_ptr() as *const libc::c_char,
            libc::IN_CREATE | libc::IN_ATTRIB | libc::IN_DELETE,
        )
    };

    if wd < 0 {
        eprintln!("Failed to add inotify watch for /dev/input");
        unsafe {
            libc::close(fd);
        }
        return;
    }

    let mut buffer = [0u8; 4096];

    while RUNNING.load(Ordering::Relaxed) {
        // Use select to wait for events with timeout
        let mut readfds = unsafe { mem::zeroed::<libc::fd_set>() };
        unsafe {
            libc::FD_ZERO(&mut readfds);
            libc::FD_SET(fd, &mut readfds);

            let mut timeout = libc::timeval {
                tv_sec: 1,
                tv_usec: 0,
            };

            let result = libc::select(
                fd + 1,
                &mut readfds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut timeout,
            );

            if result <= 0 {
                continue;
            }
        }

        // Read inotify events
        let bytes_read =
            unsafe { libc::read(fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len()) };

        if bytes_read <= 0 {
            continue;
        }

        // Process inotify events
        let mut offset = 0;
        while offset < bytes_read as usize {
            let event = unsafe { &*(buffer.as_ptr().add(offset) as *const libc::inotify_event) };

            if event.len > 0 {
                let name_ptr = unsafe {
                    buffer
                        .as_ptr()
                        .add(offset + mem::size_of::<libc::inotify_event>())
                };
                let name = unsafe { std::ffi::CStr::from_ptr(name_ptr as *const libc::c_char) };

                if let Ok(name_str) = name.to_str() {
                    if name_str.starts_with("event") {
                        let device_path = format!("/dev/input/{}", name_str);

                        // Check if this is a DELETE event
                        if event.mask & libc::IN_DELETE != 0 {
                            // Device was removed - the monitor thread will clean itself up
                            // We just note it here for logging
                            if let Ok(active) = active_devices.lock() {
                                if active.contains(&device_path) {
                                    // Monitor thread will handle the cleanup
                                }
                            }
                        } else if event.mask & (libc::IN_CREATE | libc::IN_ATTRIB) != 0 {
                            // Give the device a moment to settle
                            std::thread::sleep(Duration::from_millis(100));

                            // Check if it's a mouse device
                            if let Ok(file) = File::open(&device_path) {
                                if is_mouse_device(file.as_raw_fd()) {
                                    let mut should_monitor = false;

                                    // Check if we're already monitoring this device
                                    if let Ok(mut active) = active_devices.lock() {
                                        if !active.contains(&device_path) {
                                            active.insert(device_path.clone());
                                            should_monitor = true;
                                        }
                                    }

                                    if should_monitor {
                                        println!("New mouse detected: {}", device_path);
                                        let device_path_clone = device_path.clone();
                                        let active_devices_clone = active_devices.clone();
                                        std::thread::spawn(move || {
                                            let _ = monitor_device(&device_path_clone, active_devices_clone);
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }

            offset += mem::size_of::<libc::inotify_event>() + event.len as usize;
        }
    }

    unsafe {
        libc::close(fd);
    }
}
