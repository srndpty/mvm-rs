<#
.SYNOPSIS
    cargo が FFmpeg と libclang を見つけるための環境変数を、このプロセスへ設定する。

.DESCRIPTION
    取得や検証はしない。無いものは設定せず、呼び出し側が失敗させる。
    dev.ps1 と setup.ps1 がここだけを使う。
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$ffmpegRoot = Join-Path $repoRoot 'third_party\ffmpeg'

if (Test-Path -LiteralPath (Join-Path $ffmpegRoot 'include\libavcodec\avcodec.h')) {
    $env:FFMPEG_DIR = $ffmpegRoot
    $ffmpegBin = Join-Path $ffmpegRoot 'bin'
    if (Test-Path -LiteralPath $ffmpegBin) {
        $env:PATH = "$ffmpegBin;$env:PATH"
    }
}

function Find-LibClangDirectory {
    $candidates = @()
    if ($env:LIBCLANG_PATH) {
        $candidates += $env:LIBCLANG_PATH
    }
    $candidates += 'C:\Program Files\LLVM\bin'
    $candidates += (Join-Path $env:LOCALAPPDATA 'LLVM\bin')
    foreach ($dir in $candidates) {
        if ($dir -and (Test-Path -LiteralPath (Join-Path $dir 'libclang.dll'))) {
            return $dir
        }
    }
    return $null
}

$libclang = Find-LibClangDirectory
if ($libclang) {
    $env:LIBCLANG_PATH = $libclang
}
