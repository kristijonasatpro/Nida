use std::ffi::CString;
use std::hint::black_box;
use std::time::Instant;

use ffmpeg_next::{filter, format, frame, ChannelLayout};
use serde_json::json;

const FRAMES: usize = 480;
const RATE: u32 = 48_000;
type Block = [[f32; 2]; FRAMES];
const COMPRESSOR: &str = "acompressor@comp=threshold=0.125:ratio=4:attack=10:release=100:knee=1:makeup=1:link=maximum:detection=peak";

struct Probe {
    graph: filter::Graph,
    input: frame::Audio,
    output: frame::Audio,
    double: bool,
}

impl Probe {
    fn new(spec: &str, double: bool) -> Self {
        let mut graph = filter::Graph::new();
        let sample = if double { "dbl" } else { "flt" };
        unsafe { (*graph.as_mut_ptr()).nb_threads = 1 };
        graph
            .add(
                &filter::find("abuffer").expect("abuffer unavailable"),
                "in",
                &format!(
                    "time_base=1/48000:sample_rate=48000:sample_fmt={sample}:channel_layout=stereo"
                ),
            )
            .unwrap();
        graph
            .add(
                &filter::find("abuffersink").expect("abuffersink unavailable"),
                "out",
                "",
            )
            .unwrap();
        graph
            .output("in", 0)
            .unwrap()
            .input("out", 0)
            .unwrap()
            .parse(&format!(
                "{spec},aformat=sample_fmts={sample}:sample_rates=48000:channel_layouts=stereo"
            ))
            .unwrap();
        graph.validate().unwrap();
        let sample_format = if double {
            format::Sample::F64(format::sample::Type::Packed)
        } else {
            format::Sample::F32(format::sample::Type::Packed)
        };
        let mut input = frame::Audio::new(sample_format, FRAMES, ChannelLayout::STEREO);
        input.set_rate(RATE);
        Self {
            graph,
            input,
            output: frame::Audio::empty(),
            double,
        }
    }

    fn process(&mut self, input: &Block, output: &mut Block, pts: i64) {
        if self.double {
            for (target, source) in self.input.plane_mut::<(f64, f64)>(0).iter_mut().zip(input) {
                *target = (f64::from(source[0]), f64::from(source[1]));
            }
        } else {
            for (target, source) in self.input.plane_mut::<(f32, f32)>(0).iter_mut().zip(input) {
                *target = (source[0], source[1]);
            }
        }
        self.input.set_pts(Some(pts));
        self.graph
            .get("in")
            .unwrap()
            .source()
            .add(&self.input)
            .unwrap();
        self.graph
            .get("out")
            .unwrap()
            .sink()
            .frame(&mut self.output)
            .unwrap();
        assert_eq!(self.output.samples(), FRAMES);
        assert_eq!(self.output.pts(), Some(pts));
        if self.double {
            for (target, source) in output.iter_mut().zip(self.output.plane::<(f64, f64)>(0)) {
                *target = [source.0 as f32, source.1 as f32];
            }
        } else {
            for (target, source) in output.iter_mut().zip(self.output.plane::<(f32, f32)>(0)) {
                *target = [source.0, source.1];
            }
        }
        std::mem::swap(&mut self.input, &mut self.output);
    }

    fn command(&mut self, name: &str, value: &str) -> Result<(), ffmpeg_next::Error> {
        let target = CString::new("acompressor@comp").unwrap();
        let name = CString::new(name).unwrap();
        let value = CString::new(value).unwrap();
        let result = unsafe {
            ffmpeg_next::ffi::avfilter_graph_send_command(
                self.graph.as_mut_ptr(),
                target.as_ptr(),
                name.as_ptr(),
                value.as_ptr(),
                std::ptr::null_mut(),
                0,
                0,
            )
        };
        if result < 0 {
            Err(ffmpeg_next::Error::from(result))
        } else {
            Ok(())
        }
    }
}

fn meters(block: &Block) -> ([f32; 2], [f64; 2]) {
    let mut peak = [0.0_f32; 2];
    let mut power = [0.0_f64; 2];
    for frame in block {
        for channel in 0..2 {
            peak[channel] = peak[channel].max(frame[channel].abs());
            power[channel] += f64::from(frame[channel]).powi(2);
        }
    }
    (peak, power.map(|sum| (sum / FRAMES as f64).sqrt()))
}

fn fixtures() -> Vec<Block> {
    (0..50)
        .map(|block| {
            std::array::from_fn(|frame| {
                let position = block * FRAMES + frame;
                let envelope = if block < 10 {
                    0.03
                } else if block < 35 {
                    0.7
                } else {
                    0.08
                };
                let sample = (position as f32 * std::f32::consts::TAU * 997.0 / RATE as f32).sin()
                    * envelope;
                [sample, sample * 0.35]
            })
        })
        .collect()
}

