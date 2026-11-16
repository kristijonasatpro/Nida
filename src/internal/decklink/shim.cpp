// A small C face on Blackmagic's DeckLink API for Nida's Blackmagic outputs
// and inputs (see mod.rs and input.rs). The API is loaded at run time from the DeckLinkAPI framework
// Desktop Video installs (DeckLinkAPIDispatch.cpp): without it there are
// simply no devices.
//
// Devices are known through the API's discovery, which says when one arrives
// and when it goes: each is connected to once, as it arrives, its name and
// formats read then, and kept until it goes — an output opens on that very
// object. They are never looked for again with fresh iterators: doing that
// every few seconds while an output put pictures up crashed Nida within the
// hour (a connection torn down under a dispatch source still watching it —
// libdispatch's "kevent vanished", 2026-10-04 and -05).

#include <Accelerate/Accelerate.h>
#include <CoreFoundation/CFPlugInCOM.h>
#include <CoreFoundation/CoreFoundation.h>
#include <atomic>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <set>
#include <string>
#include <vector>

#include "DeckLinkAPI.h"

extern "C" {

struct nida_dl_mode {
    uint32_t code;
    int32_t width;
    int32_t height;
    int64_t frame_duration;
    int64_t time_scale;
    char name[64];
};

// A format a device can take pictures in.
struct nida_dl_in_mode {
    uint32_t code;
    int32_t width;
    int32_t height;
    int64_t frame_duration;
    int64_t time_scale;
    // Whole frames, not two fields woven.
    int32_t progressive;
    char name[64];
};

// A device that can take pictures in, as discovery has it now.
struct nida_dl_input_info {
    // This arrival of it (see `Known::id`).
    uint64_t id;
    // What tells it from another of its model, kept across arrivals and
    // restarts: Blackmagic's persistent ID; 0 when it has none (its place
    // on the bus changes as others are plugged in, so is not used).
    int64_t persistent;
    // The connections it takes video in on (`BMDVideoConnection` bits).
    int64_t connections;
    // Whether it tells what format comes in, and follows it.
    int32_t detects;
    char name[256];
};

// A picture come in: `data` is `height` rows of `row_bytes`, in
// `pixel_format` ('2vuy' or 'BGRA'); `colorspace` the format's own flags
// (Rec. 601 / 709 / 2020); `no_signal` when nothing is plugged into it;
// `progressive` 0 for two fields woven; one frame lasts `frame_duration` of
// `time_scale`.
typedef void (*nida_dl_video_fn)(void* ctx, const uint8_t* data, int32_t row_bytes, int32_t width,
                                 int32_t height, uint32_t pixel_format, uint32_t colorspace, int32_t no_signal,
                                 int32_t progressive, int64_t frame_duration, int64_t time_scale);
// Sound come in: `frames` frames of `channels` interleaved 32-bit samples.
typedef void (*nida_dl_audio_fn)(void* ctx, const int32_t* samples, int32_t frames, int32_t channels);

}  // extern "C"

