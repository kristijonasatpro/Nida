# FFmpeg 9 shared SDK and libclang for the core Windows build.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$ToolsDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (!$IsWindows) { throw 'Run this from x64 Developer PowerShell on Windows.' }
Get-Command dumpbin.exe, lib.exe -ErrorAction Stop | Out-Null
New-Item -ItemType Directory -Path $ToolsDirectory -Force | Out-Null
$ToolsDirectory = (Resolve-Path -LiteralPath $ToolsDirectory).Path
$headers = @{ 'User-Agent' = 'Nida-Windows-build' }
if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
$release = Invoke-RestMethod 'https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/tags/latest' -Headers $headers
$asset = @($release.assets | Where-Object name -eq 'ffmpeg-n9.0-latest-win64-gpl-shared-9.0.zip')
if ($asset.Count -ne 1 -or $asset[0].digest -notmatch '^sha256:([a-f0-9]{64})$') { throw 'FFmpeg 9 asset or its published SHA256 is unavailable.' }
$expectedHash = $Matches[1]
$archive = Join-Path $ToolsDirectory 'ffmpeg.zip'
Invoke-WebRequest $asset[0].browser_download_url -OutFile $archive
if ((Get-FileHash $archive -Algorithm SHA256).Hash -ne $expectedHash) { throw 'FFmpeg archive checksum mismatch.' }
$extract = Join-Path $ToolsDirectory 'ffmpeg-sdk'
if (Test-Path $extract) { throw "Use a fresh tools directory: $extract exists." }
Expand-Archive -LiteralPath $archive -DestinationPath $extract
$root = (Get-ChildItem $extract -Directory | Select-Object -First 1).FullName
if (!(Test-Path (Join-Path $root 'include/libavcodec/avcodec.h'))) { throw 'FFmpeg development headers missing.' }
foreach ($dll in Get-ChildItem (Join-Path $root 'bin') -Filter '*.dll') {
    if ($dll.Name -notmatch '^(avcodec|avdevice|avfilter|avformat|avutil|swresample|swscale|postproc)-\d+\.dll$') { continue }
    $libraryName = $dll.BaseName -replace '-\d+$', ''
    $exports = @(& dumpbin.exe /nologo /exports $dll.FullName | ForEach-Object {
        if ($_ -match '^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]+\s+(\S+)') { '  ' + $Matches[1] }
    })
    if ($LASTEXITCODE -ne 0 -or !$exports.Count) { throw "Cannot read exports: $($dll.Name)" }
    $definition = Join-Path $root "lib/$libraryName.def"
    [IO.File]::WriteAllLines($definition, @("LIBRARY $($dll.Name)", 'EXPORTS') + $exports, [Text.Encoding]::ASCII)
    & lib.exe /nologo /machine:x64 "/def:$definition" "/out:$(Join-Path $root "lib/$libraryName.lib")" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Cannot make MSVC import library: $libraryName" }
}
$clangArchive = Join-Path $ToolsDirectory 'libclang.zip'
$clangRoot = Join-Path $ToolsDirectory 'libclang'
Invoke-WebRequest 'https://api.nuget.org/v3-flatcontainer/libclang.runtime.win-x64/20.1.2/libclang.runtime.win-x64.20.1.2.nupkg' -OutFile $clangArchive
Expand-Archive -LiteralPath $clangArchive -DestinationPath $clangRoot
@{ ffmpeg_url = $asset[0].browser_download_url; ffmpeg_sha256 = $expectedHash
   ffmpeg_published = $asset[0].updated_at; build_recipes = 'https://github.com/BtbN/FFmpeg-Builds'
} | ConvertTo-Json | Set-Content (Join-Path $root 'nida-build-inputs.json') -Encoding utf8
$env:FFMPEG_DIR = $root
$env:LIBCLANG_PATH = Join-Path $clangRoot 'runtimes/win-x64/native'
$env:PATH = "$(Join-Path $root 'bin');$env:PATH"
if ($env:GITHUB_ENV) {
    "FFMPEG_DIR=$root" >> $env:GITHUB_ENV
    "LIBCLANG_PATH=$env:LIBCLANG_PATH" >> $env:GITHUB_ENV
    (Join-Path $root 'bin') >> $env:GITHUB_PATH
}
Write-Host "FFmpeg SDK ready: $root"
