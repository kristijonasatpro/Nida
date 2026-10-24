param(
    [Parameter(Mandatory)][string]$TestBinary,
    [Parameter(Mandatory)][string]$Clip,
    [Parameter(Mandatory)][string]$Output,
    [ValidateRange(1, 1000)][int]$Repeats = 3,
    [ValidateRange(1, 1000)][int]$Clips = 1,
    [ValidateRange(0.1, 3600.0)][double]$Seconds = 10,
    [switch]$Playback,
    [switch]$Hardware,
    [switch]$Upload
)
$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath $Output) { throw "Output already exists: $Output. Use a fresh log path." }
$binary = (Resolve-Path -LiteralPath $TestBinary).Path
$media = (Resolve-Path -LiteralPath $Clip).Path
$previousClip = $env:VARDA_VIDEO_BENCH_CLIP
$previousHardware = $env:VARDA_REQUIRE_HW_DECODE
$previousUpload = $env:VARDA_VIDEO_BENCH_UPLOAD
$previousClips = $env:VARDA_VIDEO_BENCH_CLIPS
$previousSeconds = $env:VARDA_VIDEO_BENCH_SECONDS
try {
    $env:VARDA_VIDEO_BENCH_CLIP = $media
    $env:VARDA_REQUIRE_HW_DECODE = if ($Hardware) { '1' } else { $null }
    $env:VARDA_VIDEO_BENCH_UPLOAD = if ($Upload) { '1' } else { $null }
    $env:VARDA_VIDEO_BENCH_CLIPS = "$Clips"
    $env:VARDA_VIDEO_BENCH_SECONDS = "$Seconds"
    $manifest = [ordered]@{
        clip_sha256 = (Get-FileHash -LiteralPath $media -Algorithm SHA256).Hash
        binary_sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
        revision = (& git rev-parse HEAD)
        ffmpeg = (& ffmpeg -version | Select-Object -First 1)
        os = [System.Runtime.InteropServices.RuntimeInformation]::OSDescription
        gpu = if (Get-Command nvidia-smi -ErrorAction SilentlyContinue) { (& nvidia-smi --query-gpu=name,driver_version --format=csv,noheader) } else { 'unavailable' }
        probe = (& ffprobe -v error -show_streams -show_format -of json $media | ConvertFrom-Json)
        hardware_required = [bool]$Hardware
        upload_wait = [bool]$Upload
        realtime_rendering = [bool]$Playback
        clips = $Clips
        seconds = $Seconds
    }
    $manifest | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath "$Output.manifest.json"
    for ($iteration = 1; $iteration -le $Repeats; $iteration++) {
        $test = if ($Playback) { 'internal::video::playback_diagnostics::realtime_measurement' } else { 'internal::video::diagnostics::pipeline_measurement' }
        $result = & $binary $test --exact --ignored --nocapture --test-threads=1 2>&1
        $result | Add-Content -LiteralPath $Output
        if ($LASTEXITCODE -ne 0) { throw "Video measurement failed (run $iteration); see $Output" }
        if (-not ($result -match 'VIDEO_PIPELINE|VIDEO_PLAYBACK')) { throw 'Missing measurement record' }
    }
} finally {
    $env:VARDA_VIDEO_BENCH_CLIP = $previousClip
    $env:VARDA_REQUIRE_HW_DECODE = $previousHardware
    $env:VARDA_VIDEO_BENCH_UPLOAD = $previousUpload
    $env:VARDA_VIDEO_BENCH_CLIPS = $previousClips
    $env:VARDA_VIDEO_BENCH_SECONDS = $previousSeconds
}
