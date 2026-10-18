<#
.SYNOPSIS
    検証素材を決定論的に生成する。

.DESCRIPTION
    FFmpeg / ffprobe は dev setup が取得した BtbN gpl-shared だけを使う。
    PATH 上の別の FFmpeg へはフォールバックしない。
    mvm がリンクする libav* と同じビルドで素材を作る。

    生成した各素材について manifest (JSON) と ffprobe の生 JSON を残す。
    映像は codec 固有の非決定要素を含むため、ファイル hash の完全一致は要求しない。
    再生成時に照合するのはメタデータと期待フレーム数である。

.PARAMETER Mode
    Smoke     : 5 秒。自動検査用。CI とローカルの毎回の実行を想定。
    Benchmark : 60 秒。S7 以降の性能計測用。

.PARAMETER OutputRoot
    出力先。既定は tests/assets/<mode>。git 管理外。

.PARAMETER Force
    既存の素材を再生成する。既定では存在するものはスキップする。

.PARAMETER VerifyRegeneration
    既存 manifest がある場合、再生成してメタデータを照合する。
    hash 不一致は情報として出すが失敗にしない。

.EXAMPLE
    pwsh scripts/make-testmedia.ps1 -Mode Smoke
    pwsh scripts/make-testmedia.ps1 -Mode Benchmark
