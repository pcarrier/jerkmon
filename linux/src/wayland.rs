//! Wayland frame monitoring module
//!
//! Creates a minimal Wayland window to receive frame callbacks for display timing.

use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::Ordering;
use std::time::Instant;

use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use crate::{EVENT_TYPE_DISPLAY, LAST_DISPLAY_NS, RUNNING, START_TIME, send_event, utils};

const WINDOW_WIDTH: i32 = 1;
const WINDOW_HEIGHT: i32 = 1;

struct AppData {
    compositor: Option<wl_compositor::WlCompositor>,
    surface: Option<wl_surface::WlSurface>,
    xdg_wm_base: Option<xdg_wm_base::XdgWmBase>,
    xdg_surface: Option<xdg_surface::XdgSurface>,
    xdg_toplevel: Option<xdg_toplevel::XdgToplevel>,
    shm: Option<wl_shm::WlShm>,
    buffer: Option<wl_buffer::WlBuffer>,
    configured: bool,
    frame_pending: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for AppData {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    let compositor =
                        registry.bind::<wl_compositor::WlCompositor, _, _>(name, 4, qh, ());
                    state.compositor = Some(compositor);
                }
                "xdg_wm_base" => {
                    let xdg_wm_base =
                        registry.bind::<xdg_wm_base::XdgWmBase, _, _>(name, 2, qh, ());
                    state.xdg_wm_base = Some(xdg_wm_base);
                }
                "wl_shm" => {
                    let shm = registry.bind::<wl_shm::WlShm, _, _>(name, 1, qh, ());
                    state.shm = Some(shm);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for AppData {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for AppData {
    fn event(
        state: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        _: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for AppData {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            // Capture timestamp immediately when frame callback is received to minimize jitter
            let timestamp = Instant::now();

            state.frame_pending = false;

            send_display_event(timestamp);

            // Request next frame callback if still running
            if RUNNING.load(Ordering::Relaxed) {
                if let Some(surface) = &state.surface {
                    surface.frame(qh, ());
                    state.frame_pending = true;
                    surface.commit();
                }
            }
        }
    }
}

pub fn monitor_wayland_frames() -> Result<(), Box<dyn std::error::Error>> {
    utils::set_thread_priority("Wayland");

    let conn = match Connection::connect_to_env() {
        Ok(conn) => conn,
        Err(_e) => {
            utils::wait_for_shutdown();
            return Ok(());
        }
    };

    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    let _registry = display.get_registry(&qh, ());

    let mut app_data = AppData {
        compositor: None,
        surface: None,
        xdg_wm_base: None,
        xdg_surface: None,
        xdg_toplevel: None,
        shm: None,
        buffer: None,
        configured: false,
        frame_pending: false,
    };

    // Initial roundtrip to get globals
    event_queue.roundtrip(&mut app_data)?;

    // Create surface and window
    if let (Some(compositor), Some(shm), Some(xdg_wm_base)) =
        (&app_data.compositor, &app_data.shm, &app_data.xdg_wm_base)
    {
        // Create surface
        let surface = compositor.create_surface(&qh, ());

        // Create XDG surface
        let xdg_surface = xdg_wm_base.get_xdg_surface(&surface, &qh, ());
        let xdg_toplevel = xdg_surface.get_toplevel(&qh, ());

        // Set window properties
        xdg_toplevel.set_title("jerkmon".to_string());
        xdg_toplevel.set_app_id("jerkmon".to_string());

        // Create a minimal buffer
        let buffer = create_buffer(shm, &qh)?;

        app_data.surface = Some(surface);
        app_data.xdg_surface = Some(xdg_surface);
        app_data.xdg_toplevel = Some(xdg_toplevel);
        app_data.buffer = Some(buffer);

        // Commit the surface to trigger configure
        app_data.surface.as_ref().unwrap().commit();
    } else {
        utils::wait_for_shutdown();
        return Ok(());
    }

    // Wait for initial configure
    while !app_data.configured && RUNNING.load(Ordering::Relaxed) {
        event_queue.blocking_dispatch(&mut app_data)?;
    }

    // Attach buffer and commit
    if let (Some(surface), Some(buffer)) = (&app_data.surface, &app_data.buffer) {
        surface.attach(Some(buffer), 0, 0);
        surface.damage(0, 0, WINDOW_WIDTH, WINDOW_HEIGHT);

        // Request first frame callback
        surface.frame(&qh, ());
        app_data.frame_pending = true;

        surface.commit();
    }

    // Event loop
    while RUNNING.load(Ordering::Relaxed) {
        if let Some(guard) = event_queue.prepare_read() {
            let _ = guard.read();
        }

        // Dispatch all pending events
        event_queue.dispatch_pending(&mut app_data)?;

        // If no frame is pending, request one
        if !app_data.frame_pending && RUNNING.load(Ordering::Relaxed) {
            if let Some(surface) = &app_data.surface {
                surface.frame(&qh, ());
                app_data.frame_pending = true;
                surface.commit();
            }
        }

        event_queue.flush()?;
    }

    Ok(())
}

fn create_buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<AppData>,
) -> Result<wl_buffer::WlBuffer, Box<dyn std::error::Error>> {
    let size = (WINDOW_WIDTH * WINDOW_HEIGHT * 4) as usize;

    // Create shared memory file
    let mut file = tempfile::tempfile()?;
    file.set_len(size as u64)?;
    file.write_all(&[0x80; size])?; // Gray pixel

    // Create pool
    use std::os::unix::io::BorrowedFd;
    let pool = shm.create_pool(
        unsafe { BorrowedFd::borrow_raw(file.as_raw_fd()) },
        size as i32,
        qh,
        (),
    );

    // Create buffer
    let buffer = pool.create_buffer(
        0,
        WINDOW_WIDTH,
        WINDOW_HEIGHT,
        WINDOW_WIDTH * 4,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );

    pool.destroy();

    Ok(buffer)
}

fn send_display_event(timestamp: Instant) {
    if let Some(start) = START_TIME.get() {
        let now_ns = timestamp.duration_since(*start).as_nanos() as u64;
        let last_ns = LAST_DISPLAY_NS.swap(now_ns, Ordering::Relaxed);
        let delta_ns = now_ns.saturating_sub(last_ns);

        send_event(EVENT_TYPE_DISPLAY, delta_ns, None);
    }
}
