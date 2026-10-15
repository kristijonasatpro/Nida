# wgpu-hal 30.0.0, patched for Nida

Taken unchanged from crates.io (wgpu-hal 30.0.0), with these changes in
`src/metal/`:

- A surface can be told to hold each frame on screen for at least a set
  time — Metal's `presentDrawable:afterMinimumDuration:`, which wgpu does not
  expose. On a ProMotion (variable refresh) screen that is Apple's way to get
  an even frame rate: each drawable stays up exactly as long, whatever the
  refresh signal does. See `Surface::set_minimum_present_duration`.
- Every present asks Metal when the frame actually reached the screen
  (`addPresentedHandler` / `presentedTime`, 0 for a frame never shown), kept
  per surface until read. That is the truth about whether an output moved
  evenly, which no CPU-side timing can tell. See
  `Surface::take_presented_times`.
- Every command buffer submitted asks Metal when it ran on the GPU
  (`addCompletedHandler` / `GPUStartTime`, `GPUEndTime`), kept under the
  label it had at submit — read first, as the last buffer of a submit is
  relabelled for the fence signal — until read. wgpu records each render
  pass in a command buffer of its own named after the pass, so this is GPU
  time by pass where Apple GPUs give no timestamps inside passes. See
  `Queue::take_gpu_times`.
- Surfaces may be drawn from a thread other than the main one (Nida's
  engine thread): the occlusion check asks the window only on the main
  thread, or goes by what the app's main thread told it
  (`Surface::set_occluded`); `nextDrawable` may time out (after a second)
  rather than wait forever; and layer settings changed off the main thread
  are committed at once (`CATransaction flush`).

Used through `[patch.crates-io]` in Nida's Cargo.toml. To update wgpu, copy
the new wgpu-hal release here and carry the changes across (search for
`Nida`).