#>
[CmdletBinding()]
param(
    [ValidateSet('Smoke', 'Benchmark')]
    [string]$Mode = 'Smoke',

    [string]$OutputRoot,
    [switch]$Force,
    [switch]$VerifyRegeneration
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
$FFmpegRoot = Join-Path $RepoRoot 'third_party\ffmpeg'
$FFmpeg   = Join-Path $FFmpegRoot 'bin\ffmpeg.exe'
$FFprobe  = Join-Path $FFmpegRoot 'bin\ffprobe.exe'

if (-not $OutputRoot) {
    $OutputRoot = Join-Path $RepoRoot "tests\assets\$($Mode.ToLower())"
}

# --- 使用する FFmpeg を固定する -------------------------------------------
# PATH 上の別 FFmpeg へフォールバックしない。見つからなければ失敗させる。
foreach ($tool in @($FFmpeg, $FFprobe)) {
    if (-not (Test-Path $tool)) {
        throw @"
dev setup が配置する FFmpeg が見つかりません: $tool

PATH 上の別の FFmpeg は使いません。mvm がリンクする libav* と同じビルドで素材を作るためです。

    .\dev.ps1 setup
"@
    }
}

$Duration = if ($Mode -eq 'Smoke') { 5 } else { 60 }
$Fps      = 60
$ExpectedFrames = $Duration * $Fps

Write-Host "=== mvm 検証素材生成 ($Mode) ===" -ForegroundColor Cyan
Write-Host "ffmpeg  : $FFmpeg"
Write-Host "出力先  : $OutputRoot"
Write-Host "尺      : $Duration 秒 / $Fps fps / 期待 $ExpectedFrames フレーム"

New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$ProbeDir = Join-Path $OutputRoot '_ffprobe'
New-Item -ItemType Directory -Force -Path $ProbeDir | Out-Null

# --- フレーム固有マーカー ---------------------------------------------------
# 仕様は docs/research/test-media-format.md を参照。
#
#   セル幅 64px、帯の高さ 64px、原点 (0,0)。
#   cell 0        : 常に白 (同期)
#   cell 1        : 常に黒 (同期)
#   cell 2..17    : フレーム番号 N の 16bit、LSB first
#   cell 18       : 常に白 (終端同期)
#   合計 19 セル = 1216px
#
# OCR は使わない。セル中心のピクセル値を閾値判定するだけで読める。
$MarkerCells = 19
$CellSize    = 64
$MarkerWidth = $MarkerCells * $CellSize   # 1216
$MarkerExpr  = "if(lt(floor(X/$CellSize),1),235," +
               "if(lt(floor(X/$CellSize),2),16," +
               "if(lt(floor(X/$CellSize),18)," +
               "if(bitand(floor(N/pow(2,floor(X/$CellSize)-2))\,1),235,16)," +
               "235)))"

# 既知の周期を持つ音声。48000Hz に対し整数周期なので完全に決定論的。
#
# 映像に埋める音声 (A1 相当) と WAV (A2 相当) で周波数を変える。
# 同じ構成にすると、片方だけが出力に含まれていても検査が通ってしまい、
# 「両方が mix された」ことを実証できない (S5 の実測で判明)。
#   A1: L 1000Hz (48 サンプル周期) / R  500Hz (96 サンプル周期)
#   A2: L 1500Hz (32 サンプル周期) / R  750Hz (64 サンプル周期)
$AudioExpr = 'aevalsrc=exprs=0.5*sin(2*PI*1000*t)|0.5*sin(2*PI*500*t):s=48000:d=' +
             "${Duration}:c=stereo"
$AudioExprA2 = 'aevalsrc=exprs=0.5*sin(2*PI*1500*t)|0.5*sin(2*PI*750*t):s=48000:d=' +
               "${Duration}:c=stereo"

function New-VideoArgs {
    param(
        [int]$Width, [int]$Height, [string]$VCodec, [string]$PixFmt,
        [string]$Preset, [string]$Output, [switch]$NoAudio,
        # 背景パターン。S5 の合成検証では V1 と V2 が視覚的に区別できる必要がある。
        # 同じ testsrc2 を使うと「全画面で重ねた」のか「縮小して重ねた」のかを
        # 画素から判定できず、検証が空振りする。
        [string]$Pattern = 'testsrc2'
    )

    $marker = "nullsrc=s=${MarkerWidth}x${CellSize}:r=${Fps}:d=${Duration}," +
              "format=gray,geq=lum='$MarkerExpr'"

    # 人間が読めるフレーム番号とタイムコード。マーカー帯の下に置く。
    $drawtext = "drawtext=fontfile='C\:/Windows/Fonts/consola.ttf'" +
                ":text='frame=%{n}  t=%{pts\:hms}':x=8:y=$($CellSize + 16)" +
                ":fontsize=36:fontcolor=white:box=1:boxcolor=black@0.7"

    $filter = "[0:v]$drawtext[b];[1:v]format=yuv420p[m];[b][m]overlay=0:0:shortest=1[v]"

    $a = @(
        '-hide_banner', '-y', '-loglevel', 'error', '-nostdin'
        '-f', 'lavfi', '-i', "${Pattern}=s=${Width}x${Height}:r=${Fps}:d=${Duration}"
        '-f', 'lavfi', '-i', $marker
    )
    if (-not $NoAudio) { $a += @('-f', 'lavfi', '-i', $AudioExpr) }

    $a += @('-filter_complex', $filter, '-map', '[v]')
    if (-not $NoAudio) { $a += @('-map', '2:a', '-c:a', 'aac', '-b:a', '192k', '-ar', '48000', '-ac', '2') }

    $a += @(
        '-c:v', $VCodec, '-preset', $Preset, '-crf', '20'
        '-pix_fmt', $PixFmt, '-r', "$Fps", '-fps_mode', 'cfr', '-g', "$Fps"
        $Output
    )
    return $a
}

# --- 素材定義 ---------------------------------------------------------------
# 4K は素材として重いので preset を落とす。完全な引数は manifest に記録される。

$Assets = @(
    @{
        Id = 'v1080p60_h264'; File = 'v1080p60_h264.mp4'; Kind = 'video'
        Expect = @{ width = 1920; height = 1080; video_codec = 'h264'; pix_fmt = 'yuv420p'
                    fps_num = 60; fps_den = 1; frames = $ExpectedFrames
                    sar_num = 1; sar_den = 1; duration_sec = $Duration
                    audio_codec = 'aac'; sample_rate = 48000; channels = 2; has_alpha = $false }
        Args = { New-VideoArgs -Width 1920 -Height 1080 -VCodec 'libx264' -PixFmt 'yuv420p' `
                               -Preset 'medium' -Output $args[0] }
    },
    @{
        Id = 'v1080p60_hevc'; File = 'v1080p60_hevc.mp4'; Kind = 'video'
        Expect = @{ width = 1920; height = 1080; video_codec = 'hevc'; pix_fmt = 'yuv420p'
                    fps_num = 60; fps_den = 1; frames = $ExpectedFrames
                    sar_num = 1; sar_den = 1; duration_sec = $Duration
                    audio_codec = 'aac'; sample_rate = 48000; channels = 2; has_alpha = $false }
        Args = { New-VideoArgs -Width 1920 -Height 1080 -VCodec 'libx265' -PixFmt 'yuv420p' `
                               -Preset 'medium' -Pattern 'smptehdbars' -Output $args[0] }
    },
    @{
        Id = 'v4k60_h264'; File = 'v4k60_h264.mp4'; Kind = 'video'
        Expect = @{ width = 3840; height = 2160; video_codec = 'h264'; pix_fmt = 'yuv420p'
                    fps_num = 60; fps_den = 1; frames = $ExpectedFrames
                    sar_num = 1; sar_den = 1; duration_sec = $Duration
                    audio_codec = 'aac'; sample_rate = 48000; channels = 2; has_alpha = $false }
        Args = { New-VideoArgs -Width 3840 -Height 2160 -VCodec 'libx264' -PixFmt 'yuv420p' `
                               -Preset 'veryfast' -Output $args[0] }
    },
    @{
        Id = 'v4k60_hevc10'; File = 'v4k60_hevc10.mp4'; Kind = 'video'
        Expect = @{ width = 3840; height = 2160; video_codec = 'hevc'; pix_fmt = 'yuv420p10le'
                    fps_num = 60; fps_den = 1; frames = $ExpectedFrames
                    sar_num = 1; sar_den = 1; duration_sec = $Duration
                    audio_codec = 'aac'; sample_rate = 48000; channels = 2; has_alpha = $false }
        Args = { New-VideoArgs -Width 3840 -Height 2160 -VCodec 'libx265' -PixFmt 'yuv420p10le' `
                               -Preset 'veryfast' -Pattern 'smptehdbars' -Output $args[0] }
    },
    @{
        Id = 'png_alpha'; File = 'png_alpha.png'; Kind = 'image'
        # アルファは (X+Y)*255/(W+H) のグラデーションなので、
        # 透明 (0 付近) と不透明 (253) の両方が必ず含まれる。
        # pix_fmt が rgba でも中身が全て 255 ならアルファは死んでいるので、
        # 実測値域まで検証する。
        Expect = @{ width = 512; height = 512; video_codec = 'png'; pix_fmt = 'rgba'
                    sar_num = 1; sar_den = 1
                    has_alpha = $true; alpha_min_le = 5; alpha_max_ge = 250 }
        Args = {
            # アルファは左上から右下へのグラデーション。
            # 完全不透明・完全透明・中間の全てを含むので、
            # 「アルファが落ちていないか」を数値で判定できる。
            #
            # NOTE: 配列リテラル内で文字列を改行して連結すると、PowerShell は
            #       行末の + を継続と見なさず 2 要素に分割する。必ず 1 行で組む。
            $src = "color=c=black:s=512x512:d=1,format=rgba,geq=r='X*255/W':g='Y*255/H':b='128':a='(X+Y)*255/(W+H)'"
            @(
                '-hide_banner', '-y', '-loglevel', 'error', '-nostdin'
                '-f', 'lavfi', '-i', $src
                '-frames:v', '1', '-c:v', 'png', '-pix_fmt', 'rgba', $args[0]
            )
        }
    },
    @{
        Id = 'wav_48k'; File = 'wav_48k.wav'; Kind = 'audio'
        Expect = @{ audio_codec = 'pcm_s16le'; sample_rate = 48000; channels = 2
                    duration_sec = $Duration; has_alpha = $false }
        Args = {
            @(
                '-hide_banner', '-y', '-loglevel', 'error', '-nostdin'
                '-f', 'lavfi', '-i', $AudioExprA2
                '-c:a', 'pcm_s16le', '-ar', '48000', '-ac', '2', $args[0]
            )
        }
    }
)

# --- 生成 -------------------------------------------------------------------

function Get-Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -Path $Path).Hash.ToLower()
}

