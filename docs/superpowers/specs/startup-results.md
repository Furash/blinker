# Startup results and parity walk (Task 15)

Date: 2026-11-06. Machine: Windows 11 Enterprise 10.0.26200, 24 logical CPUs, 4K monitor
at 150 %, Windows Defender real-time protection on (cloud protection "Advanced", Block at
First Sight on, `CloudExtendedTimeout` 50). Other desktop apps were running (Discord,
Claude, later a Steam Blender), so absolute numbers carry some noise.

Builds measured:

- **Old**: PySide6/PyInstaller onedir, rebuilt from the current `blinker_ui.py` with
  `BlinkerUI.spec` and the same post-build force-UPX steps as `build_exe.cmd` (output in
  a scratch folder; the repo's `dist\BlinkerUI` dated 2026-05-04 was older than the last
  two `blinker_ui.py` commits). 24 MB, 79 binaries.
- **New**: Tauri release exe built from commit `8e35a44` (4.7 MB, `opt-level="s"`, LTO,
  strip). The commits after it change only a test and docs. The exe rebuilt from the
  final commit is not byte-identical (the build is not reproducible), so it was checked
  again: the full live walk below passed (23/23), warm visible 690, 1328, 882, 1059,
  672 ms (median 882; measured while a Steam Blender was running on the machine).

Same config for both: the three folders of `tools/grab_reference.py` (default Tokyo
theme). Old: config next to its exe (scratch copy); new: `BLINKER_CONFIG_DIR`.

## Method

One external detector for both apps ([tools/detect_window_paint.py](../../../tools/detect_window_paint.py),
Python `-I` + ctypes; usage and definitions in its header):

- t = 0 just before `CreateProcess` (what a double-click starts).
- **visible**: a visible, titled top-level window of that PID, at least 300x200
  (Tauri's 22x22 single-instance helper window is excluded). Titles are read with
  `InternalGetWindowText`, which never sends a message (`GetWindowTextLength` does and
  stalls while the target UI thread is busy, which first produced bogus 11 s numbers).
- **paint**: first `PrintWindow(PW_CLIENTONLY|PW_RENDERFULLCONTENT)` capture (every
  ~50 ms, time = when the capture returned) whose dominant colour is neither white nor
  black and that has at least 12 colours: real themed content.
- **populated**: first capture from which on every capture equals the final settled
  frame (<= 0.3 % of sampled pixels off by > 24 levels; settle = 4 s unchanged): rows
  rendered. Both apps render the rows before their first paint, so populated is paint
  or a little later (Qt repaints ~110 ms after its first frame).
- **cold-ish**: the app folder freshly copied before each run (new files, so Defender
  has not seen them at that path); **warm**: the same copy started again.
- The new app also logs its own timeline (`BLINKER_READY_LOG`, added in `dadd253`):
  steps in ms since process creation.

Not possible here: a reboot (true cold start).

Status dots: both apps probe all ports, then apply the result at once, and on Windows
a connect to a closed loopback port times out (1 s) instead of failing fast, so a
running Blender's dot appears ~1 s after the probe starts in both apps (same code
design: `ThreadPoolExecutor` + 1 s timeout vs `net::probe` + 1 s timeout). Measured on
the new app over CDP with a fake Blender on 9876: shown at 736 ms (median, 620-1027),
dot `●` at 1610 ms (1505-1858), n = 6. The pixel detector could not time the dot flip
reliably (an occluded WebView2 stops repainting), so it is not in the tables.

## Old vs new (ms from CreateProcess)

| | visible | first paint | populated |
|---|---|---|---|
| Old, cold-ish (n=5) | **7296** (4143-14694) | 7435 (4240-14818) | 7662 (4390-15031) |
| Old, warm (n=5) | **2074** (1998-2125) | 2128 (2035-2168) | 2247 (2147-2300) |
| New, cold-ish (n=5) | **2320** (1755-11015) | 2356 (1783-11053) | = paint |
| New, warm (n=5) | **655** (630-711) | 686 (662-745) | = paint |

The cold-ish medians are not directly comparable: with n = 5, a few runs held by
Defender before the process even existed (~10 s: 1 of 5 old runs, 2 of 5 new runs) or
otherwise slowed move the median a lot; read them with the raw lists below.

Raw runs (visible / paint / populated):

- Old cold-ish: 7296/7435/7662, 14676/14806/15031, 14694/14818/14932, 6124/6238/6360,
  4143/4240/4390.
- Old warm: 2122/2164/2281, 2074/2116/2236, 2125/2168/2300, 1998/2035/2147,
  2049/2128/2247. A later warm batch: 2390, 2059, 2073, 2088, 2088 (populated 2565,
  2233, 2247, 2266, 2254). One warm run during the later, busier period took 14.2 s.
- New cold-ish: 2320/2356, 11003/11041, 1781/1810, 1755/1783, 11015/11053.
- New warm: 664/691, 630/662, 655/686, 638/666, 711/745. A later batch (owner's Blender
  running): 862, 771, 758, 942, 650 (median 771).
