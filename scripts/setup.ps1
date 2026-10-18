<#
.SYNOPSIS
    BtbN の FFmpeg n8.1 gpl-shared を取得し、libclang があることを確認する。

.DESCRIPTION
    版と SHA256 は scripts/ffmpeg.pin.json が正本。一致しない archive は展開しない。
    libclang が無ければ winget の LLVM.LLVM を導入する。導入できなければ失敗させる。
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$pinPath = Join-Path $PSScriptRoot 'ffmpeg.pin.json'
$pin = Get-Content -Raw -LiteralPath $pinPath | ConvertFrom-Json
$expected = $pin.sha256.ToLowerInvariant()
if ($expected -notmatch '^[0-9a-f]{64}$') {
    throw "ffmpeg.pin.json の sha256 が不正です: $($pin.sha256)"
}

$ffmpegRoot = Join-Path $repoRoot 'third_party\ffmpeg'
$stampPath = Join-Path $ffmpegRoot 'SOURCE.txt'
$header = Join-Path $ffmpegRoot 'include\libavcodec\avcodec.h'
$already = $false
if ((Test-Path -LiteralPath $header) -and (Test-Path -LiteralPath $stampPath)) {
    $stamp = Get-Content -Raw -LiteralPath $stampPath
    if ($stamp -match [regex]::Escape($expected)) {
        $already = $true
        Write-Host "FFmpeg $($pin.version) は取得済みです" -ForegroundColor Green
    }
}

if (-not $already) {
    New-Item -ItemType Directory -Force -Path $ffmpegRoot | Out-Null
    $zip = Join-Path $ffmpegRoot $pin.file
    Write-Host "FFmpeg を取得します: $($pin.version)" -ForegroundColor Cyan
    Invoke-WebRequest -Uri $pin.url -OutFile $zip -UseBasicParsing
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        Remove-Item -LiteralPath $zip -Force
        throw "SHA256 が一致しません。期待 $expected、実際 $actual。archive は破棄しました。"
    }

    $staging = Join-Path $ffmpegRoot '_extract'
    if (Test-Path -LiteralPath $staging) {
        Remove-Item -LiteralPath $staging -Recurse -Force
    }
    Expand-Archive -LiteralPath $zip -DestinationPath $staging -Force
    $root = @(Get-ChildItem -LiteralPath $staging -Directory) | Select-Object -First 1
    if (-not $root) {
        throw "展開結果に directory がありません: $staging"
    }
    foreach ($name in @('bin', 'include', 'lib')) {
        $source = Join-Path $root.FullName $name
        if (-not (Test-Path -LiteralPath $source)) {
            throw "展開結果に $name がありません: $($root.FullName)"
        }
        $target = Join-Path $ffmpegRoot $name
        if (Test-Path -LiteralPath $target) {
            Remove-Item -LiteralPath $target -Recurse -Force
        }
        Move-Item -LiteralPath $source -Destination $target
    }
    Remove-Item -LiteralPath $staging -Recurse -Force
    Remove-Item -LiteralPath $zip -Force
    @"
version=$($pin.version)
sha256=$expected
url=$($pin.url)
"@ | Set-Content -LiteralPath $stampPath -Encoding utf8
    Write-Host "FFmpeg を展開しました: $ffmpegRoot" -ForegroundColor Green
}

. (Join-Path $PSScriptRoot 'env.ps1')
if (-not $env:LIBCLANG_PATH) {
    Write-Host 'libclang が無いので LLVM.LLVM を導入します' -ForegroundColor Cyan
    winget install --id LLVM.LLVM -e --accept-package-agreements --accept-source-agreements --disable-interactivity
    if ($LASTEXITCODE -ne 0) {
        throw "winget で LLVM.LLVM を導入できませんでした (exit $LASTEXITCODE)。libclang.dll を LIBCLANG_PATH に置いてから再度 dev setup を実行してください。"
    }
    . (Join-Path $PSScriptRoot 'env.ps1')
}
if (-not $env:LIBCLANG_PATH) {
    throw 'libclang.dll が見つかりません。LLVM.LLVM の導入後も C:\Program Files\LLVM\bin にありません。'
}
if (-not (Test-Path -LiteralPath $header)) {
    throw "FFmpeg の header がありません: $header"
}

Write-Host "FFMPEG_DIR    = $env:FFMPEG_DIR"
Write-Host "LIBCLANG_PATH = $env:LIBCLANG_PATH"
Write-Host 'setup 完了' -ForegroundColor Green
