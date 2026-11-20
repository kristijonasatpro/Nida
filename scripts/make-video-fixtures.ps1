param([string]$Directory = 'tests/media/video-performance', [ValidateRange(1, 3600)][int]$Seconds = 20)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $Directory | Out-Null
foreach ($case in @(@('1080p60', '1920x1080', 60), @('4k30', '3840x2160', 30), @('4k60', '3840x2160', 60))) {
    $name, $size, $rate = $case
    & ffmpeg -hide_banner -loglevel error -y -f lavfi -i "testsrc2=size=${size}:rate=$rate" -t $Seconds -an -vf 'setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709' -c:v libx264 -preset veryfast -crf 18 -g ($rate * 2) -bf 2 -pix_fmt yuv420p -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv "$Directory/h264-$name.mp4"
    if ($LASTEXITCODE -ne 0) { throw "Encoding $name failed" }
}
foreach ($codec in @('hevc', 'hevc10', 'hdr10', 'av1')) {
    $encoder = if ($codec -eq 'av1') { @('-c:v', 'libsvtav1', '-preset', '12') } else { @('-c:v', 'libx265', '-preset', 'veryfast', '-tag:v', 'hvc1') }
    $pixel = if ($codec -eq 'hevc') { 'yuv420p' } else { 'yuv420p10le' }
    $tags = if ($codec -eq 'hdr10') { @('-colorspace', 'bt2020nc', '-color_primaries', 'bt2020', '-color_trc', 'smpte2084') } else { @('-colorspace', 'bt709', '-color_primaries', 'bt709', '-color_trc', 'bt709') }
    $filter = if ($codec -eq 'hdr10') { 'setparams=range=limited:color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc' } else { 'setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709' }
    & ffmpeg -hide_banner -loglevel error -y -f lavfi -i 'testsrc2=size=1920x1080:rate=60' -t $Seconds -an -vf $filter @encoder -g 120 -pix_fmt $pixel @tags -color_range tv "$Directory/$codec-1080p60.mp4"
    if ($LASTEXITCODE -ne 0) { throw "Encoding $codec failed" }
}
foreach ($clip in Get-ChildItem -LiteralPath $Directory -Filter '*.mp4') {
    $probe = & ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,pix_fmt,avg_frame_rate,nb_frames,color_transfer,color_primaries,color_space,color_range -of json $clip.FullName | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw "Probing $($clip.Name) failed" }
    $stream = $probe.streams[0]
    $hdr = $clip.Name -like 'hdr10-*'
    $transfer = if ($hdr) { 'smpte2084' } else { 'bt709' }
    $primaries = if ($hdr) { 'bt2020' } else { 'bt709' }
    $matrix = if ($hdr) { 'bt2020nc' } else { 'bt709' }
    $expectedPixel = if ($clip.Name -match '^(hevc10|hdr10|av1)-') { 'yuv420p10le' } else { 'yuv420p' }
    $expectedCodec = if ($clip.Name -like 'h264-*') { 'h264' } elseif ($clip.Name -like 'av1-*') { 'av1' } else { 'hevc' }
    $width, $height = if ($clip.Name -like '*-4k*') { 3840, 2160 } else { 1920, 1080 }
    $rate = if ($clip.Name -like '*-4k30.mp4') { 30 } else { 60 }
    if ($stream.pix_fmt -ne $expectedPixel -or $stream.codec_name -ne $expectedCodec -or $stream.width -ne $width -or $stream.height -ne $height -or $stream.avg_frame_rate -ne "$rate/1" -or [int]$stream.nb_frames -ne $Seconds * $rate) {
        throw "Incorrect codec, dimensions, pixel depth or frame count in $($clip.Name)"
    }
    if ($stream.color_transfer -ne $transfer -or $stream.color_primaries -ne $primaries -or $stream.color_space -ne $matrix -or $stream.color_range -ne 'tv') {
        throw "Missing or incorrect colour tags in $($clip.Name)"
    }
}
Get-ChildItem -LiteralPath $Directory -Filter '*.mp4' | Get-FileHash -Algorithm SHA256 | Format-Table -AutoSize
