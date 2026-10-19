//! Compare software and hardware seek-to-frame latency through `VideoPlayer`,
//! including decode, hardware download and the normal RGBA conversion.
//! Set `VARDA_VIDEO_BENCH_CLIP` to a representative H.264/HEVC clip.
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use varda::video::VideoPlayer;

fn video_decode(c: &mut Criterion) {
    let Some(path) = std::env::var_os("VARDA_VIDEO_BENCH_CLIP") else {
        eprintln!("Set VARDA_VIDEO_BENCH_CLIP to run video decode benchmarks");
        return;
    };
    let mut group = c.benchmark_group("video_seek_decode_rgba");
    group.sample_size(10);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    for (label, hardware) in [("software", false), ("hardware", true)] {
        let mut player = VideoPlayer::open(&path, hardware).expect("open benchmark clip");
        player.set_playing(false);
        let at = (player.duration() * 0.5).min(1.0);
        player.seek_and_reset(at).expect("seek benchmark clip");
        player
            .next_frame()
            .expect("decode benchmark frame")
            .expect("frame");
        if hardware && !player.hardware_decode() {
            eprintln!("Hardware unavailable for this clip; skipping hardware benchmark");
            continue;
        }
        let other = (at + 0.5).min(player.duration() - 0.5 / player.frame_rate());
        let mut alternate = false;
        group.bench_function(label, |b| {
            b.iter(|| {
                // Change the requested frame: repeated identical seeks can
                // correctly reuse the image and would not measure decoding.
                alternate = !alternate;
                let target = if alternate { other } else { at };
                player.seek_and_reset(black_box(target)).expect("seek");
                let bytes = player.next_frame().expect("decode").expect("frame").len();
                black_box(bytes)
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("video_scrub_forward_rgba");
    group.sample_size(10);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    for (label, hardware) in [("software", false), ("hardware", true)] {
        let mut player = VideoPlayer::open(&path, hardware).expect("open benchmark clip");
        player.set_playing(false);
        let base = (player.duration() * 0.25).min(1.0);
        player.seek_and_reset(base).expect("seek");
        player.next_frame().expect("decode").expect("frame");
        if hardware && !player.hardware_decode() {
            continue;
        }
        let step = 1.0 / player.frame_rate();
        let mut index = 0_u32;
        // Eight forward frames followed by a backward reset. Every step
        // asks for a different frame, so holding a cached image cannot win.
        group.bench_function(label, |b| {
            b.iter(|| {
                index = (index + 1) % 8;
                player
                    .seek_and_reset(black_box(base + f64::from(index) * step))
                    .expect("seek");
                black_box(player.next_frame().expect("decode").expect("frame").len())
            });
        });
    }
    group.finish();
}

criterion_group!(benches, video_decode);
criterion_main!(benches);
