#![cfg(windows)]
#![windows_subsystem = "windows"]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use windows::{
    Win32::Foundation::*,
    Win32::Graphics::{
        Direct3D::*,
        Direct3D11::*,
        Dxgi::{Common::*, *},
        Gdi::*,
    },
    Win32::System::LibraryLoader::*,
    Win32::UI::{Input::*, WindowsAndMessaging::*},
    core::*,
};

// Event types
const EVENT_TYPE_DISPLAY: u8 = 0x00;
const EVENT_TYPE_MOUSE: u8 = 0x04;

// Window constants
const WINDOW_WIDTH: i32 = 100;
const WINDOW_HEIGHT: i32 = 100;

// HID constants
const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_GENERIC_MOUSE: u16 = 0x02;

// WebSocket server address
const WS_BIND_ADDR: &str = "127.0.0.1:12345";

// Polling interval for raw input
const RAW_INPUT_POLL_INTERVAL: Duration = Duration::from_micros(10);

// Global state
static EVENT_TX: OnceLock<crossbeam_channel::Sender<Vec<u8>>> = OnceLock::new();
static START_TIME: OnceLock<Instant> = OnceLock::new();
static LAST_DISPLAY_NS: AtomicU64 = AtomicU64::new(0);
static LAST_MOUSE_NS: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicBool = AtomicBool::new(true);

fn main() -> Result<()> {
    let result = main_internal();
    if let Err(e) = &result {
        show_error_dialog(None, "jerkmon failed to start", &format!("{e}"));
    }
    result
}

fn show_error_dialog(hwnd: Option<HWND>, title: &str, message: &str) {
    unsafe {
        let wide_title = encode_wide(title);
        let wide_msg = encode_wide(message);
        MessageBoxW(
            hwnd,
            PCWSTR::from_raw(wide_msg.as_ptr()),
            PCWSTR::from_raw(wide_title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn encode_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main_internal() -> Result<()> {
    let (tx, rx) = crossbeam_channel::unbounded();
    EVENT_TX.set(tx).expect("EVENT_TX already initialized");
    START_TIME
        .set(Instant::now())
        .expect("START_TIME already initialized");

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                show_error_dialog(
                    None,
                    "WebSocket Server Error",
                    &format!("Failed to create Tokio runtime:\n\n{e}"),
                );
                unsafe {
                    PostQuitMessage(0);
                }
                return;
            }
        };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rt.block_on(async {
                websocket_server(rx).await;
            });
        }));

        if let Err(e) = result {
            show_error_dialog(
                None,
                "WebSocket Server Panic",
                &format!("WebSocket server thread panicked:\n\n{e:?}"),
            );
        }
    });

    // Create window on main thread
    create_window()
}

/// WebSocket server that broadcasts events to all connected clients.
async fn websocket_server(rx: crossbeam_channel::Receiver<Vec<u8>>) {
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    let (tx, mut rx_async) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(event) = rx.recv() {
            let _ = tx.send(event);
        }
    });

    let clients = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<
        tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    >::new()));
    let clients_clone = clients.clone();

    tokio::spawn(async move {
        while let Some(event) = rx_async.recv().await {
            let mut clients = clients_clone.lock().await;
            clients.retain(|tx| tx.send(event.clone()).is_ok());
        }
    });

    let listener = match TcpListener::bind(WS_BIND_ADDR).await {
        Ok(l) => l,
        Err(e) => {
            show_error_dialog(
                None,
                "WebSocket Server Error",
                &format!(
                    "Failed to bind WebSocket server to {}:\n\n{e}\n\nAnother instance might be running.",
                    WS_BIND_ADDR
                ),
            );
            return;
        }
    };

    while let Ok((stream, _)) = listener.accept().await {
        if let Ok(ws) = accept_async(stream).await {
            let (mut ws_tx, mut ws_rx) = ws.split();
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            clients.lock().await.push(event_tx);

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        Some(event) = event_rx.recv() => {
                            if ws_tx.send(Message::Binary(event.into())).await.is_err() {
                                break;
                            }
                        }
                        result = ws_rx.next() => {
                            match result {
                                Some(Ok(Message::Ping(data))) => {
                                    let _ = ws_tx.send(Message::Pong(data)).await;
                                }
                                Some(Ok(Message::Close(_))) => break,
                                Some(Err(_)) => break,
                                None => break,
                                _ => {}
                            }
                        }
                    }
                }
            });
        }
    }
}

