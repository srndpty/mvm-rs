<#
.SYNOPSIS
    mvm の lint を一括で実行する。

.DESCRIPTION
    順に実行し、1 つでも失敗すれば終了コード 1 を返す（途中で止めず、全件の結果を出す）。
      1. rustfmt の整形差分 (cargo fmt --check)
      2. clippy (警告もエラー扱い)
      3. アーキテクチャ検査 (cargo xtask arch: crate の依存方向・unsafe を許可する crate)
      4. PSScriptAnalyzer (固定バージョンとリポジトリ設定を使用)
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$failed = @()

function Write-Section {
    param([Parameter(Mandatory)][string]$Title)
    Write-Host ''
    Write-Host "=== $Title ===" -ForegroundColor Cyan
}

function Invoke-Step {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Action
    )
    Write-Section $Name
    & $Action
    if ($LASTEXITCODE -ne 0) {
        $script:failed += $Name
    } else {
        Write-Host 'OK' -ForegroundColor Green
    }
}

Push-Location $repoRoot
try {
    Invoke-Step -Name 'rustfmt' -Action { cargo fmt --all --check }
    Invoke-Step -Name 'clippy' -Action {
        cargo clippy --workspace --all-targets --locked -- -D warnings
    }
    Invoke-Step -Name 'arch' -Action { cargo xtask arch }

    # --- PSScriptAnalyzer ---------------------------------------------------
    Write-Section 'PSScriptAnalyzer'
    $versionFile = Join-Path $PSScriptRoot 'PSScriptAnalyzer.version'
    $requiredVersion = [version](Get-Content -Raw -LiteralPath $versionFile).Trim()
    $analyzer = Get-Module -ListAvailable -Name PSScriptAnalyzer |
        Where-Object { $_.Version -eq $requiredVersion } |
        Select-Object -First 1
    if (-not $analyzer) {
        Write-Host "必要な PSScriptAnalyzer $requiredVersion がありません。" -ForegroundColor Red
        Write-Host "導入: Install-Module PSScriptAnalyzer -RequiredVersion $requiredVersion -Scope CurrentUser" -ForegroundColor Yellow
        $failed += 'PSScriptAnalyzer'
    } else {
        # 対話シェルに別バージョンの型情報が残っていても衝突しないよう、
        # Analyzer は毎回クリーンな子プロセスで実行する。
        $pwsh = (Get-Process -Id $PID).Path
        & $pwsh -NoProfile -NonInteractive -File (Join-Path $PSScriptRoot 'run-script-analyzer.ps1') `
            -RepoRoot $repoRoot `
            -SettingsPath (Join-Path $PSScriptRoot 'PSScriptAnalyzerSettings.psd1') `
            -RequiredVersion $requiredVersion
        if ($LASTEXITCODE -ne 0) {
            $failed += 'PSScriptAnalyzer'
        }
    }
} finally {
    Pop-Location
}

Write-Host ''
if ($failed.Count -gt 0) {
    Write-Host "lint 失敗: $($failed -join ', ')" -ForegroundColor Red
    exit 1
}
Write-Host 'lint 通過' -ForegroundColor Green
# 明示的に 0 を返す。これが無いと $LASTEXITCODE に前の値が残り、呼び出し側が誤判定する。
exit 0
