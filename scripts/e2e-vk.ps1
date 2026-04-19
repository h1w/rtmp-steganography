# Level D — VK end-to-end smoke test (Windows / PowerShell).
param([int]$Seconds = 60)
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

foreach ($v in @("peer_my_rtmp_url","peer_my_stream_key","peer_their_vk_channel","peer_their_stream_name")) {
    if (-not [Environment]::GetEnvironmentVariable($v)) { throw "$v not set" }
}
$env:flicker_log_every_frame = "1"

cargo build --release
$log = New-TemporaryFile
Write-Host "[e2e] log: $log"
Write-Host "[e2e] running peer for $Seconds s..."

$proc = Start-Process -FilePath ".\target\release\rtmp-steganography.exe" -ArgumentList "peer" -NoNewWindow -PassThru -RedirectStandardError $log -RedirectStandardOutput $log
Start-Sleep -Seconds $Seconds
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue

$text = Get-Content $log
$sent  = ($text | Select-String 'time_sync ts=').Count
$rcvd  = ($text | Select-String '\[app\] time_sync').Count
$drops = ($text | Select-String 'dropped:').Count
Write-Host "  sent=$sent rcvd=$rcvd drops=$drops"
if ($sent -gt 0) {
    $rate = [math]::Floor(100 * $rcvd / $sent)
    Write-Host "  delivery rate: $rate%"
    if ($rate -ge 90) { Write-Host "[e2e] PASS"; exit 0 } else { Write-Host "[e2e] FAIL"; exit 1 }
}
Write-Host "[e2e] INCONCLUSIVE"
exit 2
