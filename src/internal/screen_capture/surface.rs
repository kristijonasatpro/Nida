//! A captured frame still in the GPU surface macOS captured it into, and its
//! copy into the capture's texture on the GPU (macOS).
//!
//! Copied through memory, each frame of a 1080p capture moved 8 MB three
//! times: out of the surface on the capture queue, into wgpu's staging memory
//! on the engine thread — wgpu's queue held meanwhile — and up to the GPU,
//! that last part riding on whichever thread submitted next: the control
//! window's, or an output's. At 60 fps the control window lost 4–6 frames a
//! second and a second output skipped 1–2 — and neither did with macOS
//! capturing the same screen for `QuickTime` instead (a laptop, 2026-10-02).
//! Now the surface is shown to Metal as a texture and copied on the GPU, in a
//! command buffer of the engine's own; nothing is read on the CPU.

#![allow(unsafe_code)]

use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetIOSurface, CVPixelBufferGetWidth,
};
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureType,
    MTLTextureUsage,
};

use crate::renderer::context::GpuContext;

/// A captured frame in the surface macOS captured it into: held until the
/// GPU has copied it, then given back for the next frame.
pub struct SurfaceFrame {
    buffer: CFRetained<CVPixelBuffer>,
}

// SAFETY: a pixel buffer and its surface may be retained, read and released
// from any thread; nothing here writes to it.
unsafe impl Send for SurfaceFrame {}
// SAFETY: as above; shared references only read its size and surface.
unsafe impl Sync for SurfaceFrame {}

impl SurfaceFrame {
    /// The frame `buffer` holds, if a GPU surface backs it.
    pub fn new(buffer: CFRetained<CVPixelBuffer>) -> Option<Self> {
        CVPixelBufferGetIOSurface(Some(&buffer))?;
        Some(Self { buffer })
    }

    /// Its size in pixels.
    pub fn size(&self) -> (u32, u32) {
        let width = CVPixelBufferGetWidth(&self.buffer);
        let height = CVPixelBufferGetHeight(&self.buffer);
        (
            u32::try_from(width).unwrap_or(0),
            u32::try_from(height).unwrap_or(0),
        )
    }
}