namespace {

void copy_cfstring(CFStringRef text, char* buf, int32_t len) {
    if (len <= 0) {
        return;
    }
    buf[0] = 0;
    if (text != nullptr) {
        CFStringGetCString(text, buf, len, kCFStringEncodingUTF8);
    }
}

// A device as discovery gave it: kept (one reference) until it goes.
struct Known {
    IDeckLink* device;
    // This arrival's own number: the same device unplugged and plugged in
    // again arrives anew, with another — an output opens on one arrival.
    uint64_t id;
    std::string name;
    bool can_output;
    // Its formats in 8-bit YUV, read as it arrived.
    std::vector<nida_dl_mode> modes;
    bool can_input = false;
    bool detects = false;
    int64_t persistent = 0;
    int64_t connections = 0;
    // The formats it takes pictures in, 8-bit YUV, read as it arrived.
    std::vector<nida_dl_in_mode> input_modes;
};

// Never destroyed: the API's threads may still tell of a device as the
// process ends, after statics would have gone.
std::mutex& g_known_lock = *new std::mutex;
std::vector<Known>& g_known = *new std::vector<Known>;
uint64_t g_next_id = 1;
// Counts arrivals and goings: what is known changed when it has.
std::atomic<uint64_t> g_generation{0};

std::string name_of(IDeckLink* device) {
    char buf[256];
    copy_cfstring(nullptr, buf, sizeof(buf));
    CFStringRef name = nullptr;
    if (device->GetDisplayName(&name) == S_OK && name != nullptr) {
        copy_cfstring(name, buf, sizeof(buf));
        CFRelease(name);
    }
    return std::string(buf);
}

std::vector<nida_dl_mode> modes_of(IDeckLinkOutput* output) {
    std::vector<nida_dl_mode> modes;
    IDeckLinkDisplayModeIterator* iterator = nullptr;
    if (output->GetDisplayModeIterator(&iterator) != S_OK || iterator == nullptr) {
        return modes;
    }
    IDeckLinkDisplayMode* mode = nullptr;
    while (iterator->Next(&mode) == S_OK) {
        bool supported = false;
        BMDDisplayMode code = mode->GetDisplayMode();
        if (output->DoesSupportVideoMode(bmdVideoConnectionUnspecified, code, bmdFormat8BitYUV,
                                         bmdNoVideoOutputConversion, bmdSupportedVideoModeDefault, nullptr,
                                         &supported) == S_OK &&
            supported) {
            nida_dl_mode out{};
            out.code = code;
            out.width = static_cast<int32_t>(mode->GetWidth());
            out.height = static_cast<int32_t>(mode->GetHeight());
            BMDTimeValue duration = 0;
            BMDTimeScale scale = 0;
            mode->GetFrameRate(&duration, &scale);
            out.frame_duration = duration;
            out.time_scale = scale;
            CFStringRef name = nullptr;
            if (mode->GetName(&name) == S_OK && name != nullptr) {
                copy_cfstring(name, out.name, sizeof(out.name));
                CFRelease(name);
            }
            modes.push_back(out);
        }
        mode->Release();
    }
    iterator->Release();
    return modes;
}

std::vector<nida_dl_in_mode> input_modes_of(IDeckLinkInput* input) {
    std::vector<nida_dl_in_mode> modes;
    IDeckLinkDisplayModeIterator* iterator = nullptr;
    if (input->GetDisplayModeIterator(&iterator) != S_OK || iterator == nullptr) {
        return modes;
    }
    IDeckLinkDisplayMode* mode = nullptr;
    while (iterator->Next(&mode) == S_OK) {
        bool supported = false;
        BMDDisplayMode code = mode->GetDisplayMode();
        if (input->DoesSupportVideoMode(bmdVideoConnectionUnspecified, code, bmdFormat8BitYUV,
                                        bmdNoVideoInputConversion, bmdSupportedVideoModeDefault, nullptr,
                                        &supported) == S_OK &&
            supported) {
            nida_dl_in_mode in{};
            in.code = code;
            in.width = static_cast<int32_t>(mode->GetWidth());
            in.height = static_cast<int32_t>(mode->GetHeight());
            BMDTimeValue duration = 0;
            BMDTimeScale scale = 0;
            mode->GetFrameRate(&duration, &scale);
            in.frame_duration = duration;
            in.time_scale = scale;
            const BMDFieldDominance field = mode->GetFieldDominance();
            in.progressive = (field == bmdProgressiveFrame || field == bmdProgressiveSegmentedFrame) ? 1 : 0;
            CFStringRef name = nullptr;
            if (mode->GetName(&name) == S_OK && name != nullptr) {
                copy_cfstring(name, in.name, sizeof(in.name));
                CFRelease(name);
            }
            modes.push_back(in);
        }
        mode->Release();
    }
    iterator->Release();
    return modes;
}

// What discovery tells: a device arrived, or went. On the API's own thread.
class Notifications final : public IDeckLinkDeviceNotificationCallback {
public:
    HRESULT DeckLinkDeviceArrived(IDeckLink* device) override {
        if (device == nullptr) {
            return S_OK;
        }
        device->AddRef();
        Known known{device, 0, name_of(device), false, {}};
        IDeckLinkOutput* output = nullptr;
        if (device->QueryInterface(IID_IDeckLinkOutput, reinterpret_cast<void**>(&output)) == S_OK &&
            output != nullptr) {
            known.can_output = true;
            known.modes = modes_of(output);
            output->Release();
        }
        IDeckLinkInput* input = nullptr;
        if (device->QueryInterface(IID_IDeckLinkInput, reinterpret_cast<void**>(&input)) == S_OK &&
            input != nullptr) {
            known.can_input = true;
            known.input_modes = input_modes_of(input);
            input->Release();
        }
        IDeckLinkProfileAttributes* attributes = nullptr;
        if (device->QueryInterface(IID_IDeckLinkProfileAttributes, reinterpret_cast<void**>(&attributes)) ==
                S_OK &&
            attributes != nullptr) {
            bool detects = false;
            if (attributes->GetFlag(BMDDeckLinkSupportsInputFormatDetection, &detects) == S_OK) {
                known.detects = detects;
            }
            int64_t persistent = 0;
            if (attributes->GetInt(BMDDeckLinkPersistentID, &persistent) == S_OK) {
                known.persistent = persistent;
            }
            int64_t connections = 0;
            if (attributes->GetInt(BMDDeckLinkVideoInputConnections, &connections) == S_OK) {
                known.connections = connections;
            }
            attributes->Release();
        }
        std::lock_guard<std::mutex> lock(g_known_lock);
        known.id = g_next_id++;
        g_known.push_back(std::move(known));
        ++g_generation;
        return S_OK;
    }

