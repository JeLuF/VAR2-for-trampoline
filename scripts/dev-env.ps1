# Sets up the current PowerShell session for building on Windows:
# Rust (cargo) and GStreamer (bin on PATH, PKG_CONFIG_PATH).
#
# Usage:  . .\scripts\dev-env.ps1

$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x86_64' }
$candidates = @(
    [Environment]::GetEnvironmentVariable("GSTREAMER_1_0_ROOT_MSVC_$($arch.ToUpper())"),
    "$env:LOCALAPPDATA\Programs\gstreamer\1.0\msvc_$arch",
    "C:\gstreamer\1.0\msvc_$arch",
    "C:\Program Files\gstreamer\1.0\msvc_$arch"
) | Where-Object { $_ -and (Test-Path (Join-Path $_ 'lib\pkgconfig\gstreamer-1.0.pc')) }

if (-not $candidates) {
    Write-Error "GStreamer MSVC ($arch) with development files not found."
    return
}
$gst = ($candidates | Select-Object -First 1).TrimEnd('\')

$env:PATH = "$gst\bin;$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:PKG_CONFIG_PATH = "$gst\lib\pkgconfig"
Write-Host "GStreamer: $gst"
