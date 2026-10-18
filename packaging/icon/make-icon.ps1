# Draws the application icon: a trampoline with a red dot above it (the athlete in the
# air, and the recording dot) on a dark blue rounded square. Writes
#   assets\var.ico      all sizes, embedded in var.exe (Windows) and used by the MSI
#   assets\var-256.png  window icon
#
# Usage (from the repository root, Windows):
#   powershell -ExecutionPolicy Bypass -File packaging\icon\make-icon.ps1

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
$repo = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$assets = Join-Path $repo "assets"
New-Item -ItemType Directory -Force $assets | Out-Null

function Draw-Icon([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    # Geometry on a 256 grid, scaled to the size.
    $k = $size / 256.0
    $g.ScaleTransform($k, $k)

    # Rounded square with a vertical gradient.
    $radius = 52
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc(8, 8, 2 * $radius, 2 * $radius, 180, 90)
    $path.AddArc(248 - 2 * $radius, 8, 2 * $radius, 2 * $radius, 270, 90)
    $path.AddArc(248 - 2 * $radius, 248 - 2 * $radius, 2 * $radius, 2 * $radius, 0, 90)
    $path.AddArc(8, 248 - 2 * $radius, 2 * $radius, 2 * $radius, 90, 90)
    $path.CloseFigure()
    $top = [System.Drawing.Color]::FromArgb(255, 0x24, 0x47, 0x78)
    $bottom = [System.Drawing.Color]::FromArgb(255, 0x12, 0x22, 0x3C)
    $fill = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.PointF 0, 8), (New-Object System.Drawing.PointF 0, 248), $top, $bottom
    $g.FillPath($fill, $path)

    # Small sizes: thicker strokes, so the trampoline stays visible.
    $stroke = if ($size -le 24) { 24 } elseif ($size -le 48) { 18 } else { 14 }
    $white = [System.Drawing.Color]::FromArgb(255, 0xF2, 0xF5, 0xF8)
    $pen = New-Object System.Drawing.Pen $white, $stroke
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    # Trampoline seen from the side: straight bed on two splayed legs.
    $g.DrawLine($pen, 40, 172, 216, 172)
    $g.DrawLine($pen, 66, 172, 50, 218)
    $g.DrawLine($pen, 190, 172, 206, 218)

    # Red dot: athlete in the air / recording.
    $red = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 0xE5, 0x39, 0x35))
    $g.FillEllipse($red, 94, 34, 68, 68)
    if ($size -ge 32) {
        # Motion lines between athlete and bed: the jump.
        $motion = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(150, 0xF2, 0xF5, 0xF8)), ([math]::Max(8, $stroke - 4))
        $motion.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
        $motion.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
        $g.DrawLine($motion, 106, 118, 106, 146)
        $g.DrawLine($motion, 128, 114, 128, 152)
        $g.DrawLine($motion, 150, 118, 150, 146)
    }
    if ($size -ge 48) {
        # Soft highlight on larger sizes.
        $shine = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(70, 255, 255, 255))
        $g.FillEllipse($shine, 110, 44, 26, 18)
    }
    $g.Dispose()
    return $bmp
}

function Png-Bytes($bmp) {
    $stream = New-Object System.IO.MemoryStream
    $bmp.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    # The comma keeps PowerShell from unrolling the byte array.
    return , $stream.ToArray()
}

# Classic icon image: BITMAPINFOHEADER, 32-bit BGRA rows bottom-up, empty AND mask
# (transparency comes from the alpha channel). Understood by every Windows component.
function Dib-Bytes($bmp) {
    $size = $bmp.Width
    $rect = New-Object System.Drawing.Rectangle 0, 0, $size, $size
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $pixels = New-Object byte[] ($size * $size * 4)
    for ($y = 0; $y -lt $size; $y++) {
        $row = [IntPtr]::Add($data.Scan0, $y * $data.Stride)
        [System.Runtime.InteropServices.Marshal]::Copy($row, $pixels, ($size - 1 - $y) * $size * 4, $size * 4)
    }
    $bmp.UnlockBits($data)
    $maskRow = [int]([math]::Ceiling($size / 32.0) * 4)
    $stream = New-Object System.IO.MemoryStream
    $w = New-Object System.IO.BinaryWriter $stream
    $w.Write([uint32]40); $w.Write([int32]$size); $w.Write([int32]($size * 2))
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]0)
    $w.Write([uint32]($pixels.Length + $maskRow * $size))
    $w.Write([int32]0); $w.Write([int32]0); $w.Write([uint32]0); $w.Write([uint32]0)
    $w.Write($pixels)
    $w.Write((New-Object byte[] ($maskRow * $size)))
    $w.Flush()
    return , $stream.ToArray()
}

# ICO file: header, directory, images (256 px as PNG, the others as bitmaps).
$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$images = New-Object 'System.Collections.Generic.List[byte[]]'
foreach ($size in $sizes) {
    $bmp = Draw-Icon $size
    if ($size -ge 256) { $images.Add([byte[]](Png-Bytes $bmp)) } else { $images.Add([byte[]](Dib-Bytes $bmp)) }
    if ($size -eq 256) { $bmp.Save((Join-Path $assets "var-256.png"), [System.Drawing.Imaging.ImageFormat]::Png) }
    $bmp.Dispose()
}
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$images[$i].Length); $w.Write([uint32]$offset)
    $offset += $images[$i].Length
}
foreach ($image in $images) { $w.Write([byte[]]$image) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $assets "var.ico"), $out.ToArray())
Write-Host "Wrote $assets\var.ico ($($sizes -join ', ') px) and var-256.png"
