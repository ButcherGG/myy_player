# FFmpeg 问题诊断和修复脚本
# PowerShell 脚本

Write-Host "=== MYY Player FFmpeg 问题诊断 ===" -ForegroundColor Cyan

# 设置 UTF-8 编码
$OutputEncoding = [Console]::OutputEncoding = [Text.UTF8Encoding]::UTF8

# 1. 检查 FFMPEG_DIR 环境变量
Write-Host "`n[1] 检查 FFMPEG_DIR 环境变量..." -ForegroundColor Yellow
if ($env:FFMPEG_DIR) {
    Write-Host "✓ FFMPEG_DIR = $env:FFMPEG_DIR" -ForegroundColor Green
    
    # 检查目录是否存在
    if (Test-Path $env:FFMPEG_DIR) {
        Write-Host "✓ 目录存在" -ForegroundColor Green
    } else {
        Write-Host "✗ 错误: 目录不存在!" -ForegroundColor Red
        Write-Host "请确认 FFmpeg 安装路径" -ForegroundColor Yellow
        exit 1
    }
} else {
    Write-Host "✗ 错误: FFMPEG_DIR 未设置" -ForegroundColor Red
    Write-Host "`n建议操作:" -ForegroundColor Yellow
    Write-Host "1. 下载 FFmpeg: https://github.com/BtbN/FFmpeg-Builds/releases" -ForegroundColor White
    Write-Host "2. 解压到 C:\ffmpeg" -ForegroundColor White
    Write-Host "3. 运行: `$env:FFMPEG_DIR = 'C:\ffmpeg'" -ForegroundColor White
    Write-Host "4. 运行: `$env:PATH += ';C:\ffmpeg\bin'" -ForegroundColor White
    exit 1
}

# 2. 检查必需的文件
Write-Host "`n[2] 检查 FFmpeg 文件结构..." -ForegroundColor Yellow

$requiredDirs = @("include", "lib", "bin")
$allDirsExist = $true

foreach ($dir in $requiredDirs) {
    $path = Join-Path $env:FFMPEG_DIR $dir
    if (Test-Path $path) {
        Write-Host "✓ $dir/ 目录存在" -ForegroundColor Green
    } else {
        Write-Host "✗ $dir/ 目录不存在" -ForegroundColor Red
        $allDirsExist = $false
    }
}

if (-not $allDirsExist) {
    Write-Host "`n✗ 错误: FFmpeg 目录结构不完整" -ForegroundColor Red
    Write-Host "请确保 FFmpeg 是完整的开发包（包含 include, lib, bin 目录）" -ForegroundColor Yellow
    exit 1
}

# 3. 检查头文件
Write-Host "`n[3] 检查 FFmpeg 头文件..." -ForegroundColor Yellow

$requiredHeaders = @(
    "libavcodec\avcodec.h",
    "libavformat\avformat.h",
    "libavutil\avutil.h",
    "libswscale\swscale.h",
    "libswresample\swresample.h"
)

$allHeadersExist = $true
foreach ($header in $requiredHeaders) {
    $path = Join-Path $env:FFMPEG_DIR "include\$header"
    if (Test-Path $path) {
        Write-Host "✓ $header" -ForegroundColor Green
    } else {
        Write-Host "✗ $header 不存在" -ForegroundColor Red
        $allHeadersExist = $false
    }
}

if (-not $allHeadersExist) {
    Write-Host "`n✗ 错误: 缺少必需的头文件" -ForegroundColor Red
    exit 1
}

# 4. 检查库文件
Write-Host "`n[4] 检查 FFmpeg 库文件..." -ForegroundColor Yellow

$requiredLibs = @(
    "avcodec.lib",
    "avformat.lib",
    "avutil.lib",
    "swscale.lib",
    "swresample.lib",
    "avfilter.lib",
    "avdevice.lib"
)

$allLibsExist = $true
foreach ($lib in $requiredLibs) {
    $path = Join-Path $env:FFMPEG_DIR "lib\$lib"
    if (Test-Path $path) {
        Write-Host "✓ $lib" -ForegroundColor Green
    } else {
        Write-Host "✗ $lib 不存在" -ForegroundColor Red
        $allLibsExist = $false
    }
}