function Invoke-FFprobe([string]$Path) {
    $json = & $FFprobe -hide_banner -loglevel error -print_format json `
                       -show_format -show_streams -- $Path 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "ffprobe が失敗しました ($Path):`n$json"
    }
    return ($json -join "`n")
}

$entries = @()

foreach ($asset in $Assets) {
    $outPath = Join-Path $OutputRoot $asset.File

    if ((Test-Path $outPath) -and -not $Force) {
        Write-Host "  skip  $($asset.File) (既存)" -ForegroundColor DarkGray
    } else {
        Write-Host "  生成  $($asset.File) ..." -NoNewline
        $sw = [Diagnostics.Stopwatch]::StartNew()

        $ffArgs = & $asset.Args $outPath
        & $FFmpeg @ffArgs
        if ($LASTEXITCODE -ne 0) {
            throw "ffmpeg が失敗しました ($($asset.Id)), exit $LASTEXITCODE"
        }
        $sw.Stop()
        Write-Host (" {0:N1}s" -f $sw.Elapsed.TotalSeconds) -ForegroundColor Green
    }

    # manifest 用に毎回 probe する (skip した素材も検証対象にする)
    $ffArgs   = & $asset.Args $outPath
    $probeRaw = Invoke-FFprobe $outPath
    $probe    = $probeRaw | ConvertFrom-Json

    $probeFile = Join-Path $ProbeDir "$($asset.Id).ffprobe.json"
    Set-Content -Path $probeFile -Value $probeRaw -Encoding UTF8

    $v = $probe.streams | Where-Object { $_.codec_type -eq 'video' } | Select-Object -First 1
    $a = $probe.streams | Where-Object { $_.codec_type -eq 'audio' } | Select-Object -First 1

    # fps は有理数のまま持つ。小数へ潰すと 60000/1001 のような素材で壊れる。
    $fpsNum = 0; $fpsDen = 0
    if ($v -and $v.PSObject.Properties['r_frame_rate']) {
        $parts = $v.r_frame_rate -split '/'
        $fpsNum = [int]$parts[0]
        $fpsDen = [int]$parts[1]
    }

    $frameCount = 0
    if ($v -and $v.PSObject.Properties['nb_frames'] -and $v.nb_frames) {
        $frameCount = [long]$v.nb_frames
    }

    $entries += [ordered]@{
        id                = $asset.Id
        relative_path     = $asset.File
        kind              = $asset.Kind
        sha256            = Get-Sha256 $outPath
        size_bytes        = (Get-Item $outPath).Length
        container         = $probe.format.format_name
        duration_sec      = if ($probe.format.PSObject.Properties['duration']) { [double]$probe.format.duration } else { 0.0 }
        video_codec       = if ($v) { $v.codec_name } else { '' }
        width             = if ($v) { [int]$v.width } else { 0 }
        height            = if ($v) { [int]$v.height } else { 0 }
        pix_fmt           = if ($v) { $v.pix_fmt } else { '' }
        fps_num           = $fpsNum
        fps_den           = $fpsDen
        frame_count       = $frameCount
        sample_aspect_ratio = if ($v -and $v.PSObject.Properties['sample_aspect_ratio']) { $v.sample_aspect_ratio } else { '' }
        audio_codec       = if ($a) { $a.codec_name } else { '' }
        sample_rate       = if ($a) { [int]$a.sample_rate } else { 0 }
        channels          = if ($a) { [int]$a.channels } else { 0 }
        has_alpha         = [bool]$asset.Expect.has_alpha
        expected          = $asset.Expect
        ffprobe_json      = "_ffprobe/$($asset.Id).ffprobe.json"
        ffmpeg_args       = @($ffArgs)
    }
}

# --- 日本語・空白・全角記号を含むパスへのコピー (V10) -----------------------
# 実際の運用で起きる形に近づける: 日本語ディレクトリ + 全角空白 + 全角記号 +
# 半角空白。ファイル名自体にも日本語を含める。

$JpRelDir = '素材\日本語 テスト\第1回　微分積分＆演習'
$JpDir    = Join-Path $OutputRoot $JpRelDir
New-Item -ItemType Directory -Force -Path $JpDir | Out-Null

$JpCopies = @(
    @{ From = 'v1080p60_h264.mp4'; To = '講義映像　第1回＆演習.mp4' }
    @{ From = 'png_alpha.png';     To = '数式オーバーレイ＆図.png' }
    @{ From = 'wav_48k.wav';       To = 'ナレーション　音声.wav' }
)

$jpEntries = @()
foreach ($c in $JpCopies) {
    $src = Join-Path $OutputRoot $c.From
    $dst = Join-Path $JpDir $c.To
    if (-not (Test-Path $src)) { continue }
    if ((Test-Path $dst) -and -not $Force) {
        # 既存でも manifest には載せる
    } else {
        Copy-Item -Path $src -Destination $dst -Force
    }

    $base = $entries | Where-Object { $_.relative_path -eq $c.From } | Select-Object -First 1
    $clone = [ordered]@{}
    foreach ($k in $base.Keys) { $clone[$k] = $base[$k] }
    $clone['id']            = "$($base.id)_jp"
    $clone['relative_path'] = (Join-Path $JpRelDir $c.To)
    $clone['sha256']        = Get-Sha256 $dst
    $clone['note']          = '日本語・半角空白・全角空白・全角記号を含むパス (V10 検証用)'
    $jpEntries += $clone
}
$entries += $jpEntries

Write-Host "  日本語パスへコピー: $($jpEntries.Count) 件 -> $JpRelDir" -ForegroundColor Green

