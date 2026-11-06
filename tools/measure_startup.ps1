param(
    [Parameter(Mandatory=$true)][string]$Exe,
    [int]$Runs = 3
)
# Prints ms from process start to a visible main window, one line per run, then the median.
# For the Tauri build it also sets BLINKER_READY_LOG and prints the UI's ready line:
# ready_ms (performance.now() when the window was shown, counted from navigation start)
# and the startup timeline (steps in ms since process creation, docs/dev-notes.md).
# Set BLINKER_CONFIG_DIR beforehand to keep the runs away from the real config.
$Exe = (Resolve-Path $Exe).Path
if (-not ('W.U' -as [type])) {
    Add-Type -Namespace W -Name U -MemberDefinition '[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool IsWindowVisible(System.IntPtr h);'
}
$name = [IO.Path]::GetFileNameWithoutExtension($Exe)
if (Get-Process -Name $name -ErrorAction SilentlyContinue) { Write-Error "$name is already running; close it first (singleton apps would skew the result)."; exit 1 }
$readyLog = Join-Path ([IO.Path]::GetTempPath()) ("blinker_ready_{0}.log" -f $PID)
$oldReady = $env:BLINKER_READY_LOG
$env:BLINKER_READY_LOG = $readyLog
$times = @()
for ($i = 1; $i -le $Runs; $i++) {
    Remove-Item $readyLog -ErrorAction SilentlyContinue
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $Exe -PassThru
    $ms = $null
    while ($sw.ElapsedMilliseconds -lt 60000) {
        $p.Refresh()
        if ($p.HasExited) { break }
        # A titled window: WebView2/tao first show an untitled helper window, which is not the UI.
        if ($p.MainWindowHandle -ne [IntPtr]::Zero -and $p.MainWindowTitle -and [W.U]::IsWindowVisible($p.MainWindowHandle)) {
            $ms = $sw.ElapsedMilliseconds; break
        }
        Start-Sleep -Milliseconds 10
    }
    # The UI reports right after show(); give it a moment (absent for non-Tauri builds).
    $ready = $null
    $deadline = $sw.ElapsedMilliseconds + 2000
    while ($null -ne $ms -and $sw.ElapsedMilliseconds -lt $deadline) {
        if (Test-Path $readyLog) { $ready = (Get-Content $readyLog | Select-Object -Last 1); if ($ready) { break } }
        Start-Sleep -Milliseconds 20
    }
    $suffix = if ($ready) { "  ($ready)" } else { "" }
    if ($null -eq $ms) { Write-Output ("run {0}: no window (exited={1})" -f $i, $p.HasExited) }
    else { Write-Output ("run {0}: {1} ms{2}" -f $i, $ms, $suffix); $times += $ms }
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 1000
}
Remove-Item $readyLog -ErrorAction SilentlyContinue
$env:BLINKER_READY_LOG = $oldReady
if ($times.Count -gt 0) {
    $s = $times | Sort-Object
    $n = $s.Count
    $median = if ($n % 2) { $s[[int](($n-1)/2)] } else { ($s[$n/2-1] + $s[$n/2]) / 2 }
    Write-Output ("median: {0} ms" -f $median)
}
