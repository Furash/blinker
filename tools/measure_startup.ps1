param(
    [Parameter(Mandatory=$true)][string]$Exe,
    [int]$Runs = 3
)
# Prints ms from process start to a visible main window, one line per run, then the median.
$Exe = (Resolve-Path $Exe).Path
Add-Type -Namespace W -Name U -MemberDefinition '[System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool IsWindowVisible(System.IntPtr h);'
$name = [IO.Path]::GetFileNameWithoutExtension($Exe)
if (Get-Process -Name $name -ErrorAction SilentlyContinue) { Write-Error "$name is already running; close it first (singleton apps would skew the result)."; exit 1 }
$times = @()
for ($i = 1; $i -le $Runs; $i++) {
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
    if ($null -eq $ms) { Write-Output ("run {0}: no window (exited={1})" -f $i, $p.HasExited) }
    else { Write-Output ("run {0}: {1} ms" -f $i, $ms); $times += $ms }
    Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 1000
}
if ($times.Count -gt 0) {
    $s = $times | Sort-Object
    $n = $s.Count
    $median = if ($n % 2) { $s[[int](($n-1)/2)] } else { ($s[$n/2-1] + $s[$n/2]) / 2 }
    Write-Output ("median: {0} ms" -f $median)
}
