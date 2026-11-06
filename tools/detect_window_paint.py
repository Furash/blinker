"""External startup detector for BlinkerUI (used for docs/superpowers/specs/startup-results.md).

The same detector measures the PySide6 and the Tauri build, so the numbers compare.
It starts <exe> as a fresh process (environment inherited: set BLINKER_CONFIG_DIR for
the Tauri app; the PySide6 app reads blinker_ui_config.json next to its exe, so use a
scratch copy) and records ms from just before CreateProcess to:

  visible    a visible, titled top-level window of that PID, at least 300x200 (Tauri's
             22x22 single-instance helper window is not the UI). Titles are read with
             InternalGetWindowText, which never sends a message to the busy target.
  paint      the first PrintWindow(PW_CLIENTONLY | PW_RENDERFULLCONTENT) capture of the
             client area with real content: dominant colour neither white nor black and
             >= 12 distinct colours in the sampled grid. Time = when the capture returned.
  populated  the first capture from which on every capture equals the final settled
             frame (<= 0.3 % of sampled pixels differ by more than 24 levels).
Also popen_ms (CreateProcess returned) and created_ms (OS process creation time); a
gap between t=0 and created_ms is time spent before the process existed (e.g. an
antivirus hold on a new exe). Then it posts WM_CLOSE to the window and kills only the
PID it started if that does not end it within 5 s.

Limits: an occluded WebView2 window may stop repainting, so changes after the first
paint (e.g. a status dot flipping) are not reliably seen; run it on an unlocked desktop.
Refuses to run while a process with the exe's name is running (singleton apps would
forward to it; it may be the user's own).

usage: python -I tools/detect_window_paint.py <exe> <out.json> [--settle 6] [--timeout 40] [--save dir]
       (needs numpy; Pillow for --save, which writes paint.png / final.png)
"""
import ctypes, ctypes.wintypes as W, json, os, subprocess, sys, time

import numpy as np

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

EnumWindowsProc = ctypes.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)
user32.EnumWindows.argtypes = [EnumWindowsProc, W.LPARAM]
user32.GetWindowThreadProcessId.argtypes = [W.HWND, ctypes.POINTER(W.DWORD)]
user32.IsWindowVisible.argtypes = [W.HWND]
user32.GetWindowTextLengthW.argtypes = [W.HWND]
user32.GetWindowRect.argtypes = [W.HWND, ctypes.POINTER(W.RECT)]
user32.InternalGetWindowText.argtypes = [W.HWND, W.LPWSTR, ctypes.c_int]
user32.GetClientRect.argtypes = [W.HWND, ctypes.POINTER(W.RECT)]
user32.PrintWindow.argtypes = [W.HWND, W.HDC, W.UINT]
user32.GetDC.argtypes = [W.HWND]
user32.GetDC.restype = W.HDC
user32.ReleaseDC.argtypes = [W.HWND, W.HDC]
user32.PostMessageW.argtypes = [W.HWND, W.UINT, W.WPARAM, W.LPARAM]
gdi32.CreateCompatibleDC.argtypes = [W.HDC]
gdi32.CreateCompatibleDC.restype = W.HDC
gdi32.CreateDIBSection.argtypes = [W.HDC, ctypes.c_void_p, W.UINT, ctypes.POINTER(ctypes.c_void_p), W.HANDLE, W.DWORD]
gdi32.CreateDIBSection.restype = W.HBITMAP
gdi32.SelectObject.argtypes = [W.HDC, W.HGDIOBJ]
gdi32.SelectObject.restype = W.HGDIOBJ
gdi32.DeleteObject.argtypes = [W.HGDIOBJ]
gdi32.DeleteDC.argtypes = [W.HDC]


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [("biSize", W.DWORD), ("biWidth", W.LONG), ("biHeight", W.LONG), ("biPlanes", W.WORD),
                ("biBitCount", W.WORD), ("biCompression", W.DWORD), ("biSizeImage", W.DWORD),
                ("biXPelsPerMeter", W.LONG), ("biYPelsPerMeter", W.LONG), ("biClrUsed", W.DWORD),
                ("biClrImportant", W.DWORD)]


def find_window(pid):
    found = []

    def cb(h, _):
        p = W.DWORD()
        user32.GetWindowThreadProcessId(h, ctypes.byref(p))
        if p.value == pid and user32.IsWindowVisible(h):
            r = W.RECT()
            user32.GetWindowRect(h, ctypes.byref(r))
            # the main window, not a small helper (tauri single-instance '-siw' is 22x22);
            # InternalGetWindowText never sends a message (GetWindowTextLength does and
            # blocks while the target's UI thread is busy).
            buf = ctypes.create_unicode_buffer(64)
            if r.right - r.left >= 300 and r.bottom - r.top >= 200 and user32.InternalGetWindowText(h, buf, 64) > 0:
                found.append(h)
                return False
        return True

    user32.EnumWindows(EnumWindowsProc(cb), 0)
    return found[0] if found else None