fn create_window() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("jerkmon");

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            lpszClassName: class,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };

        let _ = RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            class,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            None,
        )?;

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        // Register for raw input from mouse devices
        let rid = RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: HID_USAGE_GENERIC_MOUSE,
            dwFlags: RIDEV_INPUTSINK, // Receive input even when not in foreground
            hwndTarget: hwnd,
        };
        RegisterRawInputDevices(&[rid], std::mem::size_of::<RAWINPUTDEVICE>() as u32)?;

        // Start a dedicated thread for processing raw input more frequently
        let polling_hwnd = hwnd.0 as isize;
        std::thread::spawn(move || {
            process_raw_input_continuously(HWND(polling_hwnd as *mut _));
        });

        // Initialize minimal DXGI for vblank monitoring
        match init_minimal_dxgi(hwnd) {
            Ok((swap_chain, output)) => {
                start_vblank_thread(swap_chain, output);
            }
            Err(e) => {
                show_error_dialog(
                    Some(hwnd),
                    "DXGI Initialization Error",
                    &format!("Failed to initialize DXGI for vblank monitoring:\n\n{e}"),
                );
                return Err(e);
            }
        }

        let mut msg = MSG::default();
        loop {
            let result = GetMessageW(&mut msg, None, 0, 0);
            if result.0 == -1 {
                // Error occurred
                let err = Error::from_win32();
                show_error_dialog(
                    Some(hwnd),
                    "Message Loop Error",
                    &format!("GetMessageW failed: {err:?}"),
                );
                return Err(err);
            } else if result.0 == 0 {
                // WM_QUIT received
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_INPUT => {
                // Process the message to keep them flowing
                process_single_raw_input(lparam);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
                RUNNING.store(false, Ordering::Relaxed);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Sends a display refresh event with timing information.
fn send_display_event() {
    if let Some(tx) = EVENT_TX.get() {
        if let Some(start) = START_TIME.get() {
            let now_ns = Instant::now().duration_since(*start).as_nanos() as u64;
            let last_ns = LAST_DISPLAY_NS.swap(now_ns, Ordering::Relaxed);
            let delta_ns = now_ns.saturating_sub(last_ns);

            let mut buf = vec![EVENT_TYPE_DISPLAY];
            buf.extend_from_slice(&delta_ns.to_be_bytes());

            // No frame statistics available without D3D
            buf.extend_from_slice(&0u32.to_be_bytes());
            buf.extend_from_slice(&0u32.to_be_bytes());
            buf.extend_from_slice(&0u32.to_be_bytes());

            let _ = tx.send(buf);
        }
    }
}

/// Initialize minimal DXGI swap chain (1x1 pixel) for vblank monitoring
fn init_minimal_dxgi(hwnd: HWND) -> Result<(IDXGISwapChain, IDXGIOutput)> {
    unsafe {
        // Create a minimal swap chain description (1x1 pixel)
        let swap_chain_desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: 1,
                Height: 1,
                RefreshRate: DXGI_RATIONAL {
                    Numerator: 0,
                    Denominator: 0,
                },
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                ScanlineOrdering: DXGI_MODE_SCANLINE_ORDER_UNSPECIFIED,
                Scaling: DXGI_MODE_SCALING_UNSPECIFIED,
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: hwnd,
            Windowed: BOOL::from(true),
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            Flags: 0,
        };

        let mut swap_chain: Option<IDXGISwapChain> = None;
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;

        // Create minimal D3D11 device and swap chain
        D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            None,
            D3D11_SDK_VERSION,
            Some(&swap_chain_desc),
            Some(&mut swap_chain),
            Some(&mut device),
            None,
            Some(&mut context),
        )?;

        let swap_chain = swap_chain.ok_or_else(Error::from_win32)?;

        // Get the output (monitor) for VBlank synchronization
        let output: IDXGIOutput = swap_chain.GetContainingOutput()?;

        Ok((swap_chain, output))
    }
}

fn start_vblank_thread(swap_chain: IDXGISwapChain, output: IDXGIOutput) {
    std::thread::spawn(move || {
        while RUNNING.load(Ordering::Relaxed) {
            // Wait for vertical blank using DXGI
            unsafe {
                let _ = output.WaitForVBlank();
            }
            send_display_event();

            // Present the swap chain to keep it alive
            unsafe {
                let _ = swap_chain.Present(0, DXGI_PRESENT(0));
            }
        }
    });
}

fn process_single_raw_input(lparam: LPARAM) {
    unsafe {
        let mut size = 0u32;
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut _),
            RID_INPUT,
            None,
            &mut size,
            std::mem::size_of::<RAWINPUTHEADER>() as u32,
        );

        if size > 0 {
            let mut buffer = vec![0u8; size as usize];
            let result = GetRawInputData(
                HRAWINPUT(lparam.0 as *mut _),
                RID_INPUT,
                Some(buffer.as_mut_ptr() as *mut _),
                &mut size,
                std::mem::size_of::<RAWINPUTHEADER>() as u32,
            );

            if result != u32::MAX {
                let raw_input = &*(buffer.as_ptr() as *const RAWINPUT);
                if raw_input.header.dwType == RIM_TYPEMOUSE.0 {
                    send_mouse_event(&raw_input.data.mouse);
                }
            }
        }
    }
}

fn send_mouse_event(mouse_data: &RAWMOUSE) {
    let dx = mouse_data.lLastX;
    let dy = mouse_data.lLastY;

    if dx != 0 || dy != 0 {
        if let Some(tx) = EVENT_TX.get() {
            if let Some(start) = START_TIME.get() {
                let now_ns = Instant::now().duration_since(*start).as_nanos() as u64;
                let last_ns = LAST_MOUSE_NS.swap(now_ns, Ordering::Relaxed);
                let delta_ns = now_ns.saturating_sub(last_ns);

                let mut buf = vec![EVENT_TYPE_MOUSE];
                buf.extend_from_slice(&delta_ns.to_be_bytes());
                buf.extend_from_slice(&(dx as f32).to_be_bytes());
                buf.extend_from_slice(&(dy as f32).to_be_bytes());

                let _ = tx.send(buf);
            }
        }
    }
}

fn process_raw_input_continuously(hwnd: HWND) {
    unsafe {
        while RUNNING.load(Ordering::Relaxed) {
            let mut msg = MSG::default();
            // Use None for hwnd to get all thread messages, not just window messages
            while PeekMessageW(&mut msg, None, WM_INPUT, WM_INPUT, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            std::thread::sleep(RAW_INPUT_POLL_INTERVAL);
        }
    }
}
