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
        # 失敗した実行を後から調べられるよう、出力を毎回 tmp\test-logs へ残す（成功した実行も残す）。
        $logDirectory = Join-Path $repoRoot 'tmp\test-logs'
        New-Item -ItemType Directory -Force -Path $logDirectory | Out-Null
        $mode = if ($IncludeIgnored) { 'test-full' } else { 'test' }
        $log = Join-Path $logDirectory ("$mode-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
        $revision = (& git -C $repoRoot rev-parse --short HEAD 2>$null)
        $changes = @(& git -C $repoRoot status --porcelain 2>$null).Count
        @(
            "date: $(Get-Date -Format o)"
            "command: cargo test --workspace --locked --no-fail-fast $($ExtraArguments -join ' ') $($harness -join ' ')"
            "git: $revision（未コミットの変更 $changes 件）"
            "rustc: $(& rustc -V)"
            "host: $env:COMPUTERNAME / PowerShell $($PSVersionTable.PSVersion)"
            ''
        ) | Set-Content -LiteralPath $log -Encoding utf8
        # 1 つの試験 binary が失敗しても残りを走らせ、失敗を全てログに残す。
        & cargo test --workspace --locked --no-fail-fast @ExtraArguments @harness 2>&1 |
            Tee-Object -FilePath $log -Append | Tee-Object -Variable output | Out-Host
        $exitCode = $LASTEXITCODE
        Add-Content -LiteralPath $log -Value "`nexit code: $exitCode" -Encoding utf8
        if ($exitCode -ne 0) {
            # 長い出力の途中に埋もれた失敗を、末尾にまとめて出す。
            $failures = @($output | ForEach-Object { "$_" } | Where-Object {
                    $_ -match '^test .+ FAILED$' -or $_ -match '^---- .+ ----$' -or
                    $_ -match 'panicked at' -or $_ -match '^error: ' -or
                    $_ -match "process didn't exit successfully"
                })
            [Console]::Error.WriteLine("試験が失敗しました（exit code $exitCode）。ログ: $log")
            foreach ($line in $failures) {
                [Console]::Error.WriteLine("  $line")
            }
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
        Write-Output "ログ: $log"
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
