@{
    # 端末出力を目的とする運用スクリプトなので Write-Host は意図的に使う。
    # リポジトリの文字コードは UTF-8 (BOM なし) に統一している。
    # 公開モジュールではないため、cmdlet の命名規約と ShouldProcess は適用しない。
    ExcludeRules = @(
        'PSAvoidUsingWriteHost'
        'PSUseBOMForUnicodeEncodedFile'
        'PSUseApprovedVerbs'
        'PSUseShouldProcessForStateChangingFunctions'
        'PSUseSingularNouns'
    )
}
