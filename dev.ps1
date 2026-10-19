<#
.SYNOPSIS
    mvm の開発用コマンドを統一された入口から実行する。

.DESCRIPTION
    cargo と scripts/ の正式スクリプトへの薄い入口。判定条件やビルド処理はこのファイルへ重複実装しない。
    command の後ろの引数はそのまま cargo へ渡す（例: .\dev.ps1 test -p mvm-core）。

.EXAMPLE
    .\dev.ps1 build
    .\dev.ps1 run
    .\dev.ps1 test
    .\dev.ps1 lint
    .\dev.ps1 help
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$Command = 'help',

    [Parameter(Position = 1, ValueFromRemainingArguments)]
    [string[]]$CargoArgs = @()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = $PSScriptRoot
$scriptsDirectory = Join-Path $repoRoot 'scripts'
. (Join-Path $scriptsDirectory 'env.ps1')

function Show-DevHelp {
    Write-Output @'
使い方: .\dev.ps1 <command> [cargo への追加引数]

利用可能なコマンド:
  build      debug ビルド（--release を付けると release）
  run        debug の mvm をビルドして起動する
  gui        release の mvm をビルドして起動する
  test       試験を実行する（workstation 試験は除外。0 件なら失敗）
  test-full  workstation 試験も含めて実行する（0 件なら失敗）
  setup      FFmpeg gpl-shared の取得と libclang の確認
  testmedia  検証素材を生成する（既定は Smoke）
  lint       整形差分・clippy・アーキテクチャ検査・PSScriptAnalyzer を実行する
  fmt        rustfmt で整形する
  check      型検査（cargo check --all-targets）
  clean      ビルド成果物を消す
  help       このヘルプを表示する
'@
}

function Invoke-Cargo {
    param(
        [Parameter(Mandatory)][string[]]$Arguments,
        [string[]]$ExtraArguments = @()
    )

    Push-Location $repoRoot
    try {
        & cargo @Arguments @ExtraArguments
        exit $LASTEXITCODE
    } finally {
        Pop-Location
    }
}

function Assert-Toolchain {
    if (-not $env:FFMPEG_DIR -or -not (Test-Path -LiteralPath (Join-Path $env:FFMPEG_DIR 'include\libavcodec\avcodec.h'))) {
        [Console]::Error.WriteLine('FFmpeg がありません。先に .\dev.ps1 setup を実行してください。')
        exit 1
    }
    if (-not $env:LIBCLANG_PATH) {
        [Console]::Error.WriteLine('libclang がありません。先に .\dev.ps1 setup を実行してください。')
        exit 1
    }
}

function Invoke-Tests {
    param(
        [string[]]$ExtraArguments = @(),
        [switch]$IncludeIgnored
    )

    Push-Location $repoRoot
    try {
        $harness = @()
        $skipped = @()
        if ($IncludeIgnored) {
            $harness = @('--', '--include-ignored')
            # C++ 版の .mvm 群は手元にしか無い。指定が無ければ実行せず、skipped として報告する。
            if (-not $env:MVM_CPP_PROJECT_CORPUS) {
                $harness += @('--skip', 'reads_cpp_project_corpus')
                $skipped += 'reads_cpp_project_corpus（MVM_CPP_PROJECT_CORPUS が未指定）'
            }
            # 認識 model は数 GB あり手元にしか無い。tool・model・音声の指定が無ければ実行しない。
            if (-not ($env:MVM_WHISPER_CLI -and $env:MVM_WHISPER_MODEL -and $env:MVM_TRANSCRIBE_AUDIO)) {
                $harness += @('--skip', 'recognizes_speech_with_the_real_whisper')
                $skipped += 'recognizes_speech_with_the_real_whisper（MVM_WHISPER_CLI・MVM_WHISPER_MODEL・MVM_TRANSCRIBE_AUDIO が未指定）'
            }
        }
        & cargo test --workspace --locked @ExtraArguments @harness 2>&1 |
            Tee-Object -Variable output | Out-Host
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            exit $exitCode
        }
        $passed = 0
        foreach ($line in $output) {
            if ("$line" -match '^test result: ok\. (\d+) passed') {
                $passed += [int]$Matches[1]
            }
        }
        if ($passed -eq 0) {
            [Console]::Error.WriteLine('試験が 1 件も実行されませんでした（0 件は失敗として扱う）')
            exit 1
        }
        Write-Output "通過した試験: $passed 件"
        foreach ($name in $skipped) {
            Write-Output "skipped: $name"
        }
        exit 0
    } finally {
        Pop-Location
    }
}

switch ($Command.ToLowerInvariant()) {
    'build' {
        Assert-Toolchain
        Invoke-Cargo -Arguments @('build', '--workspace', '--locked') -ExtraArguments $CargoArgs
    }
    'run' {
        Assert-Toolchain
        Invoke-Cargo -Arguments @('run', '--package', 'mvm', '--locked') -ExtraArguments $CargoArgs
    }
    'gui' {
        Assert-Toolchain
        Invoke-Cargo -Arguments @('run', '--package', 'mvm', '--release', '--locked') -ExtraArguments $CargoArgs
    }
    'test' {
        Assert-Toolchain
        Invoke-Tests -ExtraArguments $CargoArgs
    }
    'test-full' {
        Assert-Toolchain
        Invoke-Tests -IncludeIgnored -ExtraArguments $CargoArgs
    }
    'setup' {
        & (Get-Process -Id $PID).Path -NoProfile -File (Join-Path $scriptsDirectory 'setup.ps1')
        exit $LASTEXITCODE
    }
    'testmedia' {
        Assert-Toolchain
        & (Get-Process -Id $PID).Path -NoProfile -File (Join-Path $scriptsDirectory 'make-testmedia.ps1') @CargoArgs
        exit $LASTEXITCODE
    }
    'lint' {
        Assert-Toolchain
        $pwsh = (Get-Process -Id $PID).Path
        & $pwsh -NoProfile -File (Join-Path $scriptsDirectory 'lint.ps1')
        exit $LASTEXITCODE
    }
    'fmt' { Invoke-Cargo -Arguments @('fmt', '--all') -ExtraArguments $CargoArgs }
    'check' {
        Assert-Toolchain
        Invoke-Cargo -Arguments @('check', '--workspace', '--all-targets', '--locked') -ExtraArguments $CargoArgs
    }
    'clean' { Invoke-Cargo -Arguments @('clean') -ExtraArguments $CargoArgs }
    'help' {
        Show-DevHelp
        exit 0
    }
    default {
        [Console]::Error.WriteLine("不明な dev コマンドです: $Command")
        Show-DevHelp
        exit 2
    }
}