# --- 破損素材 (negative test 用) --------------------------------------------
# MLT は開けなかった素材でも producer を返す。これらが「成功」扱いされないことを
# 確認するために、意図的に壊れた入力を用意する。
# manifest の assets には載せない (verify-media の検証対象ではない)。

$CorruptDir = Join-Path $OutputRoot '_corrupt'
New-Item -ItemType Directory -Force -Path $CorruptDir | Out-Null

# 1. 0 バイト
$zeroPath = Join-Path $CorruptDir 'zero.mp4'
[System.IO.File]::WriteAllBytes($zeroPath, @())

# 2. ランダムなバイト列 (コンテナとして解釈できない)
$randomPath = Join-Path $CorruptDir 'random.mp4'
$rnd = [byte[]]::new(65536)
# 決定論的にするため固定 seed
$gen = [System.Random]::new(20260804)
$gen.NextBytes($rnd)
[System.IO.File]::WriteAllBytes($randomPath, $rnd)

# 3. 途中で切断された動画 (moov はあるがデータが足りない / あるいは逆)
#    先頭 20% だけを取り出す。fragmented でない mp4 は moov が末尾にあるため、
#    これは「コンテナとして開けない」ケースになる。
$truncPath = Join-Path $CorruptDir 'truncated.mp4'
$srcFull = Join-Path $OutputRoot 'v1080p60_h264.mp4'
if (Test-Path $srcFull) {
    $srcBytes = [System.IO.File]::ReadAllBytes($srcFull)
    $take = [int]($srcBytes.Length * 0.2)
    [System.IO.File]::WriteAllBytes($truncPath, $srcBytes[0..($take - 1)])
}

# 4. 拡張子は動画だが中身がテキスト
$textPath = Join-Path $CorruptDir 'text.mp4'
Set-Content -Path $textPath -Value 'this is not a video file' -Encoding ASCII -NoNewline

# 5. コンテナは正当だが映像も音声も無い (字幕のみの mp4)。
#    破損ではないが「読めるが使えない」入力であり、ユーザーが実際に
#    投入しうる。壊れたバイト列とは別の失敗経路を通る。
$subOnlyPath = Join-Path $CorruptDir 'subtitle_only.mp4'
$srtPath = Join-Path $CorruptDir '_sub.srt'
Set-Content -Path $srtPath -Value "1`r`n00:00:00,000 --> 00:00:03,000`r`nmvm phase0`r`n" -Encoding ASCII
& $FFmpeg -hide_banner -loglevel error -y -i $srtPath -c:s mov_text $subOnlyPath
if ($LASTEXITCODE -ne 0) { Write-Host '  (字幕のみ mp4 の生成に失敗しました)' -ForegroundColor Yellow }

Write-Host "  破損・退化素材 (negative test 用): 5 件 -> _corrupt\" -ForegroundColor Green

# --- 音声切り分け用素材 (_diag) ---------------------------------------------
# S5 の音声不具合を最小構成で切り分けるための素材。
# 既存素材は削除せず追加する。manifest には載せない (verify-media の対象外)。
#
# 周波数は整数倍関係を避ける。1000/500 や 1500/750 のような関係だと、
# 高調波と別トラックからの漏洩を区別できない。
#   A1: L 997Hz / R 613Hz
#   A2: L 1429Hz / R 823Hz
# いずれも互いに整数倍でなく、2倍・3倍高調波も他方のターゲットと重ならない。

$DiagDir = Join-Path $OutputRoot '_diag'
New-Item -ItemType Directory -Force -Path $DiagDir | Out-Null

$DiagA1Expr = 'aevalsrc=exprs=0.5*sin(2*PI*997*t)|0.5*sin(2*PI*613*t):s=48000:d=' +
              "${Duration}:c=stereo"
$DiagA2Expr = 'aevalsrc=exprs=0.5*sin(2*PI*1429*t)|0.5*sin(2*PI*823*t):s=48000:d=' +
              "${Duration}:c=stereo"
$DiagVideo  = "testsrc2=s=1280x720:r=${Fps}:d=${Duration}"

$diagJobs = @(
    @{ Name = 'v1_video_only.mp4'
       Args = @('-hide_banner','-y','-loglevel','error','-nostdin',
                '-f','lavfi','-i',$DiagVideo,
                '-an','-c:v','libx264','-preset','veryfast','-crf','23',
                '-pix_fmt','yuv420p','-r',"$Fps",'-fps_mode','cfr') }
    @{ Name = 'a1_av.mp4'
       Args = @('-hide_banner','-y','-loglevel','error','-nostdin',
                '-f','lavfi','-i',$DiagVideo,
                '-f','lavfi','-i',$DiagA1Expr,
                '-c:v','libx264','-preset','veryfast','-crf','23',
                '-pix_fmt','yuv420p','-r',"$Fps",'-fps_mode','cfr',
                '-c:a','aac','-b:a','192k','-ar','48000','-ac','2') }
    @{ Name = 'a1_audio_only.m4a'
       Args = @('-hide_banner','-y','-loglevel','error','-nostdin',
                '-f','lavfi','-i',$DiagA1Expr,
                '-c:a','aac','-b:a','192k','-ar','48000','-ac','2') }
    @{ Name = 'a2.wav'
       Args = @('-hide_banner','-y','-loglevel','error','-nostdin',
                '-f','lavfi','-i',$DiagA2Expr,
                '-c:a','pcm_s16le','-ar','48000','-ac','2') }
    # 参照用: A1 を PCM でそのまま持つ (AAC を経由しない基準)
    @{ Name = 'a1_ref.wav'
       Args = @('-hide_banner','-y','-loglevel','error','-nostdin',
                '-f','lavfi','-i',$DiagA1Expr,
                '-c:a','pcm_s16le','-ar','48000','-ac','2') }
)

