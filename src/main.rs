#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod hid_mouse;

#[cfg(windows)]
mod windows_impl {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;
    use windows::{
        Win32::Foundation::*, Win32::Graphics::Direct3D::*, Win32::Graphics::Direct3D11::*,
        Win32::Graphics::Dxgi::Common::*, Win32::Graphics::Dxgi::*, Win32::Graphics::Gdi::*,
        Win32::System::LibraryLoader::*, Win32::UI::WindowsAndMessaging::*, core::*,
    };
    use crate::hid_mouse;

    static EVENT_TX: Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>> = Mutex::new(None);
    static START_TIME: Mutex<Option<Instant>> = Mutex::new(None);
    static LAST_DISPLAY_NS: AtomicU64 = AtomicU64::new(0);

    pub fn main() -> Result<()> {
        unsafe {
            let result = main_internal();
            if let Err(e) = &result {
                let error_msg = format!("jerkmon failed to start:\n\n{e}");
                let wide_msg: Vec<u16> = error_msg.encode_utf16().chain(std::iter::once(0)).collect();
                MessageBoxW(
                    None,
                    PCWSTR::from_raw(wide_msg.as_ptr()),
                    w!("jerkmon error"),
                    MB_OK | MB_ICONERROR,
                );
            }
            result
        }
    }

    fn main_internal() -> Result<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        *EVENT_TX.lock().unwrap() = Some(tx.clone());
        let start_time = Instant::now();
        *START_TIME.lock().unwrap() = Some(start_time);

        // Start HID mouse thread
        let hid_tx = tx.clone();
        let hid_start = start_time;
        std::thread::spawn(move || {
            match hid_mouse::HidMouse::new(hid_tx, hid_start) {
                Ok(hid) => {
                    hid.run();
                }
                Err(e) => {
                    unsafe {
                        let error_msg = format!("Failed to initialize HID mouse:\n\n{e:?}\n\nMouse tracking will not be available.");
                        let wide_msg: Vec<u16> = error_msg.encode_utf16().chain(std::iter::once(0)).collect();
                        MessageBoxW(
                            None,
                            PCWSTR::from_raw(wide_msg.as_ptr()),
                            w!("HID Mouse Error"),
                            MB_OK | MB_ICONWARNING,
                        );
                    }
                }
            }
        });

        // Start WebSocket server in a separate thread
        let _ = std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    unsafe {
                        let error_msg = format!("Failed to create Tokio runtime:\n\n{e}");
                        let wide_msg: Vec<u16> =
                            error_msg.encode_utf16().chain(std::iter::once(0)).collect();
                        MessageBoxW(
                            None,
                            PCWSTR::from_raw(wide_msg.as_ptr()),
                            w!("WebSocket Server Error"),
                            MB_OK | MB_ICONERROR,
                        );
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
                unsafe {
                    let error_msg = format!("WebSocket server thread panicked:\n\n{e:?}");
                    let wide_msg: Vec<u16> =
                        error_msg.encode_utf16().chain(std::iter::once(0)).collect();
                    MessageBoxW(
                        None,
                        PCWSTR::from_raw(wide_msg.as_ptr()),
                        w!("WebSocket Server Panic"),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
        });

        // Create window on main thread
        create_window()
    }

