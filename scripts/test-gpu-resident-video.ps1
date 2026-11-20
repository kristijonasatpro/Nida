# Explicitly opt into hardware qualification after other GPU work has finished.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string[]]$Clips,
    [switch]$RenderFinished,
    [ValidateSet('Pictures', 'Benchmark', 'Interlaced', 'Stability')][string]$Mode = 'Pictures',
    [ValidateRange(1, 64)][int]$ClipCount = 1,
    [ValidateRange(1, 7200)][int]$Seconds = 60
)
$ErrorActionPreference = 'Stop'
if (!$RenderFinished) { throw 'GPU qualification is deferred while the PC is rendering. Pass -RenderFinished only after that render is complete.' }
$nidaGpuRoot = Split-Path $PSScriptRoot -Parent
$nidaGpuClips = @($Clips | ForEach-Object { (Resolve-Path -LiteralPath $_ -ErrorAction Stop).Path })
$nidaGpuOldPath = $env:PATH
$nidaGpuSavedEnv = @{}
foreach ($nidaGpuName in @('VARDA_REQUIRE_GPU_RESIDENT', 'VARDA_VIDEO_BENCH_CLIP', 'VARDA_VIDEO_BENCH_CLIPS', 'VARDA_VIDEO_BENCH_SECONDS', 'NIDA_GPU_INTERLACED_CLIP', 'NIDA_GPU_SOAK_CLIPS', 'NIDA_GPU_SOAK_SECONDS')) {
    $nidaGpuSavedEnv[$nidaGpuName] = [Environment]::GetEnvironmentVariable($nidaGpuName, 'Process')
}
Push-Location -LiteralPath $nidaGpuRoot
try {
    . (Join-Path $nidaGpuRoot '.windows-tools/build-env.ps1')
    $env:VARDA_REQUIRE_GPU_RESIDENT = '1'
    $env:VARDA_VIDEO_BENCH_CLIPS = "$ClipCount"
    $env:VARDA_VIDEO_BENCH_SECONDS = "$Seconds"
    $nidaGpuTest = switch ($Mode) {
        'Pictures' { 'internal::video::gpu_windows::tests::gpu_resident_picture_seek_pool_and_fallback' }
        'Benchmark' { 'internal::video::playback_diagnostics::realtime_measurement' }
        'Interlaced' { 'internal::video::gpu_windows::qualification::interlaced_uses_cpu_conversion_without_losing_the_picture' }
        'Stability' { 'internal::video::gpu_windows::qualification::playlist_playback_seek_pause_reload_and_soak' }
    }
    if ($Mode -eq 'Stability') {
        if ($nidaGpuClips.Count -lt 2 -or $Seconds -lt 20) { throw 'Stability qualification requires at least two clips and 20 seconds.' }
        $env:NIDA_GPU_SOAK_CLIPS = $nidaGpuClips -join [IO.Path]::PathSeparator
        $env:NIDA_GPU_SOAK_SECONDS = "$Seconds"
    }
    $nidaGpuRuns = if ($Mode -eq 'Stability') { @($nidaGpuClips[0]) } else { $nidaGpuClips }
    foreach ($nidaGpuClip in $nidaGpuRuns) {
        $env:VARDA_VIDEO_BENCH_CLIP = $nidaGpuClip
        if ($Mode -eq 'Interlaced') { $env:NIDA_GPU_INTERLACED_CLIP = $nidaGpuClip }
        & cargo test --offline --release --no-default-features --features screen-capture,windows-browser --lib $nidaGpuTest -- --ignored --exact --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) { throw "GPU qualification failed for $nidaGpuClip" }
    }
} finally {
    foreach ($nidaGpuName in $nidaGpuSavedEnv.Keys) {
        if ($null -eq $nidaGpuSavedEnv[$nidaGpuName]) {
            Remove-Item -LiteralPath "Env:$nidaGpuName" -ErrorAction SilentlyContinue
        } else {
            Set-Item -LiteralPath "Env:$nidaGpuName" -Value $nidaGpuSavedEnv[$nidaGpuName]
        }
    }
    $env:PATH = $nidaGpuOldPath
    Pop-Location
}
