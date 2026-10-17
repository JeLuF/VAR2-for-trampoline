# Builds the Windows installer (MSI) of VAR for Trampoline with the GStreamer runtime.
#
# Requirements: Rust (with the target of the architecture), Visual Studio Build Tools
# (C++ and the compilers for the architecture), GStreamer MSVC 1.24 or later for the
# architecture (runtime and development files), WiX Toolset 5
# (`dotnet tool install --global wix --version 5.0.2`).
#
# Usage (from the repository root):
#   powershell -ExecutionPolicy Bypass -File packaging\windows\build-msi.ps1 [-Arch x64|arm64] [-GStreamerRoot DIR]
#
# -Arch defaults to x64 (typical event laptops). Building x64 on an ARM64 machine
# works: Rust cross-compiles, GStreamer x86_64 is needed in addition.
#
# The installer contains var.exe, var-check.exe (to check recordings on the event
# machine), the GStreamer plugins the application uses and exactly the DLLs they
# load, and the licence texts of all bundled components (PLT-006). It fails if a
# GPL-licensed library would be bundled.

param(
    [ValidateSet("x64", "arm64")]
    [string]$Arch = "x64",
    [string]$GStreamerRoot,
    [string]$OutDir = "target\msi"
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $repo

$gstArch = if ($Arch -eq "arm64") { "arm64" } else { "x86_64" }
$rustTarget = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$candidates = @(
    [Environment]::GetEnvironmentVariable("GSTREAMER_1_0_ROOT_MSVC_$($gstArch.ToUpper())"),
    "$env:LOCALAPPDATA\Programs\gstreamer\1.0\msvc_$gstArch",
    "C:\gstreamer\1.0\msvc_$gstArch",
    "C:\Program Files\gstreamer\1.0\msvc_$gstArch"
)
if (-not $GStreamerRoot) {
    $GStreamerRoot = $candidates |
        Where-Object { $_ -and (Test-Path (Join-Path $_ "lib\pkgconfig\gstreamer-1.0.pc")) } |
        Select-Object -First 1
}
if (-not $GStreamerRoot) { throw "GStreamer MSVC ($gstArch) with development files not found; pass -GStreamerRoot" }
$GStreamerRoot = $GStreamerRoot.TrimEnd('\')
Write-Host "Architecture: $Arch ($rustTarget)"
Write-Host "GStreamer:    $GStreamerRoot"

# pkg-config of any installed GStreamer runs on this machine; it reads the .pc files
# of the target architecture.
$hostGst = $candidates + @("$env:LOCALAPPDATA\Programs\gstreamer\1.0\msvc_arm64", "$env:LOCALAPPDATA\Programs\gstreamer\1.0\msvc_x86_64") |
    Where-Object { $_ -and (Test-Path (Join-Path $_ "bin\pkg-config.exe")) } | Select-Object -First 1
$env:PATH = "$hostGst\bin;$env:USERPROFILE\.cargo\bin;$env:USERPROFILE\.dotnet\tools;$env:PATH"
$env:PKG_CONFIG_PATH = "$GStreamerRoot\lib\pkgconfig"
$env:PKG_CONFIG_ALLOW_CROSS = "1"

# dumpbin reads the DLL imports of any architecture.
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -property installationPath
$dumpbin = Get-ChildItem -Recurse -Filter dumpbin.exe "$vs\VC\Tools\MSVC" | Select-Object -First 1 -ExpandProperty FullName
if (-not $dumpbin) { throw "dumpbin.exe not found (Visual Studio Build Tools)" }

$version = (Select-String -Path Cargo.toml -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value

cargo build --release --target $rustTarget --bin var --bin var-check
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$bin = "target\$rustTarget\release"

# Stage the files to install. Absolute, because WiX resolves relative bind paths
# against the .wxs file.
$OutDir = Join-Path $repo $OutDir
$stage = Join-Path $OutDir "stage-$Arch"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force "$stage\lib\gstreamer-1.0", "$stage\libexec\gstreamer-1.0", "$stage\licenses" | Out-Null

Copy-Item "$bin\var.exe", "$bin\var-check.exe" $stage
Copy-Item "LICENSE" "$stage\LICENSE.txt"
Copy-Item "$GStreamerRoot\libexec\gstreamer-1.0\gst-plugin-scanner.exe" "$stage\libexec\gstreamer-1.0\"

# Plugins the application uses. Not every plugin exists in every build (NVENC, Quick
# Sync and AMF are x64 only).
$plugins = @(
    "coreelements", "app", "videoconvertscale", "typefindfunctions",
    # Capture: Media Foundation (Windows), MJPEG decoding.
    "mediafoundation", "jpeg",
    # Synthetic test cameras (--synthetic-cameras).
    "videotestsrc", "imagefreeze",
    # H.264 encoders: Media Foundation, NVIDIA, Intel, AMD, D3D12; OpenH264 fallback.
    "nvcodec", "qsv", "amfcodec", "d3d12", "d3d11", "openh264",
    # Review: parser, decodebin, hardware decoders (D3D11/D3D12).
    "videoparsersbad", "playback",
    # var-check: reads the MPEG-TS segments.
    "mpegtsdemux"
)
foreach ($plugin in $plugins) {
    $dll = "$GStreamerRoot\lib\gstreamer-1.0\gst$plugin.dll"
    if (Test-Path $dll) {
        Copy-Item $dll "$stage\lib\gstreamer-1.0\"
    } else {
        Write-Host "plugin not available for ${Arch}: $plugin"
    }
}

# Exactly the GStreamer DLLs that the executables and plugins load, recursively.
$available = @{}
Get-ChildItem "$GStreamerRoot\bin" -Filter *.dll | ForEach-Object { $available[$_.Name.ToLower()] = $_.FullName }
$queue = New-Object System.Collections.Queue
Get-ChildItem $stage -Recurse -Include *.exe, *.dll | ForEach-Object { $queue.Enqueue($_.FullName) }
$bundled = @{}
while ($queue.Count -gt 0) {
    $file = $queue.Dequeue()
    $imports = & $dumpbin /nologo /dependents $file | ForEach-Object { $_.Trim() } | Where-Object { $_ -match '\.dll$' }
    foreach ($import in $imports) {
        $name = $import.ToLower()
        if ($available.ContainsKey($name) -and -not $bundled.ContainsKey($name)) {
            $bundled[$name] = $true
            Copy-Item $available[$name] $stage
            $queue.Enqueue((Join-Path $stage $import))
        }
    }
}
Write-Host "Bundled GStreamer DLLs: $($bundled.Count)"

# The Visual C++ runtime, app-local (allowed by the Visual Studio redistribution
# terms): a fresh Windows doesn't necessarily have it. Then every imported DLL must
# be bundled or part of Windows.
$crt = Get-ChildItem "$vs\VC\Redist\MSVC" -Directory | Sort-Object Name -Descending |
    ForEach-Object { Join-Path $_.FullName "$Arch\Microsoft.VC143.CRT" } |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $crt) { throw "Visual C++ runtime ($Arch) not found in $vs\VC\Redist\MSVC" }
$files = Get-ChildItem $stage -Recurse -Include *.exe, *.dll
$imports = $files | ForEach-Object { & $dumpbin /nologo /dependents $_.FullName } |
    ForEach-Object { $_.Trim() } | Where-Object { $_ -match '^[\w.-]+\.dll$' } | Sort-Object -Unique
$staged = $files | ForEach-Object { $_.Name.ToLower() }
foreach ($import in $imports) {
    $name = $import.ToLower()
    if ($staged -contains $name -or $name -match '^(api|ext)-ms-') { continue }
    if (Test-Path "$crt\$import") {
        Copy-Item "$crt\$import" $stage
        Write-Host "Visual C++ runtime: $import"
    } elseif (-not (Test-Path "$env:windir\System32\$import")) {
        throw "$import is imported but neither bundled nor part of Windows"
    }
}

# PLT-006: no GPL components.
$gpl = $bundled.Keys | Where-Object { $_ -match '^(x264|x265|libx264|libx265|dvdread|dvdnav|a52|faad|mpeg2|libmpeg2|rtmp)' }
if ($gpl) { throw "GPL-licensed libraries would be bundled: $($gpl -join ', ')" }

# Licence texts of the bundled components: GStreamer and the libraries in its
# licences folder whose DLLs are bundled.
$licenseSource = "$GStreamerRoot\share\licenses"
$components = @(
    "gstreamer-1.0", "gst-plugins-base-1.0", "gst-plugins-good-1.0", "gst-plugins-bad-1.0",
    "glib", "libffi", "pcre2", "proxy-libintl", "orc", "libjpeg-turbo", "openh264", "zlib",
    "mingw-runtime"
)
foreach ($component in $components) {
    if (Test-Path "$licenseSource\$component") {
        Copy-Item -Recurse "$licenseSource\$component" "$stage\licenses\$component"
    }
}
# Rust crates in the executables (cargo-deny allows only permissive licences).
$notices = Join-Path $stage "licenses\rust-crates.txt"
cargo tree -p var-app -p var-check --target $rustTarget -e normal --prefix none --format "{p}: {l}" |
    Sort-Object -Unique | Where-Object { $_ -notmatch '\(\*\)' } | Set-Content -Encoding utf8 $notices
Copy-Item "packaging\windows\THIRD-PARTY-NOTICES.txt" $stage

$msi = Join-Path $OutDir "var-for-trampoline-$version-$Arch.msi"
wix build packaging\windows\var.wxs -arch $Arch -bindpath "stage=$stage" -d "Version=$version" -o $msi
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }

$sizeMb = [math]::Round((Get-Item $msi).Length / 1MB)
Write-Host "Built $msi ($sizeMb MB)"
