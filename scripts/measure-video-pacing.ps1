param(
    [Parameter(Mandatory)][string]$TestBinary,
    [Parameter(Mandatory)][string]$Clip,
    [Parameter(Mandatory)][string]$Output,
    [ValidateRange(1,120)][double]$Seconds = 60,
    [ValidateRange(0,100)][double]$PhaseMs = 0,
    [ValidateRange(1,60)][int]$Clips = 1,
    [switch]$PollOnly,
    [switch]$Timer1ms,
    [switch]$HistoricalSampling,
    [switch]$LegacyWait,
    [switch]$PeriodicWake,
    [switch]$NoReadiness,
    [switch]$CopyPoll,
    [switch]$Trace
)
$ErrorActionPreference = 'Stop'
$names = @('NIDA_VIDEO_PACING_TRACE','NIDA_VIDEO_BENCH_PHASE_MS','NIDA_VIDEO_BENCH_POLL_ONLY','NIDA_VIDEO_BENCH_TIMER_1MS','NIDA_VIDEO_BENCH_HISTORICAL_SAMPLING','NIDA_VIDEO_BENCH_LEGACY_WAIT','NIDA_VIDEO_BENCH_PERIODIC_WAKE','NIDA_VIDEO_BENCH_NO_READINESS','NIDA_VIDEO_BENCH_COPY_POLL')
$previous = @{}
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
try {
    $env:NIDA_VIDEO_PACING_TRACE = if ($Trace) { [IO.Path]::GetFullPath("$Output.trace.jsonl") } else { $null }
    $env:NIDA_VIDEO_BENCH_PHASE_MS = $PhaseMs.ToString([cultureinfo]::InvariantCulture)
    $env:NIDA_VIDEO_BENCH_POLL_ONLY = if ($PollOnly) { '1' } else { $null }
    $env:NIDA_VIDEO_BENCH_TIMER_1MS = if ($Timer1ms) { '1' } else { $null }
    $env:NIDA_VIDEO_BENCH_HISTORICAL_SAMPLING = if ($HistoricalSampling) { '1' } else { $null }
    $env:NIDA_VIDEO_BENCH_LEGACY_WAIT = if ($LegacyWait) { '1' } else { $null }
    $env:NIDA_VIDEO_BENCH_PERIODIC_WAKE = if ($PeriodicWake) { '1' } else { $null }
    $env:NIDA_VIDEO_BENCH_NO_READINESS = if ($NoReadiness) { '1' } else { $null }
    $env:NIDA_VIDEO_BENCH_COPY_POLL = if ($CopyPoll) { '1' } else { $null }
    if ($Trace -and (Test-Path -LiteralPath $env:NIDA_VIDEO_PACING_TRACE)) { throw 'Trace already exists' }
    $sourcePaths = @('src/internal/video/mod.rs','src/internal/video/hap.rs','src/internal/video/cadence.rs','src/internal/video/command_channel.rs','src/internal/video/frame_mailbox.rs','src/internal/video/frame_readiness.rs','src/internal/video/pacing_trace.rs','src/internal/video/playback_diagnostics.rs','src/internal/video/gpu_windows.rs','src/internal/video/gpu_windows/qualification.rs','src/internal/deck/render.rs','src/internal/deck/playlist.rs','src/internal/deck/slides.rs','src/internal/channel/mod.rs','src/internal/mixer/render.rs','src/internal/renderer/context.rs','src/usecases/ui/runner/mod.rs','src/usecases/ui/runner/engine_thread.rs','tools/gpu-video-checks/src/lib.rs')
    $manifest = [ordered]@{
        started_utc = [DateTimeOffset]::UtcNow.ToString('o')
        source_hashes = @($sourcePaths | ForEach-Object { Get-FileHash -LiteralPath $_ -Algorithm SHA256 })
        phase_ms = $PhaseMs; poll_only = [bool]$PollOnly; timer_1ms = [bool]$Timer1ms
        historical_sampling = [bool]$HistoricalSampling
        legacy_wait = [bool]$LegacyWait; periodic_wake = [bool]$PeriodicWake
        no_readiness = [bool]$NoReadiness; clips = $Clips
        copy_poll = [bool]$CopyPoll
        trace = $env:NIDA_VIDEO_PACING_TRACE
    }
    ./scripts/measure-video.ps1 -TestBinary $TestBinary -Clip $Clip -Output $Output -Seconds $Seconds -Clips $Clips -Repeats 1 -Playback -GpuResident
    $manifest['finished_utc'] = [DateTimeOffset]::UtcNow.ToString('o')
    if ($Trace) { $manifest['trace_sha256'] = (Get-FileHash -LiteralPath $env:NIDA_VIDEO_PACING_TRACE -Algorithm SHA256).Hash }
    $manifest | ConvertTo-Json -Depth 6 > "$Output.pacing.json"
} finally {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