    async fn websocket_server(rx: std::sync::mpsc::Receiver<Vec<u8>>) {
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

        let listener = match TcpListener::bind("127.0.0.1:12345").await {
            Ok(l) => l,
            Err(e) => {
                unsafe {
                    let error_msg = format!(
                        "Failed to bind WebSocket server to port 12345:\n\n{e}\n\nAnother instance might be running."
                    );
                    let wide_msg: Vec<u16> =
                        error_msg.encode_utf16().chain(std::iter::once(0)).collect();
                    MessageBoxW(
                        None,
                        PCWSTR::from_raw(wide_msg.as_ptr()),
                        w!("WebSocket Server Error"),
                        MB_OK | MB_ICONERROR,
                    );
                }
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
                hInstance: HINSTANCE(instance.0),
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
                100,
                100,
                None,
                None,
                Some(instance.into()),
                None,
            )?;

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);

            // Verify window is actually visible
            if !IsWindowVisible(hwnd).as_bool() {
                let error_msg = "Window was created but is not visible!";
                let wide_msg: Vec<u16> = error_msg.encode_utf16().chain(std::iter::once(0)).collect();
                MessageBoxW(
                    Some(hwnd),
                    PCWSTR::from_raw(wide_msg.as_ptr()),
                    w!("Window Visibility Error"),
                    MB_OK | MB_ICONERROR,
                );
            }


            // Initialize Direct3D
            let (_device, swap_chain, context, rtv) = match init_d3d(hwnd) {
                Ok(resources) => resources,
                Err(e) => {
                    let error_msg = format!(
                        "Direct3D initialization failed:\n\n{e}\n\nThe application cannot continue."
                    );
                    let wide_msg: Vec<u16> =
                        error_msg.encode_utf16().chain(std::iter::once(0)).collect();
                    MessageBoxW(
                        Some(hwnd),
                        PCWSTR::from_raw(wide_msg.as_ptr()),
                        w!("Graphics Initialization Error"),
                        MB_OK | MB_ICONERROR,
                    );
                    return Err(e);
                }
            };

            // Store D3D resources in static variables for the render thread
            static D3D_RESOURCES: Mutex<
                Option<(ID3D11DeviceContext, ID3D11RenderTargetView, IDXGISwapChain)>,
            > = Mutex::new(None);
            *D3D_RESOURCES.lock().unwrap() = Some((context, rtv, swap_chain));

            // Start render thread for vsync
            let render_hwnd = hwnd.0 as isize;
            std::thread::spawn(move || {
                while IsWindow(Some(HWND(render_hwnd as *mut _))).as_bool() {
                    if let Ok(resources) = D3D_RESOURCES.lock() {
                        if let Some((ref context, ref rtv, ref swap_chain)) = *resources {
                            render_frame(context, rtv, swap_chain);
                        }
                    }
                }
            });

            let mut msg = MSG::default();
            let mut msg_count = 0;
            loop {
                let result = GetMessageW(&mut msg, None, 0, 0);
                if result.0 == -1 {
                    // Error occurred
                    let err = Error::from_win32();
                    let error_msg = format!("GetMessageW failed: {err:?}");
                    let wide_msg: Vec<u16> =
                        error_msg.encode_utf16().chain(std::iter::once(0)).collect();
                    MessageBoxW(
                        Some(hwnd),
                        PCWSTR::from_raw(wide_msg.as_ptr()),
                        w!("Message Loop Error"),
                        MB_OK | MB_ICONERROR,
                    );
                    return Err(err);
                } else if result.0 == 0 {
                    // WM_QUIT received
                    break;
                }

                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                msg_count += 1;
            }

            if msg_count < 5 {
                let error_msg = format!(
                    "Window closed after only {msg_count} messages. The window may have closed immediately."
                );
                let wide_msg: Vec<u16> = error_msg.encode_utf16().chain(std::iter::once(0)).collect();
                MessageBoxW(
                    None,
                    PCWSTR::from_raw(wide_msg.as_ptr()),
                    w!("Early Exit Warning"),
                    MB_OK | MB_ICONWARNING,
                );
            }

            Ok(())
        }
    }

    extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                WM_DESTROY => {
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
    }

    fn send_display_event() {
        if let Ok(tx) = EVENT_TX.lock() {
            if let Some(tx) = tx.as_ref() {
                if let Ok(start_guard) = START_TIME.lock() {
                    if let Some(start) = *start_guard {
                        let now_ns = Instant::now().duration_since(start).as_nanos() as u64;
                        let last_ns = LAST_DISPLAY_NS.swap(now_ns, Ordering::Relaxed);
                        let delta_ns = now_ns.saturating_sub(last_ns);

                        let mut buf = vec![0x00]; // Display event type
                        buf.extend_from_slice(&delta_ns.to_be_bytes());
                        let _ = tx.send(buf);
                    }
                }
            }
        }
    }

    unsafe fn init_d3d(
        hwnd: HWND,
    ) -> Result<(
        ID3D11Device,
        IDXGISwapChain,
        ID3D11DeviceContext,
        ID3D11RenderTargetView,
    )> {
        // Get window size
        let mut rect = RECT::default();
        unsafe { _ = GetClientRect(hwnd, &mut rect) };
        let width = (rect.right - rect.left) as u32;
        let height = (rect.bottom - rect.top) as u32;

        let swap_chain_desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: width,
                Height: height,
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

        unsafe {
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
        }

        // Get the unwrapped values
        let swap_chain = swap_chain.ok_or_else(Error::from_win32)?;
        let device = device.ok_or_else(Error::from_win32)?;
        let context = context.ok_or_else(Error::from_win32)?;

        // Create render target view
        let back_buffer: ID3D11Texture2D = unsafe { swap_chain.GetBuffer(0)? };
        let mut render_target_view: Option<ID3D11RenderTargetView> = None;
        unsafe {
            device.CreateRenderTargetView(&back_buffer, None, Some(&mut render_target_view))?;
        }

        let rtv = render_target_view.ok_or_else(Error::from_win32)?;

        Ok((device, swap_chain, context, rtv))
    }

    unsafe fn render_frame(
        context: &ID3D11DeviceContext,
        rtv: &ID3D11RenderTargetView,
        swap_chain: &IDXGISwapChain,
    ) {
        let clear_color = [0.1f32, 0.2f32, 0.3f32, 1.0f32];
        unsafe {
            context.ClearRenderTargetView(rtv, &clear_color);
        }
        send_display_event();
        unsafe {
            let _ = swap_chain.Present(1, DXGI_PRESENT(0));
        }
    }
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    windows_impl::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This application only runs on Windows");
    std::process::exit(1);
}