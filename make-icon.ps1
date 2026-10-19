# 从 Mac 端 AppIcon 源图生成 Windows 多尺寸 devkit.ico。
# 用法：powershell -ExecutionPolicy Bypass -File make-icon.ps1
# 默认源图：devkit-windows\crates\app\assets\devkit-icon-source.png（取自 Macos-Developer-Toolkit AppIcon.appiconset icon_512x512.png）

param(
    [string]$Source = (Join-Path $PSScriptRoot 'devkit-windows\crates\app\assets\devkit-icon-source.png'),
    [string]$Out = (Join-Path $PSScriptRoot 'devkit-windows\crates\app\assets\devkit.ico')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$sizes = @(16, 24, 32, 48, 64, 128, 256)

$src = [System.Drawing.Bitmap]::new($Source)
if ($src.Width -ne $src.Height) { throw "源图不是正方形：$($src.Width)x$($src.Height)" }

# 缩放到指定尺寸（高质量插值，保留透明通道）
function Resize ([System.Drawing.Bitmap]$from, [int]$side) {
    $bmp = [System.Drawing.Bitmap]::new($side, $side, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.DrawImage($from, 0, 0, $side, $side)
    $g.Dispose()
    return $bmp
}

# 32bpp BGRA 自下而上像素数据（ICO 的 BMP 条目格式）+ 全 0 AND 掩码（透明度走 alpha 通道）
function BmpEntry ([System.Drawing.Bitmap]$bmp) {
    $side = $bmp.Width
    $rect = [System.Drawing.Rectangle]::new(0, 0, $side, $side)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $stride = $data.Stride
    $pixels = [byte[]]::new($stride * $side)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $pixels, 0, $pixels.Length)
    $bmp.UnlockBits($data)

    $xor = [byte[]]::new($side * $side * 4)
    for ($y = 0; $y -lt $side; $y++) {
        $srcRow = $y * $stride
        $dstRow = ($side - 1 - $y) * $side * 4   # DIB 自下而上
        [Array]::Copy($pixels, $srcRow, $xor, $dstRow, $side * 4)
    }
    $maskRow = [Math]::Ceiling($side / 8.0)
    $maskRow = [Math]::Ceiling($maskRow / 4.0) * 4
    $and = [byte[]]::new($maskRow * $side)       # 全 0 = 不透明（以 alpha 为准）

    $ms = [System.IO.MemoryStream]::new()
    $bw = [System.IO.BinaryWriter]::new($ms)
    # BITMAPINFOHEADER（biHeight = XOR + AND 两倍高）
    $bw.Write([uint32]40); $bw.Write([int32]$side); $bw.Write([int32]($side * 2))
    $bw.Write([uint16]1); $bw.Write([uint16]32); $bw.Write([uint32]0)
    $bw.Write([uint32]($xor.Length + $and.Length)); $bw.Write([int32]0)
    $bw.Write([int32]0); $bw.Write([uint32]0); $bw.Write([uint32]0)
    $bw.Write($xor); $bw.Write($and)
    $bw.Flush()
    return ,$ms.ToArray()   # 逗号防止 PowerShell 把 byte[] 拆成管道流
}

function PngEntry ([System.Drawing.Bitmap]$bmp) {
    $ms = [System.IO.MemoryStream]::new()
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    return ,$ms.ToArray()
}

$entries = @()
foreach ($s in $sizes) {
    $resized = Resize $src $s
    if ($s -le 64) { $img = [byte[]](BmpEntry $resized) } else { $img = [byte[]](PngEntry $resized) }
    $entries += [pscustomobject]@{ Side = $s; Data = $img }
    $resized.Dispose()
}
$src.Dispose()

# 组装 ICO：ICONDIR(6) + 目录项(16×N) + 图像数据
$ms = [System.IO.MemoryStream]::new()
$bw = [System.IO.BinaryWriter]::new($ms)
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$entries.Count)
$offset = 6 + 16 * $entries.Count
foreach ($e in $entries) {
    $sideByte = if ($e.Side -ge 256) { 0 } else { $e.Side }
    $bw.Write([byte]$sideByte); $bw.Write([byte]$sideByte)
    $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([uint16]1); $bw.Write([uint16]32)
    $bw.Write([uint32]$e.Data.Length)
    $bw.Write([uint32]$offset)
    $offset += $e.Data.Length
}
foreach ($e in $entries) { $bw.Write($e.Data) }
$bw.Flush()
[System.IO.File]::WriteAllBytes($Out, $ms.ToArray())
$bw.Dispose(); $ms.Dispose()
Write-Host "已生成 $Out（$($sizes -join '/')）"