- First run of each brand-new build right after copying: old (repo `dist`, May build)
  **30888** ms visible; old (fresh rebuild) **19670**; old without UPX **4301**.

Headline: warm, the new app shows the full window in ~0.65 s vs ~2.1-2.2 s (about
3x faster). After a fresh copy the old app took 4-15 s (median 7.3 s; 20-31 s for the
first launch of a new build), the new one 1.8-2.3 s, or ~11 s in 2 of 5 runs.

## Where the old app's ~15 s goes (reproduced)

The owner's ~15 s is reproducible here, but only for files Defender has not cleared yet:
the first launch after an install/update, or of a fresh copy. Warm starts are ~2.1 s.

Evidence:

- `CreateProcess` itself blocks: the detector records when `Popen` returns and when the
  OS created the process. For fresh copies (both apps) `CreateProcess` took 0.3-1.4 s,
  and in 4 of 15 fresh-copy runs **~10.3 s** before the process existed at all. 10 s is
  Defender's default cloud-verdict wait for an unknown file, so this is consistent with
  Block at First Sight holding the new, unsigned exe (Defender itself is a protected
  process; its CPU time and exclusion list are not readable without admin).
- The app itself does little: app CPU time on a fresh-copy start was 1.4-2.5 s while
  wall time was 5-13 s; machine-wide busy CPU rose to 27-85 CPU-seconds over that time
  vs ~10-13 on a warm start (`GetSystemTimes`), i.e. another process (the scanner) did
  the work.
- UPX: the build's force-UPX step strips the Authenticode signatures of 22 binaries:
  the UPX build has 56 of 79 binaries validly signed, the plain build 78 of 79
  (`python311.dll`, `Qt6Core/Gui/Widgets.dll` signed by the PSF / The Qt Company in the
  plain build, `NotSigned` after UPX). Unsigned unknown DLLs get the full scan / cloud
  lookup; that fits the 19.7-30.9 s first launches of new UPX builds vs 4.3 s for the
  first launch of the plain build. Later fresh copies (same content) were less clear:
  UPX 4.1-14.7 s, plain 4.3-13.4 s, so the per-path hold above dominates once Defender
  knows the content. Warm, the plain build starts in 1.23 s vs 2.07 s for UPX (UPX
  decompression on every launch).
- Ruled out for the startup itself: the update check (the old app does not check at
  start), proxy auto-detect (only used by the update check), the `wt` lookup
  (`shutil.which` at import, cheap), the first status poll (runs after the window is
  shown).
- Not verified: why the owner sees it on (apparently) every launch. Plausible, not
  measured: Defender re-scans after definition updates (several a day), so the first
  launch after one behaves like cold-ish; slow or network disks; a non-reboot "cold"
  start after the file cache was evicted.

## New app breakdown (warm, median of 5, ms since process creation)

| step | ms | delta |
|---|---|---|
| Rust `run()` | 22 | 22 exe load |
| webview navigation start | 437 | 415 WebView2 environment + controller (browser process start) |
| Tauri `setup` hook | 509 | 72 rest of window build |
| page load started | 560 | 51 event loop starts serving `index.html` |
| JS module start | 588 | 28 HTML, CSS, 13 ES modules |
| `get_state` returned | 601 | 13 |
| icons + `fonts.ready` | 610 | 9 |
| rows rendered | 626 | 16 |
| double rAF fired (`show_raf`, never the 100 ms timeout) | 642 | 16 |
| `show()` returned | 666 | 24 |

(Each step's median separately, runs 2-6 of the warm batch; process creation is ~1 ms
after `CreateProcess` was called.)

About 0.48 s (75 %) is the framework's WebView2 start before any of our code runs; our
setup (config load, poller) takes 4 ms and the frontend ~80 ms from JS start to shown.
On a cold-ish start the exe is scanned before `run()` (124-477 ms) and WebView2 start
takes ~450-580 ms.

Experiments (A/B, alternating runs, 8 pairs each, warm, detector "visible" median):

| change | before | after | verdict |
|---|---|---|---|
| extra WebView2 args `--disable-background-networking --disable-component-update --no-first-run` (via `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`) | 790 | 871 | no gain, not adopted |
| `opt-level = 3` (exe 6.4 MB vs 4.7 MB) | 670 | 844 | no gain, not adopted |
| fresh WebView2 user-data folder (first ever start) | nav start ~430 | ~513-598 | +90-170 ms once; the default folder `%LOCALAPPDATA%\com.fenris.blinkerui\EBWebView` persists (created 11:34, reused by every later run) |

Not tried, with reasons: Tauri already passes
`--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection` by default (wry);
`modulepreload` (module loading is ~30 ms, below the noise of ±100 ms between batches);
`fonts.ready`/icons (8 ms); showing the window before the webview is ready with a native
themed background (would make "visible" ~50 ms but not the first paint, and the spec
asks for show-after-first-paint). No perf change was adopted; nothing measurable beat the
noise.