foreach ($j in $diagJobs) {
    $p = Join-Path $DiagDir $j.Name
    if ((Test-Path $p) -and -not $Force) { continue }
    & $FFmpeg @($j.Args + @($p))
    if ($LASTEXITCODE -ne 0) { throw "切り分け用素材の生成に失敗: $($j.Name)" }
}
Write-Host "  音声切り分け用素材: $($diagJobs.Count) 件 -> _diag\ (A1 997/613Hz, A2 1429/823Hz)" -ForegroundColor Green

# --- 画像・音声の読み込み判定用素材 (_import) --------------------------------
# 素材種別の判定 (media_stream_facts) と静止画 decoder の検査に使う。
# manifest には載せない (verify-media の対象外)。MLT が開けない素材も含むため。
#
# 向きの検査用画像は 64x32 で、左上 32x16 だけが赤、残りが青。
# 向きを反映すると赤の位置と縦横が変わるので、画素で向きを判別できる。

$ImportDir = Join-Path $OutputRoot '_import'
New-Item -ItemType Directory -Force -Path $ImportDir | Out-Null

$ImportQuadrant = 'color=c=blue:s=64x32,drawbox=x=0:y=0:w=32:h=16:color=red@1:t=fill'
$ImportTest     = 'testsrc2=s=64x48'
$ImportAnim     = 'testsrc2=s=64x48:r=10:d=0.3'
$ImportTone     = 'sine=f=997:d=1:sample_rate=48000'
$ImportBase     = @('-hide_banner', '-y', '-loglevel', 'error', '-nostdin')

$importJobs = @(
    @{ Name = 'jpg_quadrant.jpg'
       Args = @('-f','lavfi','-i',$ImportQuadrant,'-frames:v','1','-q:v','2') }
    # full range の灰色 128。limited range と取り違えると 128 から大きくずれる。
    @{ Name = 'jpg_gray128.jpg'
       Args = @('-f','lavfi','-i','color=c=0x808080:s=64x32','-frames:v','1','-q:v','2') }
    @{ Name = 'png_rgb24.png'
       Args = @('-f','lavfi','-i',$ImportTest,'-frames:v','1') }
    # 透明な背景に不透明な赤の四角。palette が透明色を持つ。
    # drawbox は rgba の alpha を書かず、-pix_fmt pal8 への単純な変換は alpha を落とす (実測)。
    # geq で alpha を明示し、palettegen で透明色を palette に予約する。
    @{ Name = 'png_pal8_alpha.png'
       Args = @('-f','lavfi','-i',("color=c=black:s=64x32:d=0.04,format=rgba," +
                "geq=r='if(lt(X\,32)*lt(Y\,16)\,255\,0)':g=0:b=0:a='if(lt(X\,32)*lt(Y\,16)\,255\,0)'," +
                'split[a][b];[a]palettegen=reserve_transparent=1[p];[b][p]paletteuse=alpha_threshold=128'),
                '-frames:v','1','-pix_fmt','pal8') }
    @{ Name = 'png_gray16.png'
       Args = @('-f','lavfi','-i','color=c=0x808080:s=64x32','-frames:v','1','-pix_fmt','gray16be') }
    @{ Name = 'tiff_rgb48.tif'
       Args = @('-f','lavfi','-i',$ImportTest,'-frames:v','1','-pix_fmt','rgb48le') }
    @{ Name = 'bmp_24.bmp'
       Args = @('-f','lavfi','-i',$ImportTest,'-frames:v','1') }
    @{ Name = 'tga_24.tga'
       Args = @('-f','lavfi','-i',$ImportTest,'-frames:v','1') }
    @{ Name = 'qoi_rgb.qoi'
       Args = @('-f','lavfi','-i',$ImportTest,'-frames:v','1') }
    @{ Name = 'webp_static.webp'
       Args = @('-f','lavfi','-i',$ImportTest,'-frames:v','1','-c:v','libwebp') }
    @{ Name = 'gif_static.gif'
       Args = @('-f','lavfi','-i',$ImportTest,'-frames:v','1') }
    @{ Name = 'webp_animated.webp'
       Args = @('-f','lavfi','-i',$ImportAnim,'-c:v','libwebp_anim','-loop','0') }
    @{ Name = 'gif_animated.gif'
       Args = @('-f','lavfi','-i',$ImportAnim) }
    @{ Name = 'apng_animated.png'
       Args = @('-f','lavfi','-i',$ImportAnim,'-f','apng','-plays','0') }
    # scene-linear の HDR 画像。tone map が要るので拒否される側。
    @{ Name = 'exr_float.exr'
       Args = @('-f','lavfi','-i',$ImportTest,'-frames:v','1','-pix_fmt','gbrpf32le') }
    @{ Name = 'opus_48k.opus'
       Args = @('-f','lavfi','-i',$ImportTone,'-c:a','libopus') }
    @{ Name = 'vorbis_48k.ogg'
       Args = @('-f','lavfi','-i',$ImportTone,'-c:a','libvorbis') }
    # 対照: 本物の Motion JPEG 動画。codec は JPEG と同じだが動画として扱う。
    @{ Name = 'mjpeg_av.avi'
       Args = @('-f','lavfi','-i','testsrc2=s=64x48:r=10:d=1','-f','lavfi','-i',$ImportTone,
                '-c:v','mjpeg','-c:a','pcm_s16le') }
    # 透過のある動画 (取り込みで断る側)。VP9 は画素形式が yuv420p のまま alpha を別に持ち、
    # webm の alpha_mode=1 で示される。ProRes 4444 は画素形式に alpha がある。
    @{ Name = 'vp9_alpha.webm'
       Args = @('-f','lavfi','-i','color=c=red@0.5:s=64x48:r=10:d=1,format=yuva420p',
                '-c:v','libvpx-vp9','-pix_fmt','yuva420p') }
    @{ Name = 'prores4444_alpha.mov'
       Args = @('-f','lavfi','-i','color=c=red@0.5:s=64x48:r=10:d=1,format=yuva444p10le',
                '-c:v','prores_ks','-profile:v','4444','-pix_fmt','yuva444p10le') }
    # 対照: 透過の無い VP9。codec と器は同じで alpha だけが違う。
    @{ Name = 'vp9_opaque.webm'
       Args = @('-f','lavfi','-i','testsrc2=s=64x48:r=10:d=1','-c:v','libvpx-vp9','-pix_fmt','yuv420p') }
)

