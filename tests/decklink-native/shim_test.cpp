// Compile and run without Desktop Video or a GPU. The real shim is exercised;
// only SDK/platform calls are replaced. Native Windows compilation must also
// be checked against the MIDL-generated interfaces, not these doubles.
#define NIDA_DECKLINK_PLATFORM_HEADER "fake_decklink.hpp"
#include <thread>
#include "../../src/internal/decklink/shim.cpp"

struct ModeDouble : IDeckLinkDisplayMode {
    int refs = 0;
    BMDDisplayMode code = 1;
    int64_t duration = 1, scale = 60;
    BMDDisplayMode GetDisplayMode() override { return code; }
    long GetWidth() override { return 4; }
    long GetHeight() override { return 2; }
    HRESULT GetName(DlString* name) override { *name = "tiny p60"; return S_OK; }
    HRESULT GetFrameRate(BMDTimeValue* duration, BMDTimeScale* scale) override {
        *duration = this->duration; *scale = this->scale; return S_OK;
    }
    ULONG Release() override { assert(refs > 0); return --refs; }
};

struct IteratorDouble : IDeckLinkDisplayModeIterator {
    ModeDouble mode;
    int next = 0;
    int refs = 0;
    HRESULT Next(IDeckLinkDisplayMode** out) override {
        if (next == 2) { *out = nullptr; return E_FAIL; }
        mode.code = static_cast<uint32_t>(++next);
        ++mode.refs; *out = &mode; return S_OK;
    }
    ULONG Release() override { assert(mode.refs == 0); return --refs; }
};

struct FrameDouble : IDeckLinkMutableVideoFrame {
    int refs = 0;
    // Padding on each row must remain untouched by frame delivery.
    std::vector<uint8_t> bytes = std::vector<uint8_t>(24, 0xee);
    HRESULT GetBytes(void** out) override { *out = bytes.data(); return S_OK; }
    long GetRowBytes() override { return 12; }
    ULONG Release() override { assert(refs > 0); return --refs; }
};