    HRESULT DeckLinkDeviceRemoved(IDeckLink* device) override {
        IDeckLink* gone = nullptr;
        {
            std::lock_guard<std::mutex> lock(g_known_lock);
            for (auto it = g_known.begin(); it != g_known.end(); ++it) {
                if (it->device == device) {
                    gone = it->device;
                    g_known.erase(it);
                    ++g_generation;
                    break;
                }
            }
        }
        // An output open on it keeps its own references.
        if (gone != nullptr) {
            gone->Release();
        }
        return S_OK;
    }

    HRESULT QueryInterface(REFIID iid, LPVOID* ppv) override {
        CFUUIDBytes unknown = CFUUIDGetUUIDBytes(IUnknownUUID);
        if (std::memcmp(&iid, &unknown, sizeof(REFIID)) == 0 ||
            std::memcmp(&iid, &IID_IDeckLinkDeviceNotificationCallback, sizeof(REFIID)) == 0) {
            *ppv = this;
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }

    // Lives as long as the process: counting is not needed.
    ULONG AddRef() override { return 1; }
    ULONG Release() override { return 1; }
};

Notifications g_notifications;
IDeckLinkDiscovery* g_discovery = nullptr;
std::once_flag g_discovery_started;

// Have discovery tell about devices — once: devices present already arrive
// at once.
void discover() {
    std::call_once(g_discovery_started, [] {
        g_discovery = CreateDeckLinkDiscoveryInstance();
        if (g_discovery != nullptr && g_discovery->InstallDeviceNotifications(&g_notifications) != S_OK) {
            g_discovery->Release();
            g_discovery = nullptr;
        }
    });
}

// The known device of arrival `id`, if it can put out video, with a
// reference of the caller's own; null when it has gone.
IDeckLink* output_device(uint64_t id) {
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    for (auto& known : g_known) {
        if (known.id == id && known.can_output) {
            known.device->AddRef();
            return known.device;
        }
    }
    return nullptr;
}

// The known device of arrival `id`, if it can take pictures in, with a
// reference of the caller's own; null when it has gone.
IDeckLink* input_device(uint64_t id) {
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    for (auto& known : g_known) {
        if (known.id == id && known.can_input) {
            known.device->AddRef();
            return known.device;
        }
    }
    return nullptr;
}

bool still_known(uint64_t id) {
    std::lock_guard<std::mutex> lock(g_known_lock);
    for (const auto& known : g_known) {
        if (known.id == id) {
            return true;
        }
    }
    return false;
}

// A device taking pictures in: the API calls it with each frame, and when
// the format coming in changes. Its calls into Nida are made under `lock`,
// so that once it is closed none is made.
class Capture final : public IDeckLinkInputCallback {
public:
    Capture(IDeckLink* device, IDeckLinkInput* input, uint64_t id, void* ctx, nida_dl_video_fn video,
            nida_dl_audio_fn audio)
        : device_(device), input_(input), id_(id), ctx_(ctx), video_(video), audio_(audio) {
        if (device->QueryInterface(IID_IDeckLinkConfiguration, reinterpret_cast<void**>(&config_)) != S_OK) {
            config_ = nullptr;
        }
    }

    IDeckLink* device_;
    IDeckLinkInput* input_;
    // Its settings — the connection video comes in on; null if it has none.
    IDeckLinkConfiguration* config_ = nullptr;
    uint64_t id_;
    BMDPixelFormat pixel_format_ = bmdFormat8BitYUV;
    std::atomic<uint32_t> colorspace_{0};
    std::atomic<int32_t> progressive_{1};
    std::atomic<int64_t> frame_duration_{0};
    std::atomic<int64_t> time_scale_{0};
    // Opening it again at a format that came in failed: it is to be opened
    // afresh.
    std::atomic<bool> failed_{false};
    // The format that came in, as the device told it, once it differed from
    // the one opened; 0 till then.
    std::atomic<uint32_t> detected_{0};

    // Make no more calls into Nida, nor open the device again at another
    // format: on return, none is being made.
    void close() {
        std::lock_guard<std::mutex> restarting(restart_lock_);
        std::lock_guard<std::mutex> guard(lock_);
        closed_ = true;
    }

    void take_mode(IDeckLinkDisplayMode* mode) {
        colorspace_ = static_cast<uint32_t>(mode->GetFlags());
        const BMDFieldDominance field = mode->GetFieldDominance();
        progressive_ = (field == bmdProgressiveFrame || field == bmdProgressiveSegmentedFrame ||
                        field == bmdUnknownFieldDominance)
                           ? 1
                           : 0;
        BMDTimeValue duration = 0;
        BMDTimeScale scale = 0;
        mode->GetFrameRate(&duration, &scale);
        frame_duration_ = duration;
        time_scale_ = scale;
    }

