use std::time::{Duration, Instant};

use super::state::{lock, ChildRef};

/// `QProcess::waitForFinished(500)` after `terminate()` (WM_CLOSE), before the hard kill.
const TERMINATE_WAIT: Duration = Duration::from_millis(500);

fn has_exited(c: &ChildRef) -> bool {
    matches!(lock(c).try_wait(), Ok(Some(_)) | Err(_))
}

/// `QProcess::terminate()` + `waitForFinished(500)` for several processes at once:
/// all get WM_CLOSE first, they share one 500 ms wait, then the rest are killed.
pub(super) fn close_gracefully(children: Vec<ChildRef>) {
    let running: Vec<ChildRef> = children.into_iter().filter(|c| !has_exited(c)).collect();
    if running.is_empty() {
        return;
    }
    for c in &running {
        let pid = lock(c).id();
        post_close(pid);
    }
    let deadline = Instant::now() + TERMINATE_WAIT;
    while Instant::now() < deadline && !running.iter().all(has_exited) {
        std::thread::sleep(Duration::from_millis(5));
    }
    for c in &running {
        if !has_exited(c) {
            let _ = lock(c).kill();
        }
    }
}

/// Post WM_CLOSE to every top-level window (visible or not) owned by `pid`, as
/// `QProcess::terminate()` does on Windows. Returns how many windows were found.
#[cfg(windows)]
fn post_close(pid: u32) -> usize {
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };

    struct Search {
        pid: u32,
        found: usize,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the `&mut Search` passed to EnumWindows below, which is
        // alive for the whole (synchronous) enumeration.
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut owner = 0u32;
        // SAFETY: hwnd comes from EnumWindows; owner is a valid out pointer.
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        if owner == search.pid {
            // SAFETY: posting a message to a window handle has no memory effects here.
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
            search.found += 1;
        }
        1
    }
    let mut search = Search { pid, found: 0 };
    // SAFETY: `each` matches WNDENUMPROC and only uses lparam as `&mut Search`.
    unsafe { EnumWindows(Some(each), &mut search as *mut Search as LPARAM) };
    search.found
}

#[cfg(not(windows))]
fn post_close(_pid: u32) -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blender::test_support::*;

    /// Stand-in Blender with a real (hidden) top-level window. Runs only as a child
    /// process of `terminate_closes_window_gracefully`, which sets the env var.
    #[test]
    #[ignore = "helper process for terminate_closes_window_gracefully"]
    fn window_child_helper() {
        use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostQuitMessage,
            RegisterClassW, MSG, WM_CLOSE, WM_DESTROY, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        };
        if std::env::var_os(WINDOW_CHILD_ENV).is_none() {
            return;
        }
        unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            match msg {
                WM_CLOSE => {
                    println!("got WM_CLOSE");
                    unsafe { DestroyWindow(hwnd) };
                    0
                }
                WM_DESTROY => {
                    unsafe { PostQuitMessage(0) };
                    0
                }
                _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
            }
        }
        let class: Vec<u16> = "BlinkerTestWindow\0".encode_utf16().collect();
        unsafe {
            let hinst = GetModuleHandleW(std::ptr::null());
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = hinst;
            wc.lpszClassName = class.as_ptr();
            // BLINKER_TEST_FORCE_NO_WINDOW: exercise the skip path by hand.
            if RegisterClassW(&wc) == 0 || std::env::var_os("BLINKER_TEST_FORCE_NO_WINDOW").is_some() {
                println!("{WINDOW_UNAVAILABLE}");
                std::process::exit(43);
            }
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPEDWINDOW, // not WS_VISIBLE: hidden, still enumerated
                0,
                0,
                100,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                println!("{WINDOW_UNAVAILABLE}");
                std::process::exit(43);
            }
            println!("window-ready");
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
        }
        println!("closed gracefully");
        std::process::exit(42);
    }

    const WINDOW_CHILD_ENV: &str = "BLINKER_TEST_WINDOW_CHILD";
    /// Printed by the helper when it cannot create a window (no usable desktop/window
    /// station, e.g. some service-hosted CI runners): the windowed tests then skip.
    const WINDOW_UNAVAILABLE: &str = "window-unavailable";

    /// Launch the test binary itself as a windowed stand-in Blender and wait for its
    /// window. False (after printing a skip note) when no window can be created here.
    fn launch_windowed(e: &Env, port: u16) -> bool {
        // Inherited by children; cmd.exe stand-ins in other tests ignore it.
        std::env::set_var(WINDOW_CHILD_ENV, "1");
        let mut spec = e.cmd(&format!("windowed{port}"), port, "");
        spec.blender = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        spec.args_override = Some(
            ["blender::close::tests::window_child_helper", "--exact", "--ignored", "--nocapture", "--test-threads=1"]
                .map(String::from)
                .to_vec(),
        );
        e.sup.launch(spec).unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            let out = e.sup.output(port);
            if out.contains("window-ready") {
                return true;
            }
            if out.contains(WINDOW_UNAVAILABLE) {
                eprintln!("SKIP: no window could be created (no interactive desktop?)");
                return false;
            }
            assert!(Instant::now() < deadline, "helper never opened its window: {out}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn shutdown_closes_windows_gracefully() {
        let e = env();
        if !launch_windowed(&e, 9019) {
            return;
        }
        e.sup.launch(e.cmd("stubborn2", 9020, BUSY)).unwrap();
        let t0 = Instant::now();
        e.sup.shutdown();
        let took = t0.elapsed();
        // One shared 500 ms wait, not one per child; generous ceiling for loaded CI runners.
        assert!(took >= TERMINATE_WAIT && took < Duration::from_secs(10), "{took:?}");
        let mut exits = e.sink.wait_exits(2);
        exits.sort();
        assert_eq!(exits, vec![(9019, Some(42)), (9020, Some(1))], "windowed one closed, stubborn one killed");
        assert!(e.sup.output(9019).contains("closed gracefully"));
    }

    #[test]
    fn terminate_closes_window_gracefully() {
        let e = env();
        if !launch_windowed(&e, 9016) {
            return;
        }
        let t0 = Instant::now();
        assert!(e.sup.terminate(9016));
        // Normally well under the 500 ms wait; the ceiling only guards a loaded CI runner.
        // That it closed gracefully (not killed) is the exit code 42 below.
        assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
        // 42 = left its message loop normally; a hard kill would give 1
        assert_eq!(e.sink.wait_exits(1), vec![(9016, Some(42))]);
        let out = e.sup.output(9016);
        assert!(out.contains("got WM_CLOSE") && out.contains("closed gracefully"), "{out}");
        assert!(!e.sup.terminate(9016), "nothing left to terminate");
    }

    #[test]
    fn terminate_kills_child_that_ignores_close() {
        let e = env();
        assert!(!e.sup.terminate(9017));
        e.sup.launch(e.cmd("stubborn", 9017, BUSY)).unwrap();
        let t0 = Instant::now();
        assert!(e.sup.terminate(9017));
        let took = t0.elapsed();
        assert!(took >= TERMINATE_WAIT, "waited for a graceful exit first: {took:?}");
        assert!(took < Duration::from_secs(10), "{took:?}");
        assert_eq!(e.sink.wait_exits(1), vec![(9017, Some(1))], "hard-killed after the wait");
        let out = e.sup.output(9017);
        assert!(!out.contains("[restarting"), "{out}");
        assert!(!out.contains("[kill]"), "terminate is silent, like Python: {out}");
        assert!(!e.sup.is_alive(9017));
    }
}
