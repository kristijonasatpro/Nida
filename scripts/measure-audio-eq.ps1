param(
    [Parameter(Mandatory=$true)][string]$Binary,
    [Parameter(Mandatory=$true)][string]$OutputRoot,
    [ValidateSet('disabled','sources','sources-and-buses')][string]$Mode='disabled',
    [ValidateSet('mixer_block_budget_probe','equalizer_bank_budget_probe')][string]$Test='mixer_block_budget_probe',
    [switch]$Compressor
)
$ErrorActionPreference='Stop'
$binaryPath=(Resolve-Path -LiteralPath $Binary).Path
New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
$output=(Resolve-Path -LiteralPath $OutputRoot).Path
$previousMode=$env:NIDA_AUDIO_PROBE_EQ
$previousCompressor=$env:NIDA_AUDIO_PROBE_COMPRESSOR
if($Compressor -and $Test -ne 'mixer_block_budget_probe') {throw 'Compressor measurement requires the production mix-core probe.'}
try {
    if($Mode -eq 'disabled') {Remove-Item Env:NIDA_AUDIO_PROBE_EQ -ErrorAction SilentlyContinue}
    else {$env:NIDA_AUDIO_PROBE_EQ=$Mode}
    if($Compressor) {$env:NIDA_AUDIO_PROBE_COMPRESSOR='1'}
    else {Remove-Item Env:NIDA_AUDIO_PROBE_COMPRESSOR -ErrorAction SilentlyContinue}
    $timer=[System.Diagnostics.Stopwatch]::StartNew()
    $process=Start-Process -FilePath $binaryPath -ArgumentList @($Test,'--ignored','--nocapture','--test-threads=1') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $output 'stdout.log') -RedirectStandardError (Join-Path $output 'stderr.log')
    $peakWorkingSet=0L
    while(-not $process.HasExited) {
        try {$process.Refresh();$peakWorkingSet=[Math]::Max($peakWorkingSet,$process.WorkingSet64)} catch {}
        Start-Sleep -Milliseconds 20
    }
    $process.WaitForExit()
    $timer.Stop()
    $cpuSeconds=$null
    try {$cpuSeconds=$process.TotalProcessorTime.TotalSeconds} catch {}
    $metadata=@{
        mode=$Mode
        compressor=[bool]$Compressor
        test=$Test
        binary=$binaryPath
        sha256=(Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
        exit_code=$process.ExitCode
        wall_seconds=$timer.Elapsed.TotalSeconds
        process_cpu_seconds=$cpuSeconds
        sampled_peak_working_set_bytes=$peakWorkingSet
        processor_count=[Environment]::ProcessorCount
        note='Busy-loop audio probe, no playback clock/rendering/devices; CPU includes fixture refill and setup. Memory is sampled, not an allocator count.'
    }
    $metadata | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'metadata.json')
    Get-Content -LiteralPath (Join-Path $output 'stdout.log')
    if($process.ExitCode -ne 0) {throw "Audio probe failed: $($process.ExitCode). See stderr.log."}
} finally {
    $env:NIDA_AUDIO_PROBE_EQ=$previousMode
    $env:NIDA_AUDIO_PROBE_COMPRESSOR=$previousCompressor
}
