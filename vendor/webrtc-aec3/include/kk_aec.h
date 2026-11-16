/*
 * kk_aec.h - minimal C API over WebRTC AudioProcessing (AEC3) from
 * webrtc-audio-processing 2.1 (BSD-3-Clause). Pure C, so Swift can import it
 * through a module map or bridging header without seeing any C++ or abseil.
 *
 * Signal flow per 10 ms step:
 *   kk_aec_process_render(far)            reference = the call audio tapped from the app
 *   kk_aec_process_capture(near, out)     mic in, echo-cancelled mic out
 * The reference must lead the echo it causes (render first, and the echo may
 * appear in the mic up to ~0.5 s later). AEC3 estimates that delay itself.
 *
 * Audio: mono, deinterleaved float in [-1, 1], exactly 10 ms per call
 * (rate / 100 samples). Capture, render and output rates may differ
 * (e.g. 48 kHz mic + 48 kHz reference -> 16 kHz output for ASR).
 *
 * Threading: one instance per call/session. Render and capture may be fed from
 * different threads (APM locks internally), but the simplest correct setup is
 * one serial worker that alternates render/capture 10 ms frames.
 */
#ifndef KK_AEC_H
#define KK_AEC_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct kk_aec kk_aec;

typedef struct kk_aec_config {
  int capture_rate_hz;   /* mic rate: 16000 or 48000 typical (8000..384000 accepted)  */
  int render_rate_hz;    /* reference (far-end) rate                                   */
  int output_rate_hz;    /* rate of the cleaned output; 0 = same as capture            */
  int noise_suppression; /* -1 = off (default), 0..3 = low, moderate, high, very high  */
  int agc;               /* 0 = off (default), 1 = AGC2 adaptive digital gain          */
  int export_linear;     /* 1 = keep AEC3's linear-filter output (16 kHz) per frame    */
  int stream_delay_ms;   /* <0 = never set (default: AEC3 estimates the delay itself)  */
  /* AEC3 tuning; 0 = library default (the config Chrome ships). */
  int   filter_blocks;   /* linear filter length in 4 ms blocks (default 13 = 52 ms)   */
  float erle_max_lf;     /* max ERLE (power ratio) the suppressor credits the linear
                            filter with below 4 kHz (default 4 = 6 dB)                */
  float erle_max_hf;     /* same above 4 kHz (default 1.5 = 1.8 dB)                    */
} kk_aec_config;

typedef struct kk_aec_stats {
  int has_delay;   int    delay_ms;   /* AEC3 render-buffer alignment, 4 ms steps     */
  int has_erl;     double erl_db;     /* AEC3's own estimate of echo return loss      */
  int has_erle;    double erle_db;    /* AEC3's own estimate of ERLE                  */
  int has_divergent; double divergent_filter_fraction;
} kk_aec_stats;

/* Fills defaults: 16 kHz everywhere, AEC3 on, HPF on, NS/AGC off, no delay hint. */
void kk_aec_config_default(kk_aec_config* cfg);

/* Returns NULL on invalid config. */
kk_aec* kk_aec_create(const kk_aec_config* cfg);
void kk_aec_destroy(kk_aec* aec);

/* Samples per 10 ms frame for each stream. */
int kk_aec_render_frame_samples(const kk_aec* aec);
int kk_aec_capture_frame_samples(const kk_aec* aec);
int kk_aec_output_frame_samples(const kk_aec* aec);

/* Feed 10 ms of reference audio. Returns 0 on success, <0 = APM error code. */
int kk_aec_process_render(kk_aec* aec, const float* far_frame);

/* Process 10 ms of mic audio; writes kk_aec_output_frame_samples() samples to
 * out_frame (may alias near_frame only when output rate == capture rate).
 * Returns 0 on success, <0 = APM error code. */
int kk_aec_process_capture(kk_aec* aec, const float* near_frame, float* out_frame);

/* After kk_aec_process_capture: copies 160 samples (10 ms @ 16 kHz) of the
 * linear AEC output (before residual-echo suppression). Returns 1 if written. */
int kk_aec_linear_output(const kk_aec* aec, float* out160);

/* AEC3 metrics as of the last kk_aec_process_capture call (cached by the wrapper). */
int kk_aec_get_stats(kk_aec* aec, kk_aec_stats* stats);

/* Clears all adaptive state (new call). */
int kk_aec_reset(kk_aec* aec);

const char* kk_aec_version(void);

#ifdef __cplusplus
}
#endif
#endif /* KK_AEC_H */