foreach ($j in $importJobs) {
    $p = Join-Path $ImportDir $j.Name
    if ((Test-Path $p) -and -not $Force) { continue }
    & $FFmpeg @($ImportBase + $j.Args + @($p))
    if ($LASTEXITCODE -ne 0) { throw "読み込み判定用素材の生成に失敗: $($j.Name)" }
}

# カバーアート (attached_pic) 付きの音声と、本物の映像 + カバーアートの mp4。
# 絵は jpg_quadrant.jpg を流用する。
$coverSource = Join-Path $ImportDir 'jpg_quadrant.jpg'
$coverJobs = @(
    @{ Name = 'mp3_cover.mp3';  Audio = @('-c:a','libmp3lame') }
    @{ Name = 'm4a_cover.m4a';  Audio = @('-c:a','aac') }
    @{ Name = 'flac_cover.flac'; Audio = @('-c:a','flac') }
)
foreach ($j in $coverJobs) {
    $p = Join-Path $ImportDir $j.Name
    if ((Test-Path $p) -and -not $Force) { continue }
    & $FFmpeg @($ImportBase + @('-f','lavfi','-i',$ImportTone,'-i',$coverSource,'-map','0','-map','1') +
                $j.Audio + @('-c:v','copy','-disposition:v','attached_pic',$p))
    if ($LASTEXITCODE -ne 0) { throw "読み込み判定用素材の生成に失敗: $($j.Name)" }
}
$coverVideoPath = Join-Path $ImportDir 'mp4_h264_with_cover.mp4'
if (-not (Test-Path $coverVideoPath) -or $Force) {
    & $FFmpeg @($ImportBase + @('-f','lavfi','-i','testsrc2=s=64x48:r=10:d=1','-i',$coverSource,
                                '-map','0','-map','1','-c:v:0','libx264','-pix_fmt','yuv420p',
                                '-c:v:1','copy','-disposition:v:1','attached_pic',$coverVideoPath))
    if ($LASTEXITCODE -ne 0) { throw '読み込み判定用素材の生成に失敗: mp4_h264_with_cover.mp4' }
}

# JPEG の SOI 直後へ、Orientation (0x0112) だけを持つ最小の APP1 Exif を差し込む。
# ffmpeg の mjpeg encoder は EXIF を書かないため、byte で組む。
function Add-JpegExifOrientation([string]$Source, [string]$Destination, [int]$Orientation) {
    $jpeg = [System.IO.File]::ReadAllBytes($Source)
    if ($jpeg.Length -lt 2 -or $jpeg[0] -ne 0xFF -or $jpeg[1] -ne 0xD8) {
        throw "JPEG ではありません: $Source"
    }
    $tiff = [System.Collections.Generic.List[byte]]::new()
    $tiff.AddRange([byte[]](0x49, 0x49, 0x2A, 0x00))           # "II" little endian, 42
    $tiff.AddRange([BitConverter]::GetBytes([uint32]8))        # IFD0 の位置
    $tiff.AddRange([BitConverter]::GetBytes([uint16]1))        # entry 数
    $tiff.AddRange([BitConverter]::GetBytes([uint16]0x0112))   # Orientation
    $tiff.AddRange([BitConverter]::GetBytes([uint16]3))        # SHORT
    $tiff.AddRange([BitConverter]::GetBytes([uint32]1))        # count
    $tiff.AddRange([BitConverter]::GetBytes([uint16]$Orientation))
    $tiff.AddRange([byte[]](0x00, 0x00))                       # 4 byte への詰め
    $tiff.AddRange([BitConverter]::GetBytes([uint32]0))        # 次の IFD は無い
    $app1 = [System.Collections.Generic.List[byte]]::new()
    $app1.AddRange([System.Text.Encoding]::ASCII.GetBytes("Exif"))
    $app1.AddRange([byte[]](0x00, 0x00))
    $app1.AddRange($tiff)
    $segmentLength = $app1.Count + 2
    $out = [System.Collections.Generic.List[byte]]::new()
    $out.AddRange([byte[]](0xFF, 0xD8, 0xFF, 0xE1))
    $out.Add([byte](($segmentLength -shr 8) -band 0xFF))
    $out.Add([byte]($segmentLength -band 0xFF))
    $out.AddRange($app1)
    $out.AddRange([byte[]]$jpeg[2..($jpeg.Length - 1)])
    [System.IO.File]::WriteAllBytes($Destination, $out.ToArray())
}

foreach ($orientation in @(1, 6)) {
    $p = Join-Path $ImportDir "jpg_exif_orient$orientation.jpg"
    if ((Test-Path $p) -and -not $Force) { continue }
    Add-JpegExifOrientation -Source $coverSource -Destination $p -Orientation $orientation
}

# 4200x4200 (約 1764 万画素) の PNG。decoder の画素数上限を小さくして、上限で拒否されることと、
# 上限を上げれば通ること (対照) を検査する。PNG は寸法を知るために stream 情報の取得でも
# decode されるので、そこへ上限が渡っていなければ確保が先に起きる。
$largePngPath = Join-Path $ImportDir 'png_gray_4200.png'
if (-not (Test-Path $largePngPath) -or $Force) {
    & $FFmpeg @($ImportBase + @('-f','lavfi','-i','color=c=0x808080:s=4200x4200','-frames:v','1',
                                $largePngPath))
    if ($LASTEXITCODE -ne 0) { throw '読み込み判定用素材の生成に失敗: png_gray_4200.png' }
}