    HRESULT VideoInputFormatChanged(BMDVideoInputFormatChangedEvents events, IDeckLinkDisplayMode* mode,
                                    BMDDetectedVideoInputFormatFlags flags) override {
        if (mode == nullptr) {
            return S_OK;
        }
        // Not while it closes: its streams stopped stay stopped. Frames are
        // not held up meanwhile — they wait on `lock_` alone.
        std::lock_guard<std::mutex> restarting(restart_lock_);
        if (closed_) {
            return S_OK;
        }
        // RGB comes in as BGRA when the device can give it so; else, and for
        // YUV, as 8-bit YUV.
        BMDPixelFormat format = bmdFormat8BitYUV;
        if ((flags & bmdDetectedVideoInputRGB444) != 0) {
            bool supported = false;
            if (input_->DoesSupportVideoMode(bmdVideoConnectionUnspecified, mode->GetDisplayMode(),
                                             bmdFormat8BitBGRA, bmdNoVideoInputConversion,
                                             bmdSupportedVideoModeDefault, nullptr, &supported) == S_OK &&
                supported) {
                format = bmdFormat8BitBGRA;
            }
        }
        if ((events & (bmdVideoInputDisplayModeChanged | bmdVideoInputFieldDominanceChanged)) == 0 &&
            format == pixel_format_) {
            take_mode(mode);
            return S_OK;
        }
        // Opened again at the format that came in, as Blackmagic's own
        // samples do; what it is taken as only once that has worked. Should
        // it not, the capture is opened afresh.
        const bool ok = input_->PauseStreams() == S_OK &&
                        input_->EnableVideoInput(mode->GetDisplayMode(), format,
                                                 bmdVideoInputEnableFormatDetection) == S_OK &&
                        input_->FlushStreams() == S_OK && input_->StartStreams() == S_OK;
        if (!ok) {
            failed_ = true;
            return S_OK;
        }
        pixel_format_ = format;
        take_mode(mode);
        detected_ = mode->GetDisplayMode();
        return S_OK;
    }

    HRESULT VideoInputFrameArrived(IDeckLinkVideoInputFrame* frame, IDeckLinkAudioInputPacket* packet) override {
        std::lock_guard<std::mutex> guard(lock_);
        if (closed_) {
            return S_OK;
        }
        if (packet != nullptr) {
            void* samples = nullptr;
            const long count = packet->GetSampleFrameCount();
            if (count > 0 && packet->GetBytes(&samples) == S_OK && samples != nullptr) {
                audio_(ctx_, static_cast<const int32_t*>(samples), static_cast<int32_t>(count), 2);
            }
        }
        if (frame != nullptr) {
            void* bytes = nullptr;
            if (frame->GetBytes(&bytes) == S_OK && bytes != nullptr) {
                const bool no_signal = (frame->GetFlags() & bmdFrameHasNoInputSource) != 0;
                video_(ctx_, static_cast<const uint8_t*>(bytes), static_cast<int32_t>(frame->GetRowBytes()),
                       static_cast<int32_t>(frame->GetWidth()), static_cast<int32_t>(frame->GetHeight()),
                       frame->GetPixelFormat(), colorspace_.load(), no_signal ? 1 : 0, progressive_.load(),
                       frame_duration_.load(), time_scale_.load());
            }
        }
        return S_OK;
    }

