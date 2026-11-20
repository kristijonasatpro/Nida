//! Explicit hardware qualification; never runs in the ordinary test suite.
use crate::{
    renderer::GpuContext,
    video::{DecodedPixels, FramePlayer, LoopMode, VideoPlayer},
};

#[test]
#[ignore = "creates a D3D12 device; requires an idle GPU"]
fn copy_completion_notification_rechecks_fence_and_reuses_event() {
    use std::{os::windows::io::AsRawHandle, time::Duration};
    let gpu = GpuContext::new_headless().unwrap();
    let device = unsafe { gpu.device.as_hal::<wgpu::hal::api::Dx12>() }
        .unwrap()
        .raw_device()
        .clone();
    let fence: super::dx12::ID3D12Fence =
        unsafe { device.CreateFence(0, super::dx12::D3D12_FENCE_FLAG_NONE) }.unwrap();
    let event = super::CopyCompletion::new().expect("native completion event");
    assert!(event.arm(&fence, 1));
    // A timeout is not completion, and must not release registered ownership.
    assert!(event.wait(Duration::ZERO));
    assert_eq!(unsafe { fence.GetCompletedValue() }, 0);
    let producer = fence.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        unsafe { producer.Signal(1) }.unwrap();
    });
    assert!(event.wait(Duration::from_secs(2)));
    worker.join().unwrap();
    assert_eq!(unsafe { fence.GetCompletedValue() }, 1);

    assert!(event.arm(&fence, 2));
    // A stale/spurious event must not be mistaken for the new fence value.
    unsafe { windows_sys::Win32::System::Threading::SetEvent(event.0.as_raw_handle()) };
    assert!(event.wait(Duration::ZERO));
    assert_eq!(unsafe { fence.GetCompletedValue() }, 1);
    unsafe { fence.Signal(2) }.unwrap();
    assert!(event.wait(Duration::from_secs(2)));
    assert_eq!(unsafe { fence.GetCompletedValue() }, 2);
}