/// Copy `frame` — BGRA, in sRGB — into `target`, a `Bgra8UnormSrgb` texture
/// of its size, on the GPU. The surface is given back once the GPU has read
/// it: wgpu lets the texture showing it to Metal go only then.
///
/// # Errors
///
/// When the frame has no surface after all, or Metal cannot show it as a
/// texture; nothing is copied then.
pub fn copy_into(
    context: &GpuContext,
    frame: SurfaceFrame,
    target: &wgpu::Texture,
) -> Result<(), String> {
    let (width, height) = frame.size();
    if (width, height) != (target.width(), target.height()) || width == 0 || height == 0 {
        return Err(format!(
            "frame {width}×{height} for a {}×{} texture",
            target.width(),
            target.height()
        ));
    }
    let surface = CVPixelBufferGetIOSurface(Some(&frame.buffer)).ok_or("no surface")?;
    // SAFETY: only reads the device's Metal device.
    let raw = unsafe {
        context
            .device
            .as_hal::<wgpu::hal::api::Metal>()
            .map(|device| device.raw_device().clone())
    }
    .ok_or("not a Metal device")?;
    // SAFETY: a plain 2D descriptor of the frame's size.
    let descriptor = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm_sRGB,
            width as usize,
            height as usize,
            false,
        )
    };
    descriptor.setUsage(MTLTextureUsage::ShaderRead);
    descriptor.setStorageMode(MTLStorageMode::Shared);
    let texture = raw
        .newTextureWithDescriptor_iosurface_plane(&descriptor, &surface, 0)
        .ok_or("Metal could not show the surface as a texture")?;
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    // SAFETY: the texture is the frame's surface as Metal sees it, described
    // as it is; the frame stays held until wgpu drops it, after the GPU's
    // last use (the drop callback).
    let shown = unsafe {
        let hal = wgpu::hal::metal::Device::texture_from_raw(
            texture,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            MTLTextureType::Type2D,
            1,
            1,
            wgpu::hal::CopyExtent {
                width,
                height,
                depth: 1,
            },
            Some(Box::new(move || drop(frame))),
        );
        context
            .device
            .create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal,
                &wgpu::TextureDescriptor {
                    label: Some("Screen Capture Surface"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    usage: wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                },
                wgpu::TextureUses::COPY_SRC,
            )
    };
    let mut encoder = context
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Screen Capture Copy"),
        });
    encoder.copy_texture_to_texture(shown.as_image_copy(), target.as_image_copy(), size);
    context.submit([encoder.finish()]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFString, CFType};
    use objc2_core_video::{
        kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
        kCVPixelFormatType_32BGRA, kCVReturnSuccess, CVPixelBufferCreate,
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    use std::ptr::NonNull;

    /// A surface-backed pixel buffer of `width`×`height`, each pixel's BGRA
    /// bytes made by `pixel(x, y)`.
    fn surface_buffer(
        width: usize,
        height: usize,
        pixel: impl Fn(usize, usize) -> [u8; 4],
    ) -> CFRetained<CVPixelBuffer> {
        let no_properties = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        // SAFETY: CoreVideo's own keys, with values of the types they take.
        let keys: [&CFString; 2] = unsafe {
            [
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferMetalCompatibilityKey,
            ]
        };
        let values: [&CFType; 2] = [no_properties.as_ref(), CFBoolean::new(true).as_ref()];
        let attributes = CFDictionary::from_slices(&keys, &values);
        let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: `out` is a valid place for the answer; the attributes are
        // a dictionary of the kind CoreVideo takes.
        let made = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                kCVPixelFormatType_32BGRA,
                Some(attributes.as_opaque()),
                NonNull::from(&mut out),
            )
        };
        assert_eq!(made, kCVReturnSuccess, "pixel buffer made");
        // SAFETY: CoreVideo returned it retained (a Create call).
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(out).expect("a buffer")) };
        let lock = CVPixelBufferLockFlags(0);
        // SAFETY: locked for writing below, unlocked after.
        assert_eq!(unsafe { CVPixelBufferLockBaseAddress(&buffer, lock) }, 0);
        let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRow(&buffer);
        for y in 0..height {
            for x in 0..width {
                let bytes = pixel(x, y);
                // SAFETY: within the locked buffer's rows.
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(y * stride + x * 4), 4);
                }
            }
        }
        // SAFETY: locked above.
        unsafe { CVPixelBufferUnlockBaseAddress(&buffer, lock) };
        buffer
    }

    fn texture(context: &GpuContext, width: u32, height: u32) -> wgpu::Texture {
        context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    /// Read `texture` (BGRA, 4 bytes a pixel) back to memory.
    fn read_back(context: &GpuContext, texture: &wgpu::Texture) -> Vec<u8> {
        let (width, height) = (texture.width(), texture.height());
        let row = (width * 4).div_ceil(256) * 256;
        let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read back"),
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            texture.size(),
        );
        context.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = context.device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = buffer.slice(..).get_mapped_range().expect("mapped");
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let start = (y * row) as usize;
            pixels.extend_from_slice(&mapped[start..start + (width * 4) as usize]);
        }
        pixels
    }

    /// A frame in a GPU surface reaches the capture's texture exactly as
    /// captured — every pixel, rows padded or not — copied on the GPU, and
    /// the surface is given back once the GPU is done with it.
    #[test]
    fn a_captured_surface_is_copied_into_the_texture_on_the_gpu_and_given_back() {
        let Ok(context) = GpuContext::new_headless() else {
            return;
        };
        // An odd width, so the surface's rows are padded past width × 4.
        let (width, height) = (333_usize, 77_usize);
        let pixel = |x: usize, y: usize| [(x % 256) as u8, (y * 3 % 256) as u8, (x ^ y) as u8, 255];
        let buffer = surface_buffer(width, height, pixel);
        let held = buffer.retain_count();
        let frame = SurfaceFrame::new(buffer.clone()).expect("a surface backs it");
        assert_eq!(frame.size(), (333, 77));
        let target = texture(&context, 333, 77);

        copy_into(&context, frame, &target).expect("copied");
        let pixels = read_back(&context, &target);
        for y in 0..height {
            for x in 0..width {
                let at = (y * width + x) * 4;
                assert_eq!(&pixels[at..at + 4], &pixel(x, y), "pixel ({x}, {y})");
            }
        }
        let _ = context.device.poll(wgpu::PollType::wait_indefinitely());
        assert_eq!(buffer.retain_count(), held, "given back once the GPU was done");
    }

    /// A frame of another size than the texture is not copied.
    #[test]
    fn a_frame_of_another_size_is_refused() {
        let Ok(context) = GpuContext::new_headless() else {
            return;
        };
        let frame = SurfaceFrame::new(surface_buffer(64, 32, |_, _| [0, 0, 0, 255])).expect("surface");
        let target = texture(&context, 32, 32);
        assert!(copy_into(&context, frame, &target).is_err());
    }
}