def capture(hwnd):
    r = W.RECT()
    user32.GetClientRect(hwnd, ctypes.byref(r))
    w, h = r.right, r.bottom
    if w <= 0 or h <= 0:
        return None
    sdc = user32.GetDC(None)
    mdc = gdi32.CreateCompatibleDC(sdc)
    bmi = BITMAPINFOHEADER(ctypes.sizeof(BITMAPINFOHEADER), w, -h, 1, 32, 0, 0, 0, 0, 0, 0)
    bits = ctypes.c_void_p()
    hbm = gdi32.CreateDIBSection(mdc, ctypes.byref(bmi), 0, ctypes.byref(bits), None, 0)
    old = gdi32.SelectObject(mdc, hbm)
    ok = user32.PrintWindow(hwnd, mdc, 3)  # PW_CLIENTONLY | PW_RENDERFULLCONTENT
    arr = np.ctypeslib.as_array(ctypes.cast(bits, ctypes.POINTER(ctypes.c_ubyte)), (h, w, 4)).copy() if ok else None
    gdi32.SelectObject(mdc, old)
    gdi32.DeleteObject(hbm)
    gdi32.DeleteDC(mdc)
    user32.ReleaseDC(None, sdc)
    return arr


STRIDE = 3


def sample(arr):
    return arr[::STRIDE, ::STRIDE, :3].astype(np.int16)


def has_content(s):
    flat = s.reshape(-1, 3)
    cols, counts = np.unique(flat, axis=0, return_counts=True)
    dom = cols[counts.argmax()]
    if (dom >= 250).all() or (dom <= 5).all():
        return False
    return len(cols) >= 12


def same(a, b):
    if a.shape != b.shape:
        return False
    d = (np.abs(a - b).max(axis=2) > 24).mean()
    return d <= 0.003


def main():
    exe, out = sys.argv[1], sys.argv[2]
    args = sys.argv[3:]
    settle = float(args[args.index("--settle") + 1]) if "--settle" in args else 6.0
    timeout = float(args[args.index("--timeout") + 1]) if "--timeout" in args else 40.0
    save = args[args.index("--save") + 1] if "--save" in args else None
    name = os.path.basename(exe)
    running = subprocess.run(["tasklist", "/FI", f"IMAGENAME eq {name}", "/FO", "CSV", "/NH"],
                             capture_output=True, text=True).stdout
    if name.lower() in running.lower():
        sys.exit(f"{name} is already running (maybe not ours); refusing to measure")
    e0 = time.time()
    t0 = time.perf_counter()
    p = subprocess.Popen([exe], cwd=os.path.dirname(exe))
    ms = lambda: (time.perf_counter() - t0) * 1000
    res = {"exe": exe, "pid": p.pid, "popen_ms": round(ms())}
    # when the OS created the process object, relative to t0 (the app's timeline origin)
    k32 = ctypes.WinDLL("kernel32")
    c, x, k, u = W.FILETIME(), W.FILETIME(), W.FILETIME(), W.FILETIME()
    if k32.GetProcessTimes(W.HANDLE(int(p._handle)), ctypes.byref(c), ctypes.byref(x), ctypes.byref(k), ctypes.byref(u)):
        ce = ((c.dwHighDateTime << 32) | c.dwLowDateTime) / 1e7 - 11644473600
        res["created_ms"] = round((ce - e0) * 1000)
    hwnd = None
    while ms() < timeout * 1000:
        if p.poll() is not None:
            res["exited"] = p.returncode
            break
        hwnd = find_window(p.pid)
        if hwnd:
            res["visible"] = round(ms())
            break
        time.sleep(0.005)
    frames = []  # (t, sample, capture_ms)
    first_full = None
    if hwnd:
        last_change = ms()
        while ms() < timeout * 1000:
            a = ms()
            arr = capture(hwnd)
            b = ms()
            if arr is not None:
                s = sample(arr)
                if first_full is None and has_content(s):
                    first_full = arr
                if frames and not same(frames[-1][1], s):
                    last_change = b
                frames.append((round(b), s, round(b - a)))  # time the content was seen (capture returned)
                if save and len(frames) == 1:
                    np.save(os.path.join(save, "first.npy"), arr)
            if b - last_change > settle * 1000 and b - res["visible"] > settle * 1000:
                break
            time.sleep(max(0, 0.05 - (ms() - a) / 1000))
        if frames:
            final = frames[-1][1]
            paint = next((t for t, s, _ in frames if has_content(s)), None)
            pop = None
            for t, s, _ in reversed(frames):
                if same(s, final):
                    pop = t
                else:
                    break
            res["paint"] = paint
            res["populated"] = pop
            res["frames"] = len(frames)
            res["capture_ms_median"] = float(np.median([c for _, _, c in frames]))
            res["first_frame_t"] = frames[0][0]
            if save:
                from PIL import Image
                Image.fromarray(arr[:, :, 2::-1]).save(os.path.join(save, "final.png"))
                if first_full is not None:
                    Image.fromarray(first_full[:, :, 2::-1]).save(os.path.join(save, "paint.png"))
        user32.PostMessageW(hwnd, 0x0010, 0, 0)  # WM_CLOSE
    try:
        p.wait(5)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait(5)
        res["killed"] = True
    with open(out, "w") as f:
        json.dump(res, f)
    print(json.dumps(res))


if __name__ == "__main__":
    main()