# ICC profile (iCCP chunk) を持つ PNG。decoder は色の変換をしないので、sRGB 以外は拒否し、
# sRGB は受理する (対照)。decoder は説明 ('desc') ではなく中身 (原色・白色点・トーンカーブ)
# で判定するので、matrix/TRC 型の ICC v2 profile を byte で組む。ffmpeg の png encoder は
# iCCP を書かないため。
function Get-Crc32([byte[]]$Bytes) {
    [uint32]$crc = [uint32]::MaxValue
    foreach ($b in $Bytes) {
        $crc = [uint32]($crc -bxor [uint32]$b)
        for ($k = 0; $k -lt 8; $k++) {
            if (($crc -band 1) -ne 0) { $crc = [uint32](($crc -shr 1) -bxor [uint32]3988292384) }
            else { $crc = $crc -shr 1 }
        }
    }
    return [uint32]($crc -bxor [uint32]::MaxValue)
}
function Get-BigEndian32([uint32]$Value) {
    $bytes = [BitConverter]::GetBytes($Value)
    [Array]::Reverse($bytes)
    return $bytes
}
# s15Fixed16Number (符号付き 16.16 固定小数点、big endian)。
function Get-S15Fixed16([double]$Value) {
    $bytes = [BitConverter]::GetBytes([int32][math]::Round($Value * 65536))
    [Array]::Reverse($bytes)
    return $bytes
}
function New-IccXyzTag([double[]]$Xyz) {
    $tag = [System.Collections.Generic.List[byte]]::new()
    $tag.AddRange([System.Text.Encoding]::ASCII.GetBytes('XYZ '))
    $tag.AddRange([byte[]]::new(4))
    foreach ($v in $Xyz) { $tag.AddRange([byte[]](Get-S15Fixed16 $v)) }
    return $tag.ToArray()
}
# 原色は D50 へ Bradford 変換した XYZ (ICC の PCS)。
$IccColorants = @{
    Srgb = @(@(0.436066, 0.222488, 0.013916), @(0.385147, 0.716873, 0.097076),
             @(0.143066, 0.060608, 0.714096))
    DisplayP3 = @(@(0.515121, 0.241196, -0.001053), @(0.291977, 0.692245, 0.041885),
                  @(0.157104, 0.066574, 0.784073))
}
function New-IccProfile([string]$Description, [object[]]$Colorants) {
    $ascii = [System.Text.Encoding]::ASCII.GetBytes($Description)
    $desc = [System.Collections.Generic.List[byte]]::new()
    $desc.AddRange([System.Text.Encoding]::ASCII.GetBytes('desc'))
    $desc.AddRange([byte[]](0, 0, 0, 0))
    $desc.AddRange([byte[]](Get-BigEndian32 ([uint32]($ascii.Length + 1))))
    $desc.AddRange($ascii)
    $desc.Add([byte]0)
    $desc.AddRange([byte[]]::new(4 + 4 + 2 + 1 + 67))   # unicode / scriptcode は空
    # sRGB の曲線 (parametricCurveType の関数型 3)。3 channel で共有する。
    $trc = [System.Collections.Generic.List[byte]]::new()
    $trc.AddRange([System.Text.Encoding]::ASCII.GetBytes('para'))
    $trc.AddRange([byte[]](0, 0, 0, 0, 0, 3, 0, 0))
    foreach ($v in @(2.4, (1 / 1.055), (0.055 / 1.055), (1 / 12.92), 0.04045)) {
        $trc.AddRange([byte[]](Get-S15Fixed16 $v))
    }
    $tags = [ordered]@{
        desc = $desc.ToArray()
        wtpt = New-IccXyzTag @(0.950455, 1.0, 1.089050)   # v2 の sRGB profile と同じ D65
        rXYZ = New-IccXyzTag $Colorants[0]
        gXYZ = New-IccXyzTag $Colorants[1]
        bXYZ = New-IccXyzTag $Colorants[2]
        rTRC = $trc.ToArray()
    }
    $tableSize = 4 + 12 * ($tags.Count + 2)               # gTRC / bTRC は rTRC を指す
    $body = [System.Collections.Generic.List[byte]]::new()
    $table = [System.Collections.Generic.List[byte]]::new()
    $table.AddRange([byte[]](Get-BigEndian32 ([uint32]($tags.Count + 2))))
    $offsets = @{}
    foreach ($name in $tags.Keys) {
        $offsets[$name] = 128 + $tableSize + $body.Count
        $body.AddRange([byte[]]$tags[$name])
        while ($body.Count % 4 -ne 0) { $body.Add([byte]0) }
    }
    foreach ($name in @($tags.Keys) + @('gTRC', 'bTRC')) {
        $source = if ($name -in @('gTRC', 'bTRC')) { 'rTRC' } else { $name }
        $table.AddRange([System.Text.Encoding]::ASCII.GetBytes($name))
        $table.AddRange([byte[]](Get-BigEndian32 ([uint32]$offsets[$source])))
        $table.AddRange([byte[]](Get-BigEndian32 ([uint32]$tags[$source].Length)))
    }
    $iccBytes = [System.Collections.Generic.List[byte]]::new()
    $iccBytes.AddRange([byte[]](Get-BigEndian32 ([uint32](128 + $tableSize + $body.Count))))
    $iccBytes.AddRange([byte[]]::new(4))                                   # CMM
    $iccBytes.AddRange([byte[]](0x02, 0x10, 0x00, 0x00))                   # version 2.1
    $iccBytes.AddRange([System.Text.Encoding]::ASCII.GetBytes('mntrRGB XYZ '))
    $iccBytes.AddRange([byte[]]::new(12))                                  # date
    $iccBytes.AddRange([System.Text.Encoding]::ASCII.GetBytes('acsp'))
    $iccBytes.AddRange([byte[]]::new(128 - $iccBytes.Count))
    $iccBytes.AddRange($table)
    $iccBytes.AddRange($body)
    return $iccBytes.ToArray()
}
function Add-PngIccProfile([string]$Source, [string]$Destination, [string]$Description,
                           [object[]]$Colorants) {
    $png = [System.IO.File]::ReadAllBytes($Source)
    $compressed = [System.IO.MemoryStream]::new()
    $zlib = [System.IO.Compression.ZLibStream]::new($compressed, [System.IO.Compression.CompressionLevel]::Optimal)
    $icc = New-IccProfile -Description $Description -Colorants $Colorants
    $zlib.Write($icc, 0, $icc.Length)
    $zlib.Dispose()
    $data = [System.Collections.Generic.List[byte]]::new()
    $data.AddRange([System.Text.Encoding]::ASCII.GetBytes('ICC Profile'))
    $data.AddRange([byte[]](0, 0))                                        # 区切りと圧縮方式 0
    $data.AddRange($compressed.ToArray())
    $typeAndData = [byte[]]([System.Text.Encoding]::ASCII.GetBytes('iCCP') + $data.ToArray())
    $chunk = [System.Collections.Generic.List[byte]]::new()
    $chunk.AddRange([byte[]](Get-BigEndian32 ([uint32]$data.Count)))
    $chunk.AddRange($typeAndData)
    $chunk.AddRange([byte[]](Get-BigEndian32 (Get-Crc32 $typeAndData)))
    # signature (8) + IHDR chunk (4 + 4 + 13 + 4) の直後へ差し込む。
    $out = [System.Collections.Generic.List[byte]]::new()
    $out.AddRange([byte[]]$png[0..32])
    $out.AddRange($chunk)
    $out.AddRange([byte[]]$png[33..($png.Length - 1)])
    [System.IO.File]::WriteAllBytes($Destination, $out.ToArray())
}
$iccSource = Join-Path $ImportDir 'png_rgb24.png'
# fake_srgb は説明だけ sRGB を名乗り、原色は Display P3。説明で判定していると通ってしまう。
foreach ($icc in @(@{ Name = 'png_icc_display_p3.png'; Description = 'Display P3'; Colorants = 'DisplayP3' },
                   @{ Name = 'png_icc_srgb.png'; Description = 'sRGB IEC61966-2.1'; Colorants = 'Srgb' },
                   @{ Name = 'png_icc_fake_srgb.png'; Description = 'sRGB compatible profile'; Colorants = 'DisplayP3' })) {
    $p = Join-Path $ImportDir $icc.Name
    if ((Test-Path $p) -and -not $Force) { continue }
    Add-PngIccProfile -Source $iccSource -Destination $p -Description $icc.Description `
        -Colorants $IccColorants[$icc.Colorants]
}

# 日本語名の複製。判定と decode が UTF-8 のパスを正しく渡していることを見る。
$importJpPath = Join-Path $ImportDir '写真　縦向き＆EXIF.jpg'
Copy-Item -Path (Join-Path $ImportDir 'jpg_exif_orient6.jpg') -Destination $importJpPath -Force

$importCount = @(Get-ChildItem -Path $ImportDir -File).Count
Write-Host "  読み込み判定用素材: $importCount 件 -> _import\" -ForegroundColor Green

# --- manifest ---------------------------------------------------------------

$ffVersion = (& $FFmpeg -hide_banner -version 2>&1 | Select-Object -First 1)

$manifest = [ordered]@{
    schema_version   = 1
    mode             = $Mode
    generated_at     = (Get-Date -Format 'yyyy-MM-ddTHH:mm:sszzz')
    duration_sec     = $Duration
    fps              = $Fps
    expected_frames  = $ExpectedFrames
    ffmpeg           = $FFmpeg
    ffmpeg_version   = $ffVersion
    marker = [ordered]@{
        cell_size_px  = $CellSize
        cell_count    = $MarkerCells
        band_width_px = $MarkerWidth
        band_height_px= $CellSize
        origin_x      = 0
        origin_y      = 0
        white_level   = 235
        black_level   = 16
        layout        = 'cell0=sync_white, cell1=sync_black, cell2..17=frame index 16bit LSB-first, cell18=sync_white'
        spec_doc      = 'crates/testkit/src/marker.rs'
    }
    audio = [ordered]@{
        left_hz     = 1000
        right_hz    = 500
        sample_rate = 48000
        note        = '48000Hz に対し整数周期 (48 / 96 サンプル)。位相は t=0 で 0。'
    }
    assets = $entries
}

$manifestPath = Join-Path $OutputRoot 'manifest.json'

# --- 再生成時のメタデータ照合 ----------------------------------------------
if ($VerifyRegeneration -and (Test-Path $manifestPath)) {
    Write-Host "`n--- 再生成の照合 ---" -ForegroundColor Yellow
    $old = Get-Content $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json

    $metaFields = @('container','video_codec','width','height','pix_fmt',
                    'fps_num','fps_den','frame_count','audio_codec','sample_rate','channels')
    $diffs = 0; $hashDiffs = 0

    foreach ($new in $entries) {
        $prev = $old.assets | Where-Object { $_.id -eq $new.id } | Select-Object -First 1
        if (-not $prev) { continue }
        foreach ($f in $metaFields) {
            if ("$($prev.$f)" -ne "$($new[$f])") {
                Write-Host "  META 不一致 $($new.id).$f : '$($prev.$f)' -> '$($new[$f])'" -ForegroundColor Red
                $diffs++
            }
        }
        if ($prev.sha256 -ne $new['sha256']) { $hashDiffs++ }
    }

    Write-Host "  メタデータ不一致 : $diffs 件"
    Write-Host "  hash 不一致       : $hashDiffs 件 (映像 codec の非決定性により発生しうる。失敗ではない)"
    if ($diffs -gt 0) {
        throw "再生成でメタデータが変化しました。生成条件が決定論的ではありません。"
    }
}

$manifest | ConvertTo-Json -Depth 8 | Set-Content -Path $manifestPath -Encoding UTF8

Write-Host "`nmanifest: $manifestPath" -ForegroundColor Cyan
Write-Host "素材数  : $($entries.Count)"
Write-Host "`n次: .\dev.ps1 test-full"
