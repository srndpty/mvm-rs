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

function Show-DevHelp {
    Write-Output @'
使い方: .\dev.ps1 <command> [cargo への追加引数]

利用可能なコマンド:
  build      debug ビルド（--release を付けると release）
  run        debug の mvm をビルドして起動する
  gui        release の mvm をビルドして起動する
  test       試験を実行する（workstation 試験は除外。0 件なら失敗）
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

function Invoke-Tests {
    param([string[]]$ExtraArguments = @())

    Push-Location $repoRoot
    try {
        & cargo test --workspace --locked @ExtraArguments 2>&1 |
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
        exit 0
    } finally {
        Pop-Location
    }
}

switch ($Command.ToLowerInvariant()) {
    'build' { Invoke-Cargo -Arguments @('build', '--workspace', '--locked') -ExtraArguments $CargoArgs }
    'run' { Invoke-Cargo -Arguments @('run', '--package', 'mvm', '--locked') -ExtraArguments $CargoArgs }
    'gui' { Invoke-Cargo -Arguments @('run', '--package', 'mvm', '--release', '--locked') -ExtraArguments $CargoArgs }
    'test' { Invoke-Tests -ExtraArguments $CargoArgs }
    'lint' {
        $pwsh = (Get-Process -Id $PID).Path
        & $pwsh -NoProfile -File (Join-Path $scriptsDirectory 'lint.ps1')
        exit $LASTEXITCODE
    }
    'fmt' { Invoke-Cargo -Arguments @('fmt', '--all') -ExtraArguments $CargoArgs }
    'check' { Invoke-Cargo -Arguments @('check', '--workspace', '--all-targets', '--locked') -ExtraArguments $CargoArgs }
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