#[test]
#[ignore = "requires an idle GPU and VARDA_REQUIRE_GPU_RESIDENT=1; performs GPU readback for picture comparison"]
fn gpu_resident_picture_seek_pool_and_fallback() {
    assert!(
        std::env::var_os("VARDA_REQUIRE_GPU_RESIDENT").is_some(),
        "Hardware qualification must be explicitly requested"
    );
    let path = std::env::var_os("VARDA_VIDEO_BENCH_CLIP")
        .expect("Set VARDA_VIDEO_BENCH_CLIP to a qualification fixture");
    let gpu = GpuContext::new_headless().unwrap();
    let mut native = VideoPlayer::open(&path, false).unwrap();
    native
        .try_enable_gpu(&gpu)
        .expect("D3D12VA is required; fallback is a failure");
    native.set_playing(false);
    let mut reference = VideoPlayer::open(&path, false).unwrap();
    reference.set_playing(false);
    let width = native.width();
    let height = native.height();
    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GPU video qualification target"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let stride = (width * 4).div_ceil(256) * 256;
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Qualification-only video readback"),
        size: u64::from(stride) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    for fraction in [0.0, 0.5, 0.1, 0.75, 0.0] {
        let at = native.playback.duration * fraction;
        native.seek_and_reset(at).unwrap();
        native.next_frame().unwrap().expect("seek picture");
        let frame = native
            .gpu_pending
            .take()
            .expect("GPU payload required after seek");
        reference.seek_and_reset(at).unwrap();
        reference.next_frame().unwrap().expect("CPU reference");
        let packing = if frame.params.layout[1] == 6 {
            ffmpeg_next::format::Pixel::P010LE
        } else {
            ffmpeg_next::format::Pixel::NV12
        };
        // Use the established hardware-picture test's common YUV packing.
        // Merely rearrange software-decoded samples; do not change the limit
        // or use GPU-decoded samples as the software correctness reference.
        let mut repacked = crate::video::tests::repack_reference(&reference.decoded, packing);
        unsafe {
            ffmpeg_next::ffi::av_frame_copy_props(
                repacked.as_mut_ptr(),
                reference.decoded.as_ptr(),
            );
        }
        reference.decoded = repacked;
        reference.convert_current().unwrap();
        let want = reference.frame_data.clone();
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        frame.encode(&mut encoder, &target);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        gpu.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                tx.send(result).unwrap();
            });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        rx.recv().unwrap().unwrap();
        let mapped = readback.slice(..).get_mapped_range().unwrap();
        let got: Vec<u8> = mapped
            .chunks_exact(stride as usize)
            .flat_map(|row| row[..(width * 4) as usize].iter().copied())
            .collect();
        drop(mapped);
        readback.unmap();
        let mut download = crate::video::hwdecode::Download::default();
        crate::video::hwdecode::to_memory(&native.decoded, &mut download).unwrap();
        let mut packed_reference = VideoPlayer::open(&path, false).unwrap();
        packed_reference.decoded = download.frame;
        packed_reference.convert_current().unwrap();
        let packed = &packed_reference.frame_data;
        let packed_difference = packed
            .iter()
            .zip(&got)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum::<u64>() as f64
            / packed.len() as f64;
        eprintln!("GPU_PICTURE at={at} packed_mean={packed_difference}");
        if let Some(directory) = std::env::var_os("NIDA_TEST_GPU_VIDEO_DUMP") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            for (name, bytes) in [("gpu", &got), ("cpu-planar", &want), ("cpu-packed", packed)] {
                image::save_buffer(
                    directory.join(format!("{name}.png")),
                    bytes,
                    width,
                    height,
                    image::ColorType::Rgba8,
                )
                .unwrap();
                std::fs::write(directory.join(format!("{name}.rgba")), bytes).unwrap();
            }
        }
        assert_eq!(want.len(), got.len());
        let difference = want
            .iter()
            .zip(&got)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum::<u64>() as f64
            / want.len() as f64;
        // Preserve the existing video hardware picture test's acceptance limit.
        assert!(
            difference < 0.5,
            "seek {at}: mean picture difference {difference}"
        );
        assert!(
            gpu.errors.take_faults().is_empty(),
            "GPU validation failure"
        );
    }
    // Decoder surfaces are never held by display: owned planes exhaust at
    // three slots, an unsubmitted frame can be dropped, then production resumes.
    let converter = native.gpu.as_mut().unwrap();
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(
            converter
                .copy(&native.decoded, (width, height), native.quarter_turns, 0.0)
                .unwrap()
                .unwrap(),
        );
    }
    assert!(converter
        .copy(&native.decoded, (width, height), native.quarter_turns, 0.0)
        .unwrap()
        .is_none());
    // Pressure retains the current decoder picture. A playing worker must use
    // bounded periodic retries even when its eligibility deadline is in the past.
    native.set_playing(true);
    native.playback.last_advance = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap();
    assert!(native.next_picture_deadline().is_some());
    let position = native.playback.position;
    let timestamp = native.decoded.timestamp().unwrap() as f64 * native.time_base - native.start;
    assert!(native.convert_current().unwrap());
    assert!(native.gpu_retry);
    assert_eq!(native.next_picture_deadline(), None);
    let pool = std::sync::Mutex::new(Vec::new());
    for _ in 0..3 {
        assert!(native.next_decoded_frame(&pool).unwrap().is_none());
        assert!(native.gpu_retry);
        assert_eq!(native.next_picture_deadline(), None);
        assert_eq!(native.playback.position, position);
    }
    drop(held.pop());
    let recovered = native
        .next_decoded_frame(&pool)
        .unwrap()
        .expect("retained picture retries");
    let DecodedPixels::Gpu(frame) = recovered.color_data else {
        panic!("pool pressure must not introduce a CPU transfer");
    };
    assert_eq!(frame.timestamp, timestamp);
    assert!(!native.gpu_retry);
    assert_eq!(native.playback.position, position);
    drop(frame);
    native.set_playing(false);
    drop(held);
    let counters = native.delivery.snapshot();
    assert!(counters.gpu_frames >= 5);
    assert_eq!(counters.downloaded_bytes, 0);
    assert_eq!(counters.uploaded_bytes, 0);
    assert!(counters.peak_slots <= 3);
    assert_eq!(counters.occupied_slots, 0);
    native.playback.loop_mode = LoopMode::PingPong;
    native.playback.show_frame = true;
    assert_ne!(native.next_frame().unwrap().unwrap(), [] as [u8; 0]);
    assert!(native.gpu.is_none());
    assert!(native
        .delivery
        .snapshot()
        .fallback_reason
        .unwrap()
        .contains("Ping-pong"));
}
