// A small C face on Blackmagic's DeckLink API for Nida's Blackmagic outputs
// (see mod.rs). Desktop Video supplies the API at runtime: COM on Windows,
// DeckLinkAPI.framework on macOS (DeckLinkAPIDispatch.cpp). Without it there are
// simply no devices.
//
// Devices are known through the API's discovery, which says when one arrives
// and when it goes: each is connected to once, as it arrives, its name and
// formats read then, and kept until it goes — an output opens on that very
// object. They are never looked for again with fresh iterators: doing that
// every few seconds while an output put pictures up crashed Nida within the
// hour (a connection torn down under a dispatch source still watching it —
// libdispatch's "kevent vanished", 2026-10-04 and -05).

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <limits>
#include <mutex>
#include <string>
#include <vector>

#ifndef NIDA_DECKLINK_PLATFORM_HEADER
#define NIDA_DECKLINK_PLATFORM_HEADER "platform.hpp"
#endif
#include NIDA_DECKLINK_PLATFORM_HEADER

extern "C" {

struct nida_dl_mode {
    uint32_t code;
    int32_t width;
    int32_t height;
    int64_t frame_duration;
    int64_t time_scale;
    char name[64];
};

}  // extern "C"

namespace {

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
};

// Never destroyed: the API's threads may still tell of a device as the
// process ends, after statics would have gone.
std::mutex& g_known_lock = *new std::mutex;
std::vector<Known>& g_known = *new std::vector<Known>;
uint64_t g_next_id = 1;

std::string name_of(IDeckLink* device) {
    char buf[256];
    copy_dlstring(nullptr, buf, sizeof(buf));
    DlString name = nullptr;
    if (device->GetDisplayName(&name) == S_OK && name != nullptr) {
        copy_dlstring(name, buf, sizeof(buf));
        free_dlstring(name);
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
        DlSupported supported = false;
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
            DlString name = nullptr;
            if (mode->GetName(&name) == S_OK && name != nullptr) {
                copy_dlstring(name, out.name, sizeof(out.name));
                free_dlstring(name);
            }
            modes.push_back(out);
        }
        mode->Release();
    }
    iterator->Release();
    return modes;
}

// What discovery tells: a device arrived, or went. On the API's own thread.
class Notifications final : public IDeckLinkDeviceNotificationCallback {
public:
    HRESULT NIDA_COM DeckLinkDeviceArrived(IDeckLink* device) override {
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
        std::lock_guard<std::mutex> lock(g_known_lock);
        known.id = g_next_id++;
        g_known.push_back(std::move(known));
        return S_OK;
    }

    HRESULT NIDA_COM DeckLinkDeviceRemoved(IDeckLink* device) override {
        IDeckLink* gone = nullptr;
        {
            std::lock_guard<std::mutex> lock(g_known_lock);
            for (auto it = g_known.begin(); it != g_known.end(); ++it) {
                if (it->device == device) {
                    gone = it->device;
                    g_known.erase(it);
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

    HRESULT NIDA_COM QueryInterface(REFIID iid, LPVOID* ppv) override {
        if (ppv == nullptr) return E_POINTER;
        if (dl_is_unknown(iid) || dl_same_iid(iid, IID_IDeckLinkDeviceNotificationCallback)) {
            *ppv = this;
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }

    // Lives as long as the process: counting is not needed.
    ULONG NIDA_COM AddRef() override { return 1; }
    ULONG NIDA_COM Release() override { return 1; }
};

Notifications& g_notifications = *new Notifications;
IDeckLinkDiscovery* g_discovery = nullptr;
std::once_flag g_discovery_started;

// Have discovery tell about devices — once: devices present already arrive
// at once.
void discover() {
    std::call_once(g_discovery_started, [] {
        dl_start_discovery([] {
            g_discovery = dl_create_discovery();
            if (g_discovery != nullptr && g_discovery->InstallDeviceNotifications(&g_notifications) != S_OK) {
                g_discovery->Release();
                g_discovery = nullptr;
            }
        });
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
    copy_dlstring(nullptr, buf, len);
    IDeckLinkAPIInformation* info = dl_create_api_information();
    if (info == nullptr) {
        return -1;
    }
    DlString version = nullptr;
    if (info->GetString(BMDDeckLinkAPIVersion, &version) == S_OK && version != nullptr) {
        copy_dlstring(version, buf, len);
        free_dlstring(version);
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

// Copy one device's identity, name and modes under one discovery lock. A
// removal between the count and this read skips the entry instead of mixing
// one device's name/modes with another device's arrival identity.
int32_t nida_dl_device_snapshot(int32_t index, uint64_t* id, char* name,
                               int32_t len, nida_dl_mode* modes, int32_t max) {
    if (id == nullptr || name == nullptr || modes == nullptr || len <= 0 || max < 0) return -1;
    *id = 0;
    name[0] = 0;
    discover();
    std::lock_guard<std::mutex> lock(g_known_lock);
    int32_t found = -1;
    for (const auto& known : g_known) {
        if (known.can_output && ++found == index) {
            *id = known.id;
            std::strncpy(name, known.name.c_str(), static_cast<size_t>(len) - 1);
            name[len - 1] = 0;
            const int32_t count = std::min(max, static_cast<int32_t>(known.modes.size()));
            std::copy_n(known.modes.begin(), count, modes);
            return count;
        }
    }
    return -1;
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
    if (!dl_thread_ready()) return fail("could not initialize the Blackmagic output worker COM apartment");
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
    DlSupported supported = false;
    if (output->DoesSupportVideoMode(bmdVideoConnectionUnspecified,
            static_cast<BMDDisplayMode>(mode), bmdFormat8BitYUV,
            bmdNoVideoOutputConversion, bmdSupportedVideoModeDefault,
            nullptr, &supported) != S_OK || !supported) {
        output->Release();
        device->Release();
        return fail("the device does not support 8-bit YUV in that video format");
    }
    IDeckLinkDisplayMode* display = nullptr;
    if (output->GetDisplayMode(static_cast<BMDDisplayMode>(mode), &display) != S_OK || display == nullptr) {
        output->Release();
        device->Release();
        return fail("the device has no such video format");
    }
    const int32_t width = static_cast<int32_t>(display->GetWidth());
    const int32_t height = static_cast<int32_t>(display->GetHeight());
    display->Release();
    if (width <= 0 || height <= 0 || width % 2 != 0 ||
        width > std::numeric_limits<int32_t>::max() / 4) {
        output->Release();
        device->Release();
        return fail("the device returned an invalid video frame size");
    }
    if (output->EnableVideoOutput(static_cast<BMDDisplayMode>(mode), bmdVideoOutputFlagDefault) != S_OK) {
        output->Release();
        device->Release();
        return fail("the device would not start its video output (in use by another app?)");
    }
    IDeckLinkStatus* status = nullptr;
    if (device->QueryInterface(IID_IDeckLinkStatus, reinterpret_cast<void**>(&status)) != S_OK) {
        status = nullptr;
    }
    auto* out = new Output{device, output, status, {nullptr, nullptr}, 0, width, height, static_cast<BMDDisplayMode>(mode)};
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
        DlString name = nullptr;
        if (display->GetName(&name) == S_OK && name != nullptr) {
            copy_dlstring(name, state->mode_name, sizeof(state->mode_name));
            free_dlstring(name);
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

#if defined(__APPLE__)
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

#endif  // __APPLE__

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
