# coreaudio-rs 0.14.0, patched for Nida

Taken unchanged from crates.io (coreaudio-rs 0.14.0, the version cpal 0.17
uses), with one change in `src/audio_unit/render_callback.rs`
(`set_input_callback`), marked `NIDA PATCH`:

- The input callback no longer calls `AudioUnitGetProperty` when the
  device's buffer size changes. That call takes Core Audio's recursive lock
  from inside the IO thread, which holds the device's IO lock; a thread
  starting another stream on the same device (`AudioOutputUnitStart`) holds
  the first and waits for the second, and both hang for good. Nida's live
  bus changes the device's buffer size (64 frames) as it opens, and a Mac
  Studio test (2026-10-31, kit pres-733a6f31) hung the Scarlett until
  restart once in about twenty buffer changes. The format is the one read
  when the callback was set up: the client format (the output scope of the
  input element), which cpal set and which the unit converts to whatever
  the hardware does — another app changing the device's sample rate does
  not change it. (Following such a rate change is not handled by cpal
  0.17 either way: it keeps no rate listener once the stream is made.)
- Its buffer is made once with room for 8192 frames and never reallocated
  in the callback (it was resized there); a larger callback is refused with
  kAudioUnitErr_TooManyFramesToProcess. It is freed whole, as made (the
  original freed it with its length as its capacity).