Target check: warm 0.65 s meets "under 1 s". First launch after an update does not:
1.8-2.3 s, and ~11 s when Defender holds the new unsigned exe. Only signing the exe (or
getting a Defender verdict for each release, e.g. Microsoft's file submission) can
remove that; both are owner decisions, not code changes.

## Visual parity (final build)

Qt reference (`tools/grab_reference.py`, new `*_plain_150` variants: nothing running,
row 0 selected, empty console) vs the real release exe at 150 % with the same config
(`Config:` label set to the same fake path), client area 1650x1230, share of pixels
differing by > 24 levels:

| theme | diff |
|---|---|
| Tokyo Night | 2.73 % |
| Frigus Nox | 2.96 % |
| Light | 3.47 % |

Remaining differences as in Task 9: text antialiasing (ClearType vs Chromium), ±1-3 px
text widths, rows ~1 px shorter per row at 150 %, the new Open folder button.
Earlier task numbers for the dialogs (Task 10/11) still apply; the dialogs did not
change.

## Parity checklist (spec feature inventory)

Live = release exe (debug exe for the update check), temp `BLINKER_CONFIG_DIR`, `TEMP`
redirected, driven over WebView2 CDP, fake Blender (`blender_path` = a small compiled
stand-in that answers `ping`/`reload`/`restart`/`kill` like `bootstrap.py`, exits 75
on restart).

| feature | status |
|---|---|
| Main list: name, meta text, port box, favourite, selection | verified live (this task) |
| Status running/stopped from port probe | verified live (this task: external fake Blender on 9876 -> `●` after ~0.9 s) |
| Hover/pressed states | verified by screenshot/CSS review only (Task 9) |
| Launch (bootstrap args, `BLINKER_*` env, output streamed) | verified live (this task) |
| Reload | verified live (`[reload] ok (3 modules)`) |
| Restart: exit 75 -> `[restarting blender...]` -> relaunch | verified live (this task; marker/blend path by Rust test) |
| Kill | verified live (`[kill] process killed`, exit line, Launch back, process gone) |
| Clear console | verified live (pane empty, later output shown) |
| Port edit (+ save, re-probe) | verified live (Enter commits, config saved) |
| Port box disabled while running | verified live (deviation by design) |
| Favourite toggle + sort | verified live (moved to top, saved; unpin saved) |
| Remove (confirm No/Yes, terminate) | verified live |
| Open folder (new) | verified live after fix `8e35a44` (opened Documents before) |
| Edit dialog | verified live in Task 10 (release exe, native pickers); not redone |
| Scan dialog / No addons | verified live in Task 10; not redone |
| AI launchers (icons, alias list) | icons verified by screenshot; real terminal launch verified by test only (`open_ai` tests; not launched live, to avoid opening terminals) |
| Output pane per selected folder | verified live (selection switches label and content) |
| Settings: terminal, aliases, Blender path, fonts, colours, presets | verified live in Task 11 (release exe); not redone |
| Settings Apply/Cancel/OK | verified live in Task 11 |
| Theme export/import | verified live in Task 11 (byte-identical to Python) |
| Process exit closes: closing the window kills Blender | verified live (this task: stand-in gone, app exit 0 in 1.8 s) |
| Background poll of folder ports | verified live (status event with all three ports) |
| Singleton: second launch focuses the window | verified live (this task: minimised window restored, foreground, second process exit 0 in 0.12 s) |
| Second launch before ready / after startup failure | verified by unit test + mock failure test (fix `1a1db48`) |
| Window never shown by the frontend -> backend shows it after 5 s | decision unit-tested (`fallback_should_show`); not triggered live (no way to hang the frontend in a release build); normal starts unaffected (smoke run) |
| Update check: up to date / update available (No) | verified live (debug exe + local server: texts equal to Python) |
| Update download + verify + swap + relaunch | verified end to end in Task 12 (`--update-selftest`); GUI click-through of a real install not redone |
| AUMID / icon | unit test + Task 13 (taskbar grouping not checked visually) |
| WebView2 missing message | decision unit-tested (`webview2_installed` with an injected registry reader: present / absent / `0.0.0.0` / empty); live with a debug build and `BLINKER_SIMULATE_NO_WEBVIEW2`: the native box appeared in 0.2 s with the download link, No -> exit code 1. Not verified on a machine really without the runtime. Added in the final fix wave (`9b9e830`). |
| Config migration from exe dir | verified by tests (Task 2/8); not run live (it would touch the real `%APPDATA%`) |
| 100 % DPI look | verified in Task 9 (mock at 100 %); this task compared 150 % |

## Findings not fixed

- Kill exit line says `[blender exited code 1]` (Rust `TerminateProcess(.., 1)`); Qt's
  `QProcess::kill` uses exit code 0xF291, so the Python UI most likely printed 62097.
  Cosmetic; not verified against the Python app live.
- First launch after an update can wait ~10 s on Defender (unsigned new exe); see above.
- Status dots wait for the slowest port probe (1 s timeout on closed loopback ports),
  as in Python.
- A `Documents` Explorer window may have been left open by the first walk run (before
  the Open folder fix); it could not be told apart from the user's own windows, so it was
  not closed.
