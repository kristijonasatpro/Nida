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
        if (index == 1 && fail_frame) return E_FAIL;
        ++frames[index].refs; *out = &frames[index]; return S_OK;
    }
    HRESULT DisplayVideoFrameSync(IDeckLinkVideoFrame*) override {
        ++shown; return fail_show ? E_FAIL : S_OK;
    }
    HRESULT GetHardwareReferenceClock(BMDTimeScale, BMDTimeValue*, BMDTimeValue*, BMDTimeValue*) override {
        return E_FAIL;
    }
    HRESULT ScheduleVideoFrame(IDeckLinkVideoFrame*, BMDTimeValue time, BMDTimeValue, BMDTimeScale) override {
        video_times.push_back(time); return S_OK;
    }
    HRESULT EnableAudioOutput(int rate, int type, unsigned int channels, int stream) override {
        assert(rate == 48000 && type == 32 && channels == 2 && stream == bmdAudioOutputStreamTimestamped);
        return fail_audio ? E_FAIL : S_OK;
    }
    HRESULT DisableAudioOutput() override { ++audio_disabled; return S_OK; }
    HRESULT SetScheduledFrameCompletionCallback(IDeckLinkVideoOutputCallback* callback) override {
        if (callback) callback->AddRef();
        if (video_callback) video_callback->Release();
        video_callback = callback; return S_OK;
    }
    HRESULT SetAudioCallback(IDeckLinkAudioOutputCallback* callback) override {
        if (callback) callback->AddRef();
        if (audio_callback) audio_callback->Release();
        audio_callback = callback; return S_OK;
    }
    HRESULT BeginAudioPreroll() override { return audio_callback->RenderAudioSamples(true); }
    HRESULT EndAudioPreroll() override { return S_OK; }
    HRESULT GetBufferedAudioSampleFrameCount(unsigned int* frames) override { *frames = buffered_audio; return S_OK; }
    HRESULT ScheduleAudioSamples(void* data, unsigned int frames, BMDTimeValue time, BMDTimeScale scale, unsigned int* written) override {
        assert(scale == 48000);
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

int main(int argc, char**) {
    if (argc > 1) {
        char version[64];
        assert(nida_dl_device_count() == 0);
        assert(nida_dl_api_version(version, sizeof(version)) == -1);
        assert(version[0] == 0);
        return 0;
    }
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
    assert(handle != nullptr && nida_dl_audio_preroll(handle) == 2403);
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
}