struct OutputDouble : IDeckLinkOutput {
    enum class Failure { None, Frames, Audio, VideoCallback, AudioCallback, Begin, End, Start, VideoWrite, AudioWrite, AudioCount };
    Failure failure = Failure::None;
    IteratorDouble iterator;
    FrameDouble frames[3];
    int refs = 0, created = 0, disabled = 0, shown = 0;
    bool fail_enable = false, fail_frame = false, fail_show = false;
    bool fail_audio = false;
    IDeckLinkVideoOutputCallback* video_callback = nullptr;
    IDeckLinkAudioOutputCallback* audio_callback = nullptr;
    unsigned int buffered_audio = 0, write_limit = 500;
    int audio_disabled = 0, playback_started = 0, playback_stopped = 0;
    int64_t stream_frame = -1;
    HRESULT GetScheduledStreamTime(BMDTimeScale scale, BMDTimeValue* now, double* speed) override {
        if (stream_frame < 0) return E_FAIL;
        *now = stream_frame * scale / 48000;
        *speed = 1.0;
        return S_OK;
    }
    std::vector<int64_t> video_times, audio_times;
    std::vector<int32_t> audio_samples;
    HRESULT DoesSupportVideoMode(int, BMDDisplayMode mode, int, int, int,
                                 BMDDisplayMode*, DlSupported* supported) override {
        *supported = mode == 1; return S_OK;
    }
    HRESULT GetDisplayModeIterator(IDeckLinkDisplayModeIterator** out) override {
        iterator.next = 0; ++iterator.refs; *out = &iterator; return S_OK;
    }
    HRESULT GetDisplayMode(BMDDisplayMode, IDeckLinkDisplayMode** out) override {
        ++iterator.mode.refs; *out = &iterator.mode; return S_OK;
    }
    HRESULT EnableVideoOutput(BMDDisplayMode, int) override { created = 0; return fail_enable ? E_FAIL : S_OK; }
    HRESULT DisableVideoOutput() override { ++disabled; return S_OK; }
    HRESULT CreateVideoFrame(int, int, int, int, int, IDeckLinkMutableVideoFrame** out) override {
        const int index = created++ % 3;
        if (index == 1 && (fail_frame || failure == Failure::Frames)) return E_FAIL;
        ++frames[index].refs; *out = &frames[index]; return S_OK;
    }
    HRESULT DisplayVideoFrameSync(IDeckLinkVideoFrame*) override {
        ++shown; return fail_show ? E_FAIL : S_OK;
    }
    HRESULT GetHardwareReferenceClock(BMDTimeScale, BMDTimeValue*, BMDTimeValue*, BMDTimeValue*) override {
        return E_FAIL;
    }
    HRESULT ScheduleVideoFrame(IDeckLinkVideoFrame*, BMDTimeValue time, BMDTimeValue, BMDTimeScale) override {
        if (failure == Failure::VideoWrite) return E_FAIL;
        video_times.push_back(time); return S_OK;
    }
    HRESULT EnableAudioOutput(int rate, int type, unsigned int channels, int stream) override {
        assert(rate == 48000 && type == 32 && channels == 2 && stream == bmdAudioOutputStreamTimestamped);
        buffered_audio = 0;
        return fail_audio || failure == Failure::Audio ? E_FAIL : S_OK;
    }
    HRESULT DisableAudioOutput() override { ++audio_disabled; return S_OK; }
    HRESULT SetScheduledFrameCompletionCallback(IDeckLinkVideoOutputCallback* callback) override {
        if (callback && failure == Failure::VideoCallback) return E_FAIL;
        if (callback) callback->AddRef();
        if (video_callback) video_callback->Release();
        video_callback = callback; return S_OK;
    }
    HRESULT SetAudioCallback(IDeckLinkAudioOutputCallback* callback) override {
        if (callback && failure == Failure::AudioCallback) return E_FAIL;
        if (callback) callback->AddRef();
        if (audio_callback) audio_callback->Release();
        audio_callback = callback; return S_OK;
    }
    HRESULT BeginAudioPreroll() override { return failure == Failure::Begin ? E_FAIL : audio_callback->RenderAudioSamples(true); }
    HRESULT EndAudioPreroll() override { return failure == Failure::End ? E_FAIL : S_OK; }
    HRESULT GetBufferedAudioSampleFrameCount(unsigned int* frames) override {
        if (failure == Failure::AudioCount) return E_FAIL;
        *frames = buffered_audio; return S_OK;
    }
    HRESULT ScheduleAudioSamples(void* data, unsigned int frames, BMDTimeValue time, BMDTimeScale scale, unsigned int* written) override {
        assert(scale == 48000);
        if (failure == Failure::AudioWrite) return E_FAIL;
        *written = std::min(frames, write_limit);
        if (*written > 0) {
            audio_times.push_back(time);
            auto* samples = static_cast<int32_t*>(data);
            audio_samples.insert(audio_samples.end(), samples, samples + *written * 2);
            buffered_audio += *written;
        }
        return S_OK;
    }
    HRESULT StartScheduledPlayback(BMDTimeValue time, BMDTimeScale, double speed) override {
        if (failure == Failure::Start) return E_FAIL;
        assert(time == 0 && speed == 1.0); ++playback_started; return S_OK;
    }
    HRESULT StopScheduledPlayback(BMDTimeValue, BMDTimeValue*, BMDTimeScale) override {
        ++playback_stopped;
        if (video_callback) video_callback->ScheduledPlaybackHasStopped();
        return S_OK;
    }
    ULONG AddRef() override { return ++refs; }
    ULONG Release() override { assert(refs > 0); return --refs; }
};

struct DeviceDouble : IDeckLink {
    OutputDouble output;
    int refs = 0;
    bool input_only = false;
    HRESULT GetDisplayName(DlString* out) override { *out = "Test DeckLink"; return S_OK; }
    HRESULT QueryInterface(REFIID iid, LPVOID* out) override {
        *out = nullptr;
        if (iid != IID_IDeckLinkOutput || input_only) return E_NOINTERFACE;
        ++output.refs; *out = &output; return S_OK;
    }
    ULONG AddRef() override { return ++refs; }
    ULONG Release() override { assert(refs > 0); return --refs; }
    void released() const {
        assert(refs == 0 && output.refs == 0);
        for (const auto& frame : output.frames) assert(frame.refs == 0);
    }
};

struct DiscoveryDouble : IDeckLinkDiscovery {
    HRESULT InstallDeviceNotifications(IDeckLinkDeviceNotificationCallback*) override { return S_OK; }
    ULONG Release() override { return 0; }
};

// A tap can supply only sound already produced at the current source time.
// The old always-ready Sound double hid requests one video frame in the future.
struct LiveSound {
    int64_t produced = 0;
    uint32_t delay = 0;
    uint64_t future_reads = 0;
    static void read(void* context, int64_t at, uint32_t frames, int32_t* out) {
        auto& sound = *static_cast<LiveSound*>(context);
        for (uint32_t i = 0; i < frames; ++i) {
            const int64_t source = at + i - sound.delay;
            if (source >= sound.produced) ++sound.future_reads;
            // Silence before the tap starts; a timestamped stereo marker after.
            const auto value = source < 0 ? 0 : static_cast<int32_t>(source + 1);
            out[2 * i] = value;
            out[2 * i + 1] = -value;
        }
    }
};