    HRESULT QueryInterface(REFIID iid, LPVOID* ppv) override {
        CFUUIDBytes unknown = CFUUIDGetUUIDBytes(IUnknownUUID);
        if (std::memcmp(&iid, &unknown, sizeof(REFIID)) == 0 ||
            std::memcmp(&iid, &IID_IDeckLinkInputCallback, sizeof(REFIID)) == 0) {
            AddRef();
            *ppv = this;
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }

    ULONG AddRef() override { return ++refs_; }

    ULONG Release() override {
        const ULONG left = --refs_;
        if (left == 0) {
            delete this;
        }
        return left;
    }

private:
    ~Capture() override {
        if (config_ != nullptr) {
            config_->Release();
        }
    }

    void* ctx_;
    nida_dl_video_fn video_;
    nida_dl_audio_fn audio_;
    std::mutex lock_;
    std::mutex restart_lock_;
    std::atomic<bool> closed_{false};
    std::atomic<ULONG> refs_{1};
};

// The captures open now — for stopping them all as Nida quits (see
// `nida_dl_shutdown`). Never destroyed: the process's last moments use it.
// Held across a capture's start and its stop, so shutdown misses none.
std::mutex& g_open_lock = *new std::mutex;
std::set<Capture*>& g_open = *new std::set<Capture*>;
// Nida is quitting: no capture is started from here on.
std::atomic<bool> g_shut_down{false};

// Stop `capture` taking pictures in, its callback let go: on return no more
// frames come for it. Not let go itself — `nida_dl_capture_close` does
// that; stopping twice does no harm.
void stop_capture(Capture* capture) {
    capture->close();
    IDeckLinkInput* input = capture->input_;
    input->StopStreams();
    input->DisableAudioInput();
    input->DisableVideoInput();
    input->SetCallback(nullptr);
}

struct Output {
    IDeckLink* device;
    IDeckLinkOutput* output;
    // What the device says it is doing; taken once, at opening.
    IDeckLinkStatus* status;
    IDeckLinkMutableVideoFrame* frames[2];
    int next;
    int32_t width;
    int32_t height;
    BMDDisplayMode mode;
};

}  // namespace

extern "C" {

// Desktop Video's DeckLink API version ("16.4") into `buf`; 0 when it is
// installed, -1 when not.
int32_t nida_dl_api_version(char* buf, int32_t len) {
    copy_cfstring(nullptr, buf, len);
    IDeckLinkAPIInformation* info = CreateDeckLinkAPIInformationInstance();
    if (info == nullptr) {
        return -1;
    }
    CFStringRef version = nullptr;
    if (info->GetString(BMDDeckLinkAPIVersion, &version) == S_OK && version != nullptr) {
        copy_cfstring(version, buf, len);
        CFRelease(version);
    }
    info->Release();
    return 0;
}

// Every DeckLink device, whether it can put out video or not.
int32_t nida_dl_all_device_count(void) {
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    return static_cast<int32_t>(g_known.size());
}

// DeckLink devices that can put out video; 0 without Desktop Video.
int32_t nida_dl_device_count(void) {
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    int32_t count = 0;
    for (const auto& known : g_known) {
        count += known.can_output ? 1 : 0;
    }
    return count;
}

// Output device `index`'s arrival number (see `Known::id`); 0 when none.
uint64_t nida_dl_device_id(int32_t index) {
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    int32_t found = -1;
    for (const auto& known : g_known) {
        if (known.can_output && ++found == index) {
            return known.id;
        }
    }
    return 0;
}

// Output device `index`'s name, UTF-8, into `buf`; 0 when found.
int32_t nida_dl_device_name(int32_t index, char* buf, int32_t len) {
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    int32_t found = -1;
    for (const auto& known : g_known) {
        if (known.can_output && ++found == index) {
            if (len > 0) {
                std::strncpy(buf, known.name.c_str(), static_cast<size_t>(len) - 1);
                buf[len - 1] = 0;
            }
            return 0;
        }
    }
    copy_cfstring(nullptr, buf, len);
    return -1;
}

// Up to `max` modes output device `index` can put out in 8-bit YUV; how many.
int32_t nida_dl_device_modes(int32_t index, nida_dl_mode* modes, int32_t max) {
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    int32_t found = -1;
    for (const auto& known : g_known) {
        if (known.can_output && ++found == index) {
            int32_t count = 0;
            for (const auto& mode : known.modes) {
                if (count >= max) {
                    break;
                }
                modes[count++] = mode;
            }
            return count;
        }
    }
    return 0;
}

// Open the output device of arrival `id` in display mode `mode`, 8-bit YUV;
// null on failure, why in `err`.
void* nida_dl_open(uint64_t id, uint32_t mode, char* err, int32_t errlen) {
    auto fail = [&](const char* why) -> void* {
        if (errlen > 0) {
            std::strncpy(err, why, static_cast<size_t>(errlen) - 1);
            err[errlen - 1] = 0;
        }
        return nullptr;
    };
    IDeckLink* device = output_device(id);
    if (device == nullptr) {
        return fail("the Blackmagic device is not connected (any more)");
    }
    IDeckLinkOutput* output = nullptr;
    if (device->QueryInterface(IID_IDeckLinkOutput, reinterpret_cast<void**>(&output)) != S_OK ||
        output == nullptr) {
        device->Release();
        return fail("the device puts out no video");
    }
    IDeckLinkDisplayMode* display = nullptr;
    if (output->GetDisplayMode(mode, &display) != S_OK || display == nullptr) {
        output->Release();
        device->Release();
        return fail("the device has no such video format");
    }
    const int32_t width = static_cast<int32_t>(display->GetWidth());
    const int32_t height = static_cast<int32_t>(display->GetHeight());
    display->Release();
    if (output->EnableVideoOutput(mode, bmdVideoOutputFlagDefault) != S_OK) {
        output->Release();
        device->Release();
        return fail("the device would not start its video output (in use by another app?)");
    }
    IDeckLinkStatus* status = nullptr;
    if (device->QueryInterface(IID_IDeckLinkStatus, reinterpret_cast<void**>(&status)) != S_OK) {
        status = nullptr;
    }
    auto* out = new Output{device, output, status, {nullptr, nullptr}, 0, width, height, mode};
    for (auto& frame : out->frames) {
        if (output->CreateVideoFrame(width, height, width * 2, bmdFormat8BitYUV, bmdFrameFlagDefault, &frame) != S_OK) {
            frame = nullptr;
        }
    }
    if (out->frames[0] == nullptr || out->frames[1] == nullptr) {
        for (auto* frame : out->frames) {
            if (frame != nullptr) {
                frame->Release();
            }
        }
        output->DisableVideoOutput();
        if (status != nullptr) {
            status->Release();
        }
        output->Release();
        device->Release();
        delete out;
        return fail("the device gave no video frame to draw into");
    }
    return out;
}

// Put one 8-bit YUV ('2vuy') picture up on the device at its next frame:
// `row_bytes` per line, `width`×`height` as opened. 0 when shown.
int32_t nida_dl_show(void* handle, const uint8_t* data, int32_t row_bytes, int32_t width, int32_t height) {
    auto* out = static_cast<Output*>(handle);
    if (out == nullptr || data == nullptr || width != out->width || height != out->height ||
        row_bytes < width * 2) {
        return -1;
    }
    IDeckLinkMutableVideoFrame* frame = out->frames[out->next];
    out->next ^= 1;
    void* bytes = nullptr;
    if (frame->GetBytes(&bytes) != S_OK || bytes == nullptr) {
        return -2;
    }
    const int32_t frame_row = static_cast<int32_t>(frame->GetRowBytes());
    const int32_t copy = row_bytes < frame_row ? row_bytes : frame_row;
    for (int32_t y = 0; y < height; ++y) {
        std::memcpy(static_cast<uint8_t*>(bytes) + static_cast<size_t>(y) * frame_row,
                    data + static_cast<size_t>(y) * row_bytes, static_cast<size_t>(copy));
    }
    return out->output->DisplayVideoFrameSync(frame) == S_OK ? 0 : -3;
}

struct nida_dl_state {
    char mode_name[64];
    uint32_t mode;
    int64_t frame_duration;
    int64_t time_scale;
    uint32_t current_mode;
    int64_t ticks_per_frame;
};

// What an open output is doing, for its log: the format enabled (its name,
// code and rate), what the device itself says it puts out (`current_mode`,
// 0 when it does not say) and its clock's frame (`ticks_per_frame`, in
// microseconds; 0 when unknown).
int32_t nida_dl_state(void* handle, nida_dl_state* state) {
    auto* out = static_cast<Output*>(handle);
    if (out == nullptr || state == nullptr) {
        return -1;
    }
    std::memset(state, 0, sizeof(*state));
    state->mode = out->mode;
    IDeckLinkDisplayMode* display = nullptr;
    if (out->output->GetDisplayMode(out->mode, &display) == S_OK && display != nullptr) {
        CFStringRef name = nullptr;
        if (display->GetName(&name) == S_OK && name != nullptr) {
            copy_cfstring(name, state->mode_name, sizeof(state->mode_name));
            CFRelease(name);
        }
        BMDTimeValue duration = 0;
        BMDTimeScale scale = 0;
        display->GetFrameRate(&duration, &scale);
        state->frame_duration = duration;
        state->time_scale = scale;
        display->Release();
    }
    if (out->status != nullptr) {
        int64_t current = 0;
        if (out->status->GetInt(bmdDeckLinkStatusCurrentVideoOutputMode, &current) == S_OK) {
            state->current_mode = static_cast<uint32_t>(current);
        }
    }
    BMDTimeValue hardware = 0;
    BMDTimeValue in_frame = 0;
    BMDTimeValue per_frame = 0;
    if (out->output->GetHardwareReferenceClock(1000000, &hardware, &in_frame, &per_frame) == S_OK) {
        state->ticks_per_frame = per_frame;
    }
    return 0;
}

// A converter from RGBA, as read back, to 8-bit 4:2:2 '2vuy' (Cb Y0 Cr Y1),
// with the matrix of a picture `height` lines high: BT.709 from 720 lines,
// else BT.601. Video range — Y 16–235 — unless `full`: then Y and C over the
// whole range but 0 and 255, which the signal keeps for its own timing.
// Apple's Accelerate (vImage) does the work. Null when it cannot be made.
void* nida_dl_converter_new(int32_t height, int32_t full) {
    auto* conversion = new vImage_ARGBToYpCbCr;
    vImage_YpCbCrPixelRange range = full != 0
        ? vImage_YpCbCrPixelRange{0, 128, 255, 255, 254, 1, 254, 1}
        : vImage_YpCbCrPixelRange{16, 128, 235, 240, 235, 16, 240, 16};
    const vImage_ARGBToYpCbCrMatrix* matrix =
        height >= 720 ? kvImage_ARGBToYpCbCrMatrix_ITU_R_709_2 : kvImage_ARGBToYpCbCrMatrix_ITU_R_601_4;
    if (vImageConvert_ARGBToYpCbCr_GenerateConversion(matrix, &range, conversion, kvImageARGB8888,
                                                      kvImage422CbYpCrYp8, kvImageNoFlags) != kvImageNoError) {
        delete conversion;
        return nullptr;
    }
    return conversion;
}

// Convert one `width`×`height` RGBA picture, `row_bytes` a line, into `out`,
// `out_row_bytes` a line. On the calling thread only (no worker threads of
// vImage's own). 0 when converted.
int32_t nida_dl_converter_run(void* converter, const uint8_t* rgba, int32_t row_bytes, int32_t width,
                              int32_t height, uint8_t* out, int32_t out_row_bytes) {
    if (converter == nullptr || width <= 0 || height <= 0) {
        return -1;
    }
    vImage_Buffer src = {const_cast<uint8_t*>(rgba), static_cast<vImagePixelCount>(height),
                         static_cast<vImagePixelCount>(width), static_cast<size_t>(row_bytes)};
    vImage_Buffer dst = {out, static_cast<vImagePixelCount>(height), static_cast<vImagePixelCount>(width),
                         static_cast<size_t>(out_row_bytes)};
    // The picture is R, G, B, A; vImage reads A, R, G, B.
    const uint8_t permute[4] = {3, 0, 1, 2};
    return vImageConvert_ARGB8888To422CbYpCrYp8(&src, &dst, static_cast<vImage_ARGBToYpCbCr*>(converter),
                                                permute, kvImageDoNotTile) == kvImageNoError
               ? 0
               : -2;
}

void nida_dl_converter_free(void* converter) {
    delete static_cast<vImage_ARGBToYpCbCr*>(converter);
}

// Every device that can take pictures in, up to `max`, into `infos`; how
// many there are.
int32_t nida_dl_inputs(nida_dl_input_info* infos, int32_t max) {
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    int32_t count = 0;
    for (const auto& known : g_known) {
        if (!known.can_input) {
            continue;
        }
        if (count < max) {
            nida_dl_input_info& info = infos[count];
            info.id = known.id;
            info.persistent = known.persistent;
            info.connections = known.connections;
            info.detects = known.detects ? 1 : 0;
            std::strncpy(info.name, known.name.c_str(), sizeof(info.name) - 1);
            info.name[sizeof(info.name) - 1] = 0;
        }
        ++count;
    }
    return count;
}

// Up to `max` formats input device of arrival `id` takes pictures in; how
// many were given.
int32_t nida_dl_input_modes(uint64_t id, nida_dl_in_mode* modes, int32_t max) {
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    for (const auto& known : g_known) {
        if (known.id == id && known.can_input) {
            int32_t count = 0;
            for (const auto& mode : known.input_modes) {
                if (count >= max) {
                    break;
                }
                modes[count++] = mode;
            }
            return count;
        }
    }
    return 0;
}

// Start taking pictures and sound in from the device of arrival `id`, in
// display mode `mode` — following the format that comes in, when the device
// can tell it — calling `video` and `audio` with what comes, on the API's
// thread, with `ctx`. Null on failure, why in `err`.
void* nida_dl_capture_open(uint64_t id, uint32_t mode, void* ctx, nida_dl_video_fn video, nida_dl_audio_fn audio,
                           char* err, int32_t errlen) {
    auto fail = [&](const char* why) -> void* {
        if (errlen > 0) {
            std::strncpy(err, why, static_cast<size_t>(errlen) - 1);
            err[errlen - 1] = 0;
        }
        return nullptr;
    };
    IDeckLink* device = input_device(id);
    if (device == nullptr) {
        return fail("the Blackmagic device is not connected (any more)");
    }
    bool detects = false;
    {
        std::lock_guard<std::mutex> lock(g_known_lock);
        for (const auto& known : g_known) {
            if (known.id == id) {
                detects = known.detects;
            }
        }
    }
    IDeckLinkInput* input = nullptr;
    if (device->QueryInterface(IID_IDeckLinkInput, reinterpret_cast<void**>(&input)) != S_OK || input == nullptr) {
        device->Release();
        return fail("the device takes no video in");
    }
    IDeckLinkDisplayMode* display = nullptr;
    if (input->GetDisplayMode(mode, &display) != S_OK || display == nullptr) {
        input->Release();
        device->Release();
        return fail("the device has no such video format");
    }
    auto* capture = new Capture(device, input, id, ctx, video, audio);
    capture->take_mode(display);
    display->Release();
    const BMDVideoInputFlags flags = detects ? bmdVideoInputEnableFormatDetection : bmdVideoInputFlagDefault;
    if (input->SetCallback(capture) != S_OK ||
        input->EnableVideoInput(mode, bmdFormat8BitYUV, flags) != S_OK) {
        input->SetCallback(nullptr);
        capture->close();
        capture->Release();
        input->Release();
        device->Release();
        return fail("the device would not start its video input (in use by another app?)");
    }
    // Without sound it still shows its picture.
    input->EnableAudioInput(bmdAudioSampleRate48kHz, bmdAudioSampleType32bitInteger, 2);
    // Started and counted as open in one go, and not once Nida is quitting:
    // shutdown stops every capture started.
    std::lock_guard<std::mutex> open(g_open_lock);
    const bool quitting = g_shut_down.load();
    if (quitting || input->StartStreams() != S_OK) {
        input->DisableAudioInput();
        input->DisableVideoInput();
        input->SetCallback(nullptr);
        capture->close();
        capture->Release();
        input->Release();
        device->Release();
        return fail(quitting ? "Nida is quitting" : "the device would not start taking pictures in");
    }
    g_open.insert(capture);
    return capture;
}

// 1 while the device of an open capture is still connected, and taking
// pictures in at the format that comes.
int32_t nida_dl_capture_alive(void* handle) {
    auto* capture = static_cast<Capture*>(handle);
    return capture != nullptr && !capture->failed_ && still_known(capture->id_) ? 1 : 0;
}

// Take video in on `connection` (a `BMDVideoConnection` bit) from now; 0
// when the device took it. As Desktop Video Setup's own setting would: the
// box was seen to keep it after Nida closed.
int32_t nida_dl_capture_set_connection(void* handle, int64_t connection) {
    auto* capture = static_cast<Capture*>(handle);
    if (capture == nullptr || capture->config_ == nullptr) {
        return -1;
    }
    return capture->config_->SetInt(bmdDeckLinkConfigVideoInputConnection, connection) == S_OK ? 0 : -1;
}

// The connection an open capture takes video in on; 0 when unknown.
int64_t nida_dl_capture_connection(void* handle) {
    auto* capture = static_cast<Capture*>(handle);
    int64_t connection = 0;
    if (capture == nullptr || capture->config_ == nullptr ||
        capture->config_->GetInt(bmdDeckLinkConfigVideoInputConnection, &connection) != S_OK) {
        return 0;
    }
    return connection;
}

// The format that came in, once it differed from the one opened (a
// `BMDDisplayMode`); 0 till then.
uint32_t nida_dl_capture_detected(void* handle) {
    auto* capture = static_cast<Capture*>(handle);
    return capture == nullptr ? 0 : capture->detected_.load();
}

// Changes with every device arriving or going.
uint64_t nida_dl_generation(void) {
    discover();
    return g_generation.load();
}

// Stop taking pictures in: on return, no more calls are made with its `ctx`.
void nida_dl_capture_close(void* handle) {
    auto* capture = static_cast<Capture*>(handle);
    if (capture == nullptr) {
        return;
    }
    IDeckLinkInput* input = capture->input_;
    IDeckLink* device = capture->device_;
    {
        // Stopped before it leaves the open ones: shutdown, meanwhile,
        // waits for it rather than finding it gone while frames still come.
        std::lock_guard<std::mutex> open(g_open_lock);
        stop_capture(capture);
        g_open.erase(capture);
    }
    capture->Release();
    input->Release();
    device->Release();
}

// Nida is quitting: every open capture stopped and its callback let go,
// and device notifications too, before the process ends. Quit from macOS
// (the Dock, an Apple Event) ends it straight after the show is saved, the
// captures never closed: Blackmagic's library, its own locks gone with the
// process, then took a frame in and aborted (seen on the Studio).
void nida_dl_shutdown(void) {
    {
        std::lock_guard<std::mutex> open(g_open_lock);
        g_shut_down = true;
        for (Capture* capture : g_open) {
            stop_capture(capture);
        }
    }
    if (g_discovery != nullptr) {
        g_discovery->UninstallDeviceNotifications();
    }
}

void nida_dl_close(void* handle) {
    auto* out = static_cast<Output*>(handle);
    if (out == nullptr) {
        return;
    }
    out->output->DisableVideoOutput();
    for (auto* frame : out->frames) {
        if (frame != nullptr) {
            frame->Release();
        }
    }
    if (out->status != nullptr) {
        out->status->Release();
    }
    out->output->Release();
    out->device->Release();
    delete out;
}

}  // extern "C"
