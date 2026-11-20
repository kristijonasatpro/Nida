// kk_aec.cc - C wrapper over webrtc::AudioProcessing (AEC3), see kk_aec.h.
#include "kk_aec.h"

#include <array>
#include <cstring>
#include <memory>
#include <new>
#include <optional>

#include "api/audio/audio_processing.h"
#include "api/audio/echo_canceller3_config.h"
#include "api/audio/echo_control.h"
#include "api/scoped_refptr.h"
// Internal header (not installed by webrtc-audio-processing); compile this file with
// the same defines as the library (see build.sh) so the class layout matches.
#include "modules/audio_processing/aec3/echo_canceller3.h"

namespace {
// APM's built-in AEC3 creation always uses a default EchoCanceller3Config, which
// (a) cannot be tuned and (b) crashes when export_linear_aec_output is requested,
// because filter.export_linear_aec_output stays false. Injecting our own factory
// fixes both; with all tuning fields at 0 the config equals the built-in one.
class Aec3Factory : public webrtc::EchoControlFactory {
 public:
  explicit Aec3Factory(const webrtc::EchoCanceller3Config& c) : c_(c) {}
  std::unique_ptr<webrtc::EchoControl> Create(int sample_rate_hz, int num_render_channels,
                                              int num_capture_channels) override {
    return std::make_unique<webrtc::EchoCanceller3>(c_, std::nullopt, sample_rate_hz,
                                                    num_render_channels, num_capture_channels);
  }

 private:
  webrtc::EchoCanceller3Config c_;
};
}  // namespace

struct kk_aec {
  kk_aec_config cfg;
  rtc::scoped_refptr<webrtc::AudioProcessing> apm;
  webrtc::StreamConfig cap_in, cap_out, ren;
  std::array<float, 160> linear{};  // last linear AEC frame (16 kHz)
  bool linear_valid = false;
  // APM passes stats through a 1-slot queue: stats of frames processed while the slot
  // is full are dropped, and GetStatistics() returns the oldest queued entry. Draining
  // it after every capture frame keeps this copy current.
  webrtc::AudioProcessingStats last_stats;
};

static bool valid_rate(int hz) { return hz >= 8000 && hz <= 384000 && hz % 100 == 0; }

void kk_aec_config_default(kk_aec_config* c) {
  if (!c) return;
  c->capture_rate_hz = 16000;
  c->render_rate_hz = 16000;
  c->output_rate_hz = 0;
  c->noise_suppression = -1;
  c->agc = 0;
  c->export_linear = 0;
  c->stream_delay_ms = -1;
  c->filter_blocks = 0;
  c->erle_max_lf = 0.f;
  c->erle_max_hf = 0.f;
}

static webrtc::EchoCanceller3Config make_aec3_config(const kk_aec_config& c) {
  webrtc::EchoCanceller3Config e;  // library defaults
  e.filter.export_linear_aec_output = c.export_linear != 0;
  if (c.filter_blocks > 0) {
    e.filter.refined.length_blocks = size_t(c.filter_blocks);
    e.filter.coarse.length_blocks = size_t(c.filter_blocks);
  }
  if (c.erle_max_lf > 0.f) e.erle.max_l = c.erle_max_lf;
  if (c.erle_max_hf > 0.f) e.erle.max_h = c.erle_max_hf;
  webrtc::EchoCanceller3Config::Validate(&e);
  return e;
}

static webrtc::AudioProcessing::Config make_apm_config(const kk_aec_config& c) {
  webrtc::AudioProcessing::Config ac;
  ac.echo_canceller.enabled = true;        // AEC3 (full, not the mobile AECM)
  ac.echo_canceller.mobile_mode = false;
  ac.echo_canceller.export_linear_aec_output = c.export_linear != 0;
  ac.high_pass_filter.enabled = true;      // AEC3 enforces it anyway
  if (c.noise_suppression >= 0) {
    ac.noise_suppression.enabled = true;
    static const webrtc::AudioProcessing::Config::NoiseSuppression::Level lv[4] = {
        webrtc::AudioProcessing::Config::NoiseSuppression::kLow,
        webrtc::AudioProcessing::Config::NoiseSuppression::kModerate,
        webrtc::AudioProcessing::Config::NoiseSuppression::kHigh,
        webrtc::AudioProcessing::Config::NoiseSuppression::kVeryHigh};
    ac.noise_suppression.level = lv[c.noise_suppression > 3 ? 3 : c.noise_suppression];
  }
  if (c.agc) {
    ac.gain_controller2.enabled = true;
    ac.gain_controller2.adaptive_digital.enabled = true;
  }
  return ac;
}