if (-not $allLibsExist) {
    Write-Host "`n✗ 错误: 缺少必需的库文件" -ForegroundColor Red
    Write-Host "`n可能原因:" -ForegroundColor Yellow
    Write-Host "1. 下载的不是开发包（dev 版本）" -ForegroundColor White
    Write-Host "2. 下载的是错误的架构（需要 x64）" -ForegroundColor White
    Write-Host "`n请下载正确的包:" -ForegroundColor Yellow
    Write-Host "ffmpeg-n6.0-latest-win64-gpl-shared-6.0.zip" -ForegroundColor White
    exit 1
}

# 5. 检查 DLL 文件
Write-Host "`n[5] 检查 FFmpeg DLL 文件..." -ForegroundColor Yellow

$requiredDlls = @(
    "avcodec-60.dll",
    "avformat-60.dll",
    "avutil-58.dll",
    "swscale-7.dll",
    "swresample-4.dll"
)

$missingDlls = @()
foreach ($dll in $requiredDlls) {
    $path = Join-Path $env:FFMPEG_DIR "bin\$dll"
    if (Test-Path $path) {
        Write-Host "✓ $dll" -ForegroundColor Green
    } else {
        Write-Host "⚠ $dll 不存在（可能版本号不同）" -ForegroundColor Yellow
        $missingDlls += $dll
    }
}

# 6. 检查 PATH 环境变量
Write-Host "`n[6] 检查 PATH 环境变量..." -ForegroundColor Yellow
$ffmpegBin = Join-Path $env:FFMPEG_DIR "bin"
if ($env:PATH -like "*$ffmpegBin*") {
    Write-Host "✓ FFmpeg bin 目录已在 PATH 中" -ForegroundColor Green
} else {
    Write-Host "⚠ FFmpeg bin 目录不在 PATH 中" -ForegroundColor Yellow
    Write-Host "正在添加到 PATH..." -ForegroundColor Yellow
    $env:PATH += ";$ffmpegBin"
    Write-Host "✓ 已添加（临时）" -ForegroundColor Green
    Write-Host "`n建议永久添加:" -ForegroundColor Yellow
    Write-Host "[System.Environment]::SetEnvironmentVariable('PATH', `$env:PATH, 'User')" -ForegroundColor White
}

# 7. 验证 FFmpeg 可执行
Write-Host "`n[7] 验证 FFmpeg 可执行文件..." -ForegroundColor Yellow
$ffmpegExe = Join-Path $env:FFMPEG_DIR "bin\ffmpeg.exe"
if (Test-Path $ffmpegExe) {
    Write-Host "✓ ffmpeg.exe 存在" -ForegroundColor Green
    try {
        $version = & $ffmpegExe -version 2>&1 | Select-String "ffmpeg version" | Select-Object -First 1
        Write-Host "✓ 版本: $version" -ForegroundColor Green
    } catch {
        Write-Host "⚠ 无法运行 ffmpeg.exe" -ForegroundColor Yellow
    }
} else {
    Write-Host "✗ ffmpeg.exe 不存在" -ForegroundColor Red
}

# 8. 检查 pkg-config（可选）
Write-Host "`n[8] 检查 pkg-config（可选）..." -ForegroundColor Yellow
if (Get-Command pkg-config -ErrorAction SilentlyContinue) {
    Write-Host "✓ pkg-config 已安装" -ForegroundColor Green
    try {
        $pc = pkg-config --modversion libavcodec 2>&1
        Write-Host "✓ libavcodec 版本: $pc" -ForegroundColor Green
    } catch {
        Write-Host "⚠ pkg-config 找不到 FFmpeg 库" -ForegroundColor Yellow
    }
} else {
    Write-Host "ℹ pkg-config 未安装（可选，不影响编译）" -ForegroundColor Cyan
}

# 总结
Write-Host "`n=== 诊断总结 ===" -ForegroundColor Cyan

if ($allDirsExist -and $allHeadersExist -and $allLibsExist) {
    Write-Host "`n✓ FFmpeg 环境配置正确！" -ForegroundColor Green
    Write-Host "`n可以尝试编译了:" -ForegroundColor Yellow
    Write-Host "  cargo clean" -ForegroundColor White
    Write-Host "  cargo build" -ForegroundColor White
    Write-Host "`n如果还是失败，请尝试:" -ForegroundColor Yellow
    Write-Host "  1. 重新下载 FFmpeg 开发包" -ForegroundColor White
    Write-Host "  2. 确保是 shared 版本（不是 static）" -ForegroundColor White
    Write-Host "  3. 确保是 x64 版本" -ForegroundColor White
} else {
    Write-Host "`n✗ 发现问题，请按照上述提示修复" -ForegroundColor Red
}

Write-Host "`n" -ForegroundColor White