#[cfg(windows)]
fn cpu_seconds() -> Option<f64> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn GetProcessTimes(
            process: *mut std::ffi::c_void,
            creation: *mut u64,
            exit: *mut u64,
            kernel: *mut u64,
            user: *mut u64,
        ) -> i32;
    }
    let (mut creation, mut exit, mut kernel, mut user) = (0, 0, 0, 0);
    let success = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    (success != 0).then_some((kernel + user) as f64 / 10_000_000.0)
}

#[cfg(not(windows))]
fn cpu_seconds() -> Option<f64> {
    None
}

fn benchmark(mode: &str, strips: usize, blocks: usize, repetition: usize) {
    let mut probes: Vec<_> = (0..strips)
        .filter_map(|_| match mode {
            "native-copy" => None,
            "anull-f32" => Some(Probe::new("anull", false)),
            "compressor-f32" => Some(Probe::new(COMPRESSOR, false)),
            "compressor-f64" => Some(Probe::new(COMPRESSOR, true)),
            _ => unreachable!(),
        })
        .collect();
    let fixtures = fixtures();
    let mut output = [[0.0; 2]; FRAMES];
    for block in 0..100 {
        for probe in &mut probes {
            probe.process(
                &fixtures[block % fixtures.len()],
                &mut output,
                (block * FRAMES) as i64,
            );
        }
    }
    let mut durations = Vec::with_capacity(blocks);
    let cpu_before = cpu_seconds();
    let start = Instant::now();
    for block in 0..blocks {
        let input = &fixtures[block % fixtures.len()];
        let block_start = Instant::now();
        for strip in 0..strips {
            black_box(meters(black_box(input)));
            if probes.is_empty() {
                output.copy_from_slice(black_box(input));
            } else {
                probes[strip].process(
                    black_box(input),
                    &mut output,
                    ((block + 100) * FRAMES) as i64,
                );
            }
            black_box(meters(black_box(&output)));
        }
        durations.push(block_start.elapsed().as_secs_f64() * 1e6);
    }
    let wall = start.elapsed().as_secs_f64();
    let cpu = cpu_before
        .zip(cpu_seconds())
        .map(|(before, after)| after - before);
    durations.sort_by(f64::total_cmp);
    let quantile = |percent: usize| {
        durations[((blocks * percent).div_ceil(100))
            .saturating_sub(1)
            .min(blocks - 1)]
    };
    println!(
        "{}",
        json!({"mode":mode,"strips":strips,"blocks":blocks,"repetition":repetition,
        "p50_us":quantile(50),"p95_us":quantile(95),"p99_us":quantile(99),
        "max_us":durations[blocks-1],"missed_10ms":durations.iter().filter(|time| **time > 10000.0).count(),
        "wall_s":wall,"cpu_s":cpu,"cpu_percent_one_core":cpu.map(|time|time/wall*100.0),
        "cpu_percent_one_core_at_realtime":cpu.map(|time|time/(blocks as f64*0.01)*100.0),
        "realtime_factor":blocks as f64*0.01/wall})
    );
}

