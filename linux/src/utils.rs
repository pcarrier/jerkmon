
/// Set real-time thread priority for critical monitoring threads
pub fn set_thread_priority(_name: &str) {
    unsafe {
        let param = libc::sched_param { sched_priority: 2 };
        libc::pthread_setschedparam(
            libc::pthread_self(),
            libc::SCHED_FIFO,
            &param,
        );
    }
}

/// Wait for the global RUNNING flag to be set to false
pub fn wait_for_shutdown() {
    use std::sync::atomic::Ordering;
    use crate::RUNNING;
    
    while RUNNING.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}