[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$RepoRoot,

    [Parameter(Mandatory)]
    [string]$SettingsPath,

    [Parameter(Mandatory)]
    [version]$RequiredVersion
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# 生成物と外部取得物は解析しない。
$excludedTopDirectories = @('target', '.git', 'third_party', 'tests\assets')

try {
    Import-Module PSScriptAnalyzer -RequiredVersion $RequiredVersion
    $rootPrefix = (Resolve-Path -LiteralPath $RepoRoot).Path.TrimEnd('\') + '\'
    $scripts = @(Get-ChildItem -LiteralPath $RepoRoot -Recurse -File -Filter '*.ps1' |
        Where-Object {
            $relative = $_.FullName.Substring($rootPrefix.Length)
            -not ($excludedTopDirectories | Where-Object { $relative.StartsWith("$_\") })
        })
    if ($scripts.Count -eq 0) {
        Write-Error '解析対象の .ps1 がありません'
        exit 2
    }
    $issues = @(foreach ($script in $scripts) {
        Invoke-ScriptAnalyzer -Path $script.FullName -Settings $SettingsPath -Severity Error, Warning
    })
} catch {
    Write-Error "PSScriptAnalyzer $RequiredVersion の実行に失敗しました: $($_.Exception.Message)"
    exit 2
}

if ($issues.Count -ne 0) {
    $issues | ForEach-Object {
        Write-Output ("  {0}:{1} [{2}] {3}" -f (Split-Path $_.ScriptPath -Leaf), $_.Line,
                                                $_.RuleName, $_.Message)
    }
    exit 1
}

Write-Output "OK (PSScriptAnalyzer $RequiredVersion, $($scripts.Count) files)"
exit 0