fn main() {
    ffmpeg_next::init().unwrap();
    let args: Vec<_> = std::env::args().collect();
    let blocks = args
        .get(1)
        .map_or(3000, |arg| arg.parse::<usize>().unwrap());
    assert!(blocks > 0);
    let mut graph = Probe::new(COMPRESSOR, false);
    eprintln!(
        "libavutil version (packed): {}\n{}",
        ffmpeg_next::util::version(),
        graph.graph.dump()
    );
    graph.command("ratio", "4").unwrap();
    for repetition in 0..3 {
        for strips in [1, 8, 16, 32] {
            for mode in [
                "native-copy",
                "anull-f32",
                "compressor-f32",
                "compressor-f64",
            ] {
                benchmark(mode, strips, blocks, repetition);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(probe: &mut Probe, signal: [f32; 2]) -> Block {
        let mut output = [[0.0; 2]; FRAMES];
        for block in 0..100 {
            probe.process(&[signal; FRAMES], &mut output, (block * FRAMES) as i64);
        }
        output
    }

    #[test]
    fn steady_curve_and_stereo_link_match_expected_gain() {
        for double in [false, true] {
            let mut probe = Probe::new(COMPRESSOR, double);
            let output = settled(&mut probe, [0.5, 0.125]);
            let expected = 0.125_f32 * 4.0_f32.powf(0.25);
            assert!((output[FRAMES - 1][0] - expected).abs() < 0.00001);
            assert!((output[FRAMES - 1][1] * 4.0 - output[FRAMES - 1][0]).abs() < 0.00001);
        }
    }

    #[test]
    fn silence_and_below_threshold_remain_unchanged() {
        let mut probe = Probe::new(COMPRESSOR, false);
        assert_eq!(settled(&mut probe, [0.0; 2]), [[0.0; 2]; FRAMES]);
        let output = settled(&mut probe, [0.03, -0.02]);
        assert_eq!(output, [[0.03, -0.02]; FRAMES]);
    }

    #[test]
    fn unity_ratio_and_dry_mix_are_transparent() {
        for option in ["ratio", "mix"] {
            let mut probe = Probe::new(COMPRESSOR, false);
            probe
                .command(option, if option == "ratio" { "1" } else { "0" })
                .unwrap();
            assert_eq!(settled(&mut probe, [0.7, -0.3]), [[0.7, -0.3]; FRAMES]);
        }
    }

    #[test]
    fn float_and_double_bridges_agree_on_dynamic_signal() {
        let mut float = Probe::new(COMPRESSOR, false);
        let mut double = Probe::new(COMPRESSOR, true);
        let mut float_output = [[0.0; 2]; FRAMES];
        let mut double_output = float_output;
        for (block, input) in fixtures().iter().cycle().take(150).enumerate() {
            float.process(input, &mut float_output, (block * FRAMES) as i64);
            double.process(input, &mut double_output, (block * FRAMES) as i64);
            for (left, right) in float_output
                .iter()
                .flatten()
                .zip(double_output.iter().flatten())
            {
                assert!(left.is_finite() && right.is_finite());
                assert!((left - right).abs() < 0.000001);
            }
        }
    }

    #[test]
    fn attack_release_and_runtime_update_preserve_envelope() {
        let mut probe = Probe::new(COMPRESSOR, false);
        let mut control = Probe::new(COMPRESSOR, false);
        let mut output = [[0.0; 2]; FRAMES];
        let mut expected = output;
        probe.process(&[[0.8; 2]; FRAMES], &mut output, 0);
        control.process(&[[0.8; 2]; FRAMES], &mut expected, 0);
        assert!(output[0][0] > output[FRAMES - 1][0]);
        probe.command("ratio", "4").unwrap();
        probe.process(&[[0.8; 2]; FRAMES], &mut output, 480);
        control.process(&[[0.8; 2]; FRAMES], &mut expected, 480);
        assert_eq!(output, expected);
        probe.process(&[[0.03; 2]; FRAMES], &mut output, 960);
        assert!(output[0][0] < output[FRAMES - 1][0]);
        assert!((settled(&mut probe, [0.03; 2])[FRAMES - 1][0] - 0.03).abs() < 0.000001);
        probe.command("ratio", "1").unwrap();
        assert_eq!(settled(&mut probe, [0.8; 2]), [[0.8; 2]; FRAMES]);
    }

    #[test]
    fn invalid_runtime_parameter_is_rejected() {
        let mut probe = Probe::new(COMPRESSOR, false);
        assert!(probe.command("ratio", "0").is_err());
        assert!(settled(&mut probe, [0.8; 2])
            .iter()
            .flatten()
            .all(|value| value.is_finite()));
    }

    #[test]
    fn runtime_makeup_is_abrupt_and_requires_host_smoothing() {
        let mut probe = Probe::new(COMPRESSOR, false);
        let before = settled(&mut probe, [0.03; 2]);
        probe.command("makeup", "2").unwrap();
        let mut after = [[0.0; 2]; FRAMES];
        probe.process(&[[0.03; 2]; FRAMES], &mut after, 48_000);
        assert_eq!(after[0][0], before[FRAMES - 1][0] * 2.0);
    }

    #[test]
    fn hot_signal_then_silence_stays_finite_and_has_no_tail() {
        let mut probe = Probe::new(COMPRESSOR, false);
        settled(&mut probe, [0.9, -0.9]);
        assert_eq!(settled(&mut probe, [0.0; 2]), [[0.0; 2]; FRAMES]);
    }

    #[test]
    fn double_bridge_reuses_pcm_storage() {
        let mut probe = Probe::new(COMPRESSOR, true);
        let storage = probe.input.data(0).as_ptr();
        let mut output = [[0.0; 2]; FRAMES];
        for block in 0..100 {
            probe.process(&[[0.5; 2]; FRAMES], &mut output, block * FRAMES as i64);
            assert_eq!(probe.input.data(0).as_ptr(), storage);
        }
    }

    #[test]
    fn pre_post_meters_measure_processor_not_fader() {
        let mut probe = Probe::new(COMPRESSOR, false);
        let input = [[0.5, 0.125]; FRAMES];
        let output = settled(&mut probe, input[0]);
        let (pre_peak, pre_rms) = meters(&input);
        let (post_peak, post_rms) = meters(&output);
        assert_eq!(pre_peak, [0.5, 0.125]);
        assert_eq!(pre_rms, [0.5, 0.125]);
        assert!(post_peak[0] < pre_peak[0]);
        let reduction_db = 20.0 * (pre_rms[0] / post_rms[0]).log10();
        assert!((reduction_db - 9.0309).abs() < 0.001);
    }

    #[test]
    fn flushing_has_no_delayed_audio() {
        let mut probe = Probe::new(COMPRESSOR, false);
        settled(&mut probe, [0.8; 2]);
        probe.graph.get("in").unwrap().source().flush().unwrap();
        assert_eq!(
            probe
                .graph
                .get("out")
                .unwrap()
                .sink()
                .frame(&mut probe.output),
            Err(ffmpeg_next::Error::Eof)
        );
    }
}