void live_audio_alignment(int64_t duration, int64_t scale) {
    OutputDouble output;
    output.write_limit = 137; // Retain samples across partial SDK writes.
    LiveSound sound;
    auto* scheduled = new Scheduled(&output, 4, 2, duration, scale, LiveSound::read, &sound);
    sound.delay = scheduled->source_delay;
    assert(scheduled->enable());
    const std::vector<uint8_t> picture(16, 0x81);
    assert(scheduled->submit(picture.data(), 8) == 0);
    // The SDK may ask for more preroll callbacks when writes are partial.
    while (!output.playback_started) assert(output.audio_callback->RenderAudioSamples(true) == S_OK);
    assert(sound.future_reads == 0 && "preroll must not request sound that has not happened");
    assert(output.video_times.size() == 3);
    const int64_t lead_ticks = output.video_times.back();
    assert(sound.delay == (48000 * lead_ticks + scale - 1) / scale);
    // 10,000 frames exposes accumulated fractional-rate rounding drift.
    for (int64_t frame = 1; frame <= 10000; ++frame) {
        const int64_t now = frame * 48000 * duration / scale;
        const int64_t before = (frame - 1) * 48000 * duration / scale;
        sound.produced = now;
        output.stream_frame = now;
        assert(output.buffered_audio >= now - before);
        output.buffered_audio -= static_cast<uint32_t>(now - before);
        // Fresh source picture at this frame boundary is scheduled two whole
        // frame periods ahead, not three: completion consumed the first frame.
        assert(scheduled->submit(picture.data(), 8) == 0);
        assert(output.video_callback->ScheduledFrameCompleted(&output.frames[(frame - 1) % 3], 0) == S_OK);
        assert(output.video_times.back() == (frame + 2) * duration);
        while (output.buffered_audio < scheduled->preroll)
            assert(output.audio_callback->RenderAudioSamples(false) == S_OK);
        assert(sound.future_reads == 0 && "steady playback must not read future source samples");
        assert(static_cast<int64_t>(output.audio_samples.size() / 2) == now + scheduled->preroll);
    }
    for (size_t sample = 0; sample < output.audio_samples.size() / 2; ++sample) {
        const auto expected = sample < sound.delay ? 0 : static_cast<int32_t>(sample - sound.delay + 1);
        assert(output.audio_samples[2 * sample] == expected);
        assert(output.audio_samples[2 * sample + 1] == -expected);
    }
    scheduled->stop();
    scheduled->Release();
    assert(output.refs == 0 && output.audio_callback == nullptr && output.video_callback == nullptr);
    for (const auto& frame : output.frames) assert(frame.refs == 0);
}

void audio_failures_can_restart() {
    using Failure = OutputDouble::Failure;
    for (const auto failure : {Failure::Frames, Failure::Audio, Failure::VideoCallback,
             Failure::AudioCallback, Failure::Begin, Failure::End, Failure::Start,
             Failure::VideoWrite, Failure::AudioWrite, Failure::AudioCount}) {
        DeviceDouble device;
        device.output.failure = failure;
        g_notifications.DeckLinkDeviceArrived(&device);
        uint64_t id = 0;
        char name[256], error[256];
        nida_dl_mode modes[4];
        nida_dl_device_snapshot(0, &id, name, sizeof(name), modes, 4);
        LiveSound sound;
        void* handle = nida_dl_open_with_audio(id, 1, LiveSound::read, &sound, error, sizeof(error));
        const std::vector<uint8_t> data(16, 0x81);
        if (failure == Failure::Frames || failure == Failure::Audio ||
            failure == Failure::VideoCallback || failure == Failure::AudioCallback) {
            assert(handle == nullptr); // Never silently fall back to video-only.
        } else {
            assert(handle != nullptr);
            sound.delay = nida_dl_audio_delay(handle);
            assert(nida_dl_show(handle, data.data(), 8, 4, 2) == -4);
            nida_dl_close(handle);
        }
        assert(device.output.refs == 0);
        assert(device.output.video_callback == nullptr && device.output.audio_callback == nullptr);
        for (const auto& frame : device.output.frames) assert(frame.refs == 0);
        device.output.failure = Failure::None;
        device.output.audio_times.clear();
        device.output.video_times.clear();
        handle = nida_dl_open_with_audio(id, 1, LiveSound::read, &sound, error, sizeof(error));
        assert(handle != nullptr);
        sound.delay = nida_dl_audio_delay(handle);
        assert(nida_dl_show(handle, data.data(), 8, 4, 2) == 0);
        assert(device.output.audio_times.front() == 0);
        assert((device.output.video_times == std::vector<int64_t>{0, 1, 2}));
        assert(sound.future_reads == 0);
        nida_dl_close(handle);
        g_notifications.DeckLinkDeviceRemoved(&device);
        device.released();
    }
}

