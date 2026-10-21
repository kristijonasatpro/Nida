// Audio-enabled output: video and 48 kHz stereo PCM share the hardware's
// scheduled playback clock. The video-only DisplayVideoFrameSync path stays
// independent. SDK callbacks retain this object through normal COM references.
#pragma once
#include <array>
#include <atomic>
#include <chrono>

using AudioRead = void (*)(void*, int64_t, uint32_t, int32_t*);

class Scheduled final : public IDeckLinkVideoOutputCallback, public IDeckLinkAudioOutputCallback {
    std::atomic<ULONG> refs_{1};
    std::recursive_mutex lock_;
    IDeckLinkOutput* output_;
    static constexpr size_t video_queue_frames = 3;
    std::array<IDeckLinkMutableVideoFrame*, video_queue_frames> frames_{};
    const int32_t width_, height_;
    const BMDTimeValue duration_;
    const BMDTimeScale scale_;
    AudioRead read_;
    void* context_;
    std::vector<uint8_t> latest_;
    std::array<int32_t, 4096> pcm_{};
    uint32_t pending_ = 0, offset_ = 0;
    int64_t audio_time_ = 0, video_time_ = 0;
    bool stopping_ = false, primed_ = false, running_ = false;
    std::atomic<int32_t> error_{0};
    using Clock = std::chrono::steady_clock;
    Clock::time_point primed_at_ = Clock::now(), last_video_ = primed_at_, last_audio_ = primed_at_;

    HRESULT picture(IDeckLinkVideoFrame* frame) {
        void* bytes = nullptr;
        const int64_t row = frame->GetRowBytes();
        if (frame->GetBytes(&bytes) != S_OK || bytes == nullptr || row < width_ * 2) return E_FAIL;
        for (int32_t y = 0; y < height_; ++y) {
            std::memcpy(static_cast<uint8_t*>(bytes) + static_cast<size_t>(y) * row,
                latest_.data() + static_cast<size_t>(y) * width_ * 2, static_cast<size_t>(width_) * 2);
        }
        const HRESULT result = output_->ScheduleVideoFrame(frame, video_time_, duration_, scale_);
        if (result == S_OK) video_time_ += duration_;
        else error_ = -4;
        return result;
    }

public:
    // At completion one frame period has elapsed. A fresh source picture is
    // scheduled after the remaining queue: (queue size - 1) frame periods.
    // Audio must use that same source delay AND no more buffered headroom:
    // filling three frames while delaying by two asks a live tap for future
    // sound, which it replaces with silence. Round this boundary once at 48 kHz,
    // never a per-video-frame sample count (fractional rates would drift).
    const uint32_t source_delay;
    const uint32_t preroll;

    Scheduled(IDeckLinkOutput* output, int32_t width, int32_t height,
              BMDTimeValue duration, BMDTimeScale scale, AudioRead read, void* context)
        : output_(output), width_(width), height_(height), duration_(duration), scale_(scale),
          read_(read), context_(context), latest_(static_cast<size_t>(width) * height * 2),
          source_delay(static_cast<uint32_t>((48000 * duration * (video_queue_frames - 1) + scale - 1) / scale)),
          preroll(std::min<uint32_t>(24000, source_delay)) {
        output_->AddRef();
    }

    ~Scheduled() {
        for (auto* frame : frames_) if (frame != nullptr) frame->Release();
        output_->Release();
    }

    bool enable() {
        for (auto& frame : frames_) {
            if (output_->CreateVideoFrame(width_, height_, width_ * 2, bmdFormat8BitYUV,
                    bmdFrameFlagDefault, &frame) != S_OK || frame == nullptr) return false;
        }
        return output_->EnableAudioOutput(bmdAudioSampleRate48kHz, bmdAudioSampleType32bitInteger,
                   2, bmdAudioOutputStreamTimestamped) == S_OK &&
               output_->SetScheduledFrameCompletionCallback(this) == S_OK &&
               output_->SetAudioCallback(this) == S_OK;
    }

    int32_t submit(const uint8_t* data, int32_t row_bytes) {
        bool begin = false;
        {
            std::lock_guard<std::recursive_mutex> held(lock_);
            if (stopping_ || error_ != 0) return -4;
            const auto now = Clock::now();
            if ((primed_ && !running_ && now - primed_at_ > std::chrono::seconds(3)) ||
                (running_ && (now - last_video_ > std::chrono::seconds(2) ||
                              now - last_audio_ > std::chrono::seconds(2)))) {
                error_ = -4;
                return -4;
            }
            for (int32_t y = 0; y < height_; ++y) {
                std::memcpy(latest_.data() + static_cast<size_t>(y) * width_ * 2,
                    data + static_cast<size_t>(y) * row_bytes, static_cast<size_t>(width_) * 2);
            }
            if (!primed_) {
                primed_ = true;
                primed_at_ = now;
                for (auto* frame : frames_) if (picture(frame) != S_OK) return -4;
                begin = true;
            }
        }
        // BeginAudioPreroll may call the audio callback synchronously.
        if (begin && output_->BeginAudioPreroll() != S_OK) error_ = -4;
        return error_;
    }