kk_aec* kk_aec_create(const kk_aec_config* cfg) {
  kk_aec_config c;
  kk_aec_config_default(&c);
  if (cfg) c = *cfg;
  if (c.output_rate_hz == 0) c.output_rate_hz = c.capture_rate_hz;
  if (!valid_rate(c.capture_rate_hz) || !valid_rate(c.render_rate_hz) ||
      !valid_rate(c.output_rate_hz))
    return nullptr;

  kk_aec* a = new (std::nothrow) kk_aec();
  if (!a) return nullptr;
  a->cfg = c;
#ifndef KK_AEC_BUILTIN_AEC3  // test-only switch: use APM's own AEC3 creation instead
  a->apm = webrtc::AudioProcessingBuilder()
               .SetConfig(make_apm_config(c))
               .SetEchoControlFactory(std::make_unique<Aec3Factory>(make_aec3_config(c)))
               .Create();
#else
  a->apm = webrtc::AudioProcessingBuilder().SetConfig(make_apm_config(c)).Create();
#endif
  if (!a->apm) { delete a; return nullptr; }
  a->cap_in = webrtc::StreamConfig(c.capture_rate_hz, 1);
  a->cap_out = webrtc::StreamConfig(c.output_rate_hz, 1);
  a->ren = webrtc::StreamConfig(c.render_rate_hz, 1);
  webrtc::ProcessingConfig pc;
  pc.input_stream() = a->cap_in;
  pc.output_stream() = a->cap_out;
  pc.reverse_input_stream() = a->ren;
  pc.reverse_output_stream() = a->ren;
  if (a->apm->Initialize(pc) != webrtc::AudioProcessing::kNoError) { delete a; return nullptr; }
  return a;
}

void kk_aec_destroy(kk_aec* a) { delete a; }

int kk_aec_render_frame_samples(const kk_aec* a) { return a ? a->cfg.render_rate_hz / 100 : 0; }
int kk_aec_capture_frame_samples(const kk_aec* a) { return a ? a->cfg.capture_rate_hz / 100 : 0; }
int kk_aec_output_frame_samples(const kk_aec* a) { return a ? a->cfg.output_rate_hz / 100 : 0; }

int kk_aec_process_render(kk_aec* a, const float* far_frame) {
  if (!a || !far_frame) return -1;
  const float* ch[1] = {far_frame};
  return a->apm->AnalyzeReverseStream(ch, a->ren);
}

int kk_aec_process_capture(kk_aec* a, const float* near_frame, float* out_frame) {
  if (!a || !near_frame || !out_frame) return -1;
  if (a->cfg.stream_delay_ms >= 0) a->apm->set_stream_delay_ms(a->cfg.stream_delay_ms);
  const float* in[1] = {near_frame};
  float* out[1] = {out_frame};
  int r = a->apm->ProcessStream(in, a->cap_in, a->cap_out, out);
  a->last_stats = a->apm->GetStatistics();
  a->linear_valid = false;
  if (r == webrtc::AudioProcessing::kNoError && a->cfg.export_linear) {
    a->linear_valid = a->apm->GetLinearAecOutput(
        rtc::ArrayView<std::array<float, 160>>(&a->linear, 1));
  }
  return r;
}

int kk_aec_linear_output(const kk_aec* a, float* out160) {
  if (!a || !out160 || !a->linear_valid) return 0;
  // APM already converts it to [-1, 1] (FloatS16ToFloat).
  std::memcpy(out160, a->linear.data(), sizeof(float) * 160);
  return 1;
}

int kk_aec_get_stats(kk_aec* a, kk_aec_stats* s) {
  if (!a || !s) return -1;
  std::memset(s, 0, sizeof(*s));
  const webrtc::AudioProcessingStats& st = a->last_stats;
  if (st.delay_ms) { s->has_delay = 1; s->delay_ms = *st.delay_ms; }
  if (st.echo_return_loss) { s->has_erl = 1; s->erl_db = *st.echo_return_loss; }
  if (st.echo_return_loss_enhancement) { s->has_erle = 1; s->erle_db = *st.echo_return_loss_enhancement; }
  if (st.divergent_filter_fraction) { s->has_divergent = 1; s->divergent_filter_fraction = *st.divergent_filter_fraction; }
  return 0;
}

int kk_aec_reset(kk_aec* a) {
  if (!a) return -1;
  return a->apm->Initialize();
}

const char* kk_aec_version(void) { return "kk_aec 0.1 / webrtc-audio-processing 2.1 (AEC3)"; }