int main(int argc, char**) {
    if (argc > 1) {
        char version[64];
        assert(nida_dl_device_count() == 0);
        assert(nida_dl_api_version(version, sizeof(version)) == -1);
        assert(version[0] == 0);
        return 0;
    }
    for (const auto& rate : {std::pair<int64_t, int64_t>{1, 24}, {1, 25}, {1, 30}, {1, 50},
                            {1, 60}, {1001, 24000}, {1001, 30000}, {1001, 60000}})
        live_audio_alignment(rate.first, rate.second);
    DiscoveryDouble discovery;
    fake_discovery = &discovery;
    std::vector<std::thread> readers;
    for (int i = 0; i < 8; ++i) readers.emplace_back([] { assert(nida_dl_device_count() == 0); });
    for (auto& thread : readers) thread.join();
    assert(discovery_starts == 1);
    assert(g_notifications.QueryInterface(0, nullptr) == E_POINTER);

    DeviceDouble device;
    DeviceDouble input;
    input.input_only = true;
    g_notifications.DeckLinkDeviceArrived(&device);
    g_notifications.DeckLinkDeviceArrived(&input);
    assert(nida_dl_all_device_count() == 2 && nida_dl_device_count() == 1);
    uint64_t id = 0;
    char name[256];
    nida_dl_mode modes[4];
    assert(nida_dl_device_snapshot(0, &id, name, sizeof(name), modes, 4) == 1);
    assert(id != 0 && std::string(name) == "Test DeckLink" && modes[0].code == 1);
    assert(nida_dl_device_snapshot(1, &id, name, sizeof(name), modes, 4) == -1 && id == 0);
    nida_dl_device_snapshot(0, &id, name, sizeof(name), modes, 4);
    char error[256];
    assert(nida_dl_open(id, 2, error, sizeof(error)) == nullptr);
    assert(device.refs == 1 && device.output.refs == 0);
    fake_ready = false;
    assert(nida_dl_open(id, 1, error, sizeof(error)) == nullptr);
    assert(std::string(error).find("COM apartment") != std::string::npos);
    fake_ready = true;
    device.output.fail_enable = true;
    assert(nida_dl_open(id, 1, error, sizeof(error)) == nullptr);
    assert(device.refs == 1 && device.output.refs == 0);
    device.output.fail_enable = false;
    device.output.fail_frame = true;
    assert(nida_dl_open(id, 1, error, sizeof(error)) == nullptr);
    assert(device.refs == 1 && device.output.refs == 0 && device.output.frames[0].refs == 0);
    assert(device.output.disabled == 1);
    device.output.fail_frame = false;
    void* handle = nida_dl_open(id, 1, error, sizeof(error));
    assert(handle != nullptr);
    const std::vector<uint8_t> data(16, 0x81);
    assert(nida_dl_show(handle, data.data(), 7, 4, 2) == -1);
    assert(nida_dl_show(handle, data.data(), 8, 4, 2) == 0);
    assert(device.output.audio_callback == nullptr && device.output.video_callback == nullptr);
    assert(device.output.playback_started == 0 && "video-only keeps synchronous display");
    const auto& bytes = device.output.frames[0].bytes;
    assert(bytes[0] == 0x81 && bytes[7] == 0x81 && bytes[8] == 0xee);
    assert(bytes[12] == 0x81 && bytes[19] == 0x81 && bytes[20] == 0xee);
    g_notifications.DeckLinkDeviceRemoved(&device);
    assert(nida_dl_device_count() == 0);
    assert(nida_dl_open(id, 1, error, sizeof(error)) == nullptr);
    device.output.fail_show = true;
    assert(nida_dl_show(handle, data.data(), 8, 4, 2) == -3);
    nida_dl_close(handle);
    assert(device.output.disabled == 2);
    device.released();
    g_notifications.DeckLinkDeviceArrived(&device);
    uint64_t new_id = 0;
    assert(nida_dl_device_snapshot(0, &new_id, name, sizeof(name), modes, 4) == 1);
    assert(new_id != id);
    assert(nida_dl_open(id, 1, error, sizeof(error)) == nullptr);
    g_notifications.DeckLinkDeviceRemoved(&device);
    g_notifications.DeckLinkDeviceRemoved(&input);
    device.released(); input.released();

    struct Sound {
        int reads = 0;
        static void read(void* context, int64_t at, uint32_t frames, int32_t* out) {
            ++static_cast<Sound*>(context)->reads;
            for (uint32_t i = 0; i < frames; ++i) {
                out[2 * i] = static_cast<int32_t>(at + i);
                out[2 * i + 1] = -static_cast<int32_t>(at + i);
            }
        }
    } sound;
    DeviceDouble av_device;
    av_device.output.iterator.mode.duration = 1001;
    av_device.output.iterator.mode.scale = 60000; // 59.94, not rounded to 60.
    g_notifications.DeckLinkDeviceArrived(&av_device);
    nida_dl_device_snapshot(0, &id, name, sizeof(name), modes, 4);
    av_device.output.fail_audio = true;
    assert(nida_dl_open_with_audio(id, 1, Sound::read, &sound, error, sizeof(error)) == nullptr);
    assert(av_device.refs == 1 && av_device.output.refs == 0);
    av_device.output.fail_audio = false;
    handle = nida_dl_open_with_audio(id, 1, Sound::read, &sound, error, sizeof(error));
    assert(handle != nullptr && nida_dl_audio_delay(handle) == 1602);
    av_device.output.write_limit = 0;
    assert(nida_dl_show(handle, data.data(), 8, 4, 2) == 0);
    assert(av_device.output.playback_started == 0);
    assert(sound.reads == 1); // Zero writes retain the same scratch samples.
    av_device.output.write_limit = 500;
    assert(av_device.output.audio_callback->RenderAudioSamples(true) == S_OK);
    assert(av_device.output.playback_started == 1);
    assert((av_device.output.video_times == std::vector<int64_t>{0, 1001, 2002}));
    for (int64_t frame = 1; frame <= 120; ++frame) {
        const auto before = (frame - 1) * 48000 * 1001 / 60000;
        const auto after = frame * 48000 * 1001 / 60000;
        av_device.output.buffered_audio -= static_cast<uint32_t>(after - before);
        assert(av_device.output.audio_callback->RenderAudioSamples(false) == S_OK);
        assert(av_device.output.video_callback->ScheduledFrameCompleted(&av_device.output.frames[frame % 3], 0) == S_OK);
    }
    for (size_t i = 0; i < av_device.output.video_times.size(); ++i)
        assert(av_device.output.video_times[i] == static_cast<int64_t>(i) * 1001);
    for (size_t i = 0; i < av_device.output.audio_samples.size() / 2; ++i) {
        assert(av_device.output.audio_samples[2 * i] == static_cast<int32_t>(i));
        assert(av_device.output.audio_samples[2 * i + 1] == -static_cast<int32_t>(i));
    }
    // Recover a late callback against the real stream timestamp instead of
    // replaying old sound or submitting video behind the hardware clock.
    const size_t before_jump = av_device.output.audio_times.size();
    av_device.output.stream_frame = 240000;
    av_device.output.buffered_audio = 0;
    assert(av_device.output.audio_callback->RenderAudioSamples(false) == S_OK);
    assert(av_device.output.audio_times[before_jump] == 240000);
    assert(av_device.output.video_callback->ScheduledFrameCompleted(&av_device.output.frames[0], 0) == S_OK);
    assert(av_device.output.video_times.back() == (300000 / 1001 + 1) * 1001);
    assert(av_device.output.video_callback->ScheduledPlaybackHasStopped() == S_OK);
    assert(nida_dl_show(handle, data.data(), 8, 4, 2) == -4);
    // An SDK may retain a callback while stop unregisters it. It must never
    // dereference the Rust context once close has returned.
    auto* late = av_device.output.audio_callback;
    late->AddRef();
    nida_dl_close(handle);
    const int before_reads = sound.reads;
    late->RenderAudioSamples(false);
    assert(sound.reads == before_reads);
    late->Release();
    assert(av_device.output.audio_callback == nullptr && av_device.output.video_callback == nullptr);
    assert(av_device.output.playback_stopped >= 1);
    g_notifications.DeckLinkDeviceRemoved(&av_device);
    av_device.released();
    audio_failures_can_restart();
}