    HRESULT NIDA_COM ScheduledFrameCompleted(IDeckLinkVideoFrame* frame,
                                             BMDOutputFrameCompletionResult result) override {
        std::lock_guard<std::recursive_mutex> held(lock_);
        if (stopping_ || error_ != 0) return S_OK;
        if (result == bmdOutputFrameFlushed) return S_OK;
        last_video_ = Clock::now();
        BMDTimeValue now = 0;
        double speed = 0;
        if (output_->GetScheduledStreamTime(scale_, &now, &speed) == S_OK && now >= video_time_)
            video_time_ = (now / duration_ + 1) * duration_;
        return picture(frame);
    }

    HRESULT NIDA_COM ScheduledPlaybackHasStopped() override {
        std::lock_guard<std::recursive_mutex> held(lock_);
        if (!stopping_) error_ = -4;
        return S_OK;
    }

    HRESULT NIDA_COM RenderAudioSamples(DlSupported is_preroll) override {
        {
            std::lock_guard<std::recursive_mutex> held(lock_);
            if (stopping_ || !primed_ || error_ != 0) return S_OK;
            last_audio_ = Clock::now();
            BMDTimeValue now = 0;
            double speed = 0;
            if (!is_preroll && output_->GetScheduledStreamTime(48000, &now, &speed) == S_OK && now > audio_time_) {
                // A stalled callback skips elapsed time rather than playing a
                // backlog late. The Rust source drops to this same timestamp.
                audio_time_ = now;
                pending_ = offset_ = 0;
            }
            uint32_t buffered = 0;
            if (output_->GetBufferedAudioSampleFrameCount(&buffered) != S_OK) {
                error_ = -4; return E_FAIL;
            }
            // Finite work and fixed scratch storage even if a device accepts
            // nothing, reports a huge queue, or makes only partial writes.
            for (int attempt = 0; buffered < preroll && attempt < 16; ++attempt) {
                if (pending_ == 0) {
                    pending_ = std::min<uint32_t>(2048, preroll - buffered);
                    offset_ = 0;
                    read_(context_, audio_time_, pending_, pcm_.data());
                }
                uint32_t written = 0;
                const HRESULT result = output_->ScheduleAudioSamples(pcm_.data() + offset_ * 2,
                    pending_, audio_time_, 48000, &written);
                if (result != S_OK || written > pending_) { error_ = -4; return E_FAIL; }
                audio_time_ += written;
                offset_ += written;
                pending_ -= written;
                buffered += written;
                if (written == 0) break;
            }
            if (is_preroll && buffered >= preroll && !running_) {
                running_ = true;
                last_video_ = last_audio_ = Clock::now();
                // Serialized with stop, including the start calls. A recursive
                // lock permits an SDK that invokes another callback inline.
                if (output_->EndAudioPreroll() != S_OK ||
                    output_->StartScheduledPlayback(0, scale_, 1.0) != S_OK) {
                    error_ = -4; return E_FAIL;
                }
            }
        }
        return S_OK;
    }

    void stop() {
        {
            std::lock_guard<std::recursive_mutex> held(lock_);
            stopping_ = true;
        }
        // Do not hold the callback lock while asking the SDK to stop/flush.
        output_->StopScheduledPlayback(0, nullptr, scale_);
        output_->SetScheduledFrameCompletionCallback(nullptr);
        output_->SetAudioCallback(nullptr);
        // No callback can read the Rust audio context after this barrier:
        // one already running finishes, and late callbacks see stopping_.
        { std::lock_guard<std::recursive_mutex> held(lock_); }
        output_->DisableAudioOutput();
    }

    HRESULT NIDA_COM QueryInterface(REFIID iid, LPVOID* out) override {
        if (out == nullptr) return E_POINTER;
        *out = nullptr;
        if (dl_is_unknown(iid) || dl_same_iid(iid, IID_IDeckLinkVideoOutputCallback))
            *out = static_cast<IDeckLinkVideoOutputCallback*>(this);
        else if (dl_same_iid(iid, IID_IDeckLinkAudioOutputCallback))
            *out = static_cast<IDeckLinkAudioOutputCallback*>(this);
        else return E_NOINTERFACE;
        AddRef();
        return S_OK;
    }

    ULONG NIDA_COM AddRef() override { return ++refs_; }
    ULONG NIDA_COM Release() override {
        const ULONG left = --refs_;
        if (left == 0) delete this;
        return left;
    }
};
