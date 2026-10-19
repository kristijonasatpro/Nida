// A behavioral SDK double for the shared shim. This does not model COM ABI.
#pragma once
#include <atomic>
#include <cassert>
#include <cstdlib>

#define NIDA_COM
using HRESULT = int;
using ULONG = unsigned long;
using REFIID = int;
using LPVOID = void*;
using DlString = const char*;
using DlSupported = bool;
using BMDDisplayMode = uint32_t;
using BMDTimeValue = int64_t;
using BMDTimeScale = int64_t;
constexpr int S_OK = 0, E_FAIL = -1, E_POINTER = -2, E_NOINTERFACE = -3;
constexpr int IID_IDeckLinkOutput = 1, IID_IDeckLinkStatus = 2;
constexpr int IID_IDeckLinkDeviceNotificationCallback = 3;
constexpr int bmdVideoConnectionUnspecified = 0, bmdFormat8BitYUV = 0;
constexpr int bmdNoVideoOutputConversion = 0, bmdSupportedVideoModeDefault = 0;
constexpr int bmdVideoOutputFlagDefault = 0, bmdFrameFlagDefault = 0;
constexpr int bmdDeckLinkStatusCurrentVideoOutputMode = 0, BMDDeckLinkAPIVersion = 0;
constexpr int IID_IDeckLinkVideoOutputCallback = 4, IID_IDeckLinkAudioOutputCallback = 5;
constexpr int bmdAudioSampleRate48kHz = 48000, bmdAudioSampleType32bitInteger = 32;
constexpr int bmdAudioOutputStreamTimestamped = 1;
using BMDOutputFrameCompletionResult = int;
constexpr int bmdOutputFrameFlushed = 3;

struct IDeckLinkDisplayMode {
    virtual BMDDisplayMode GetDisplayMode() = 0;
    virtual long GetWidth() = 0;
    virtual long GetHeight() = 0;
    virtual HRESULT GetName(DlString*) = 0;
    virtual HRESULT GetFrameRate(BMDTimeValue*, BMDTimeScale*) = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLinkDisplayModeIterator {
    virtual HRESULT Next(IDeckLinkDisplayMode**) = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLinkVideoFrame {
    virtual HRESULT GetBytes(void**) = 0;
    virtual long GetRowBytes() = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLinkMutableVideoFrame : IDeckLinkVideoFrame {};
struct IDeckLinkVideoOutputCallback {
    virtual HRESULT ScheduledFrameCompleted(IDeckLinkVideoFrame*, BMDOutputFrameCompletionResult) = 0;
    virtual HRESULT ScheduledPlaybackHasStopped() = 0;
    virtual HRESULT QueryInterface(REFIID, LPVOID*) = 0;
    virtual ULONG AddRef() = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLinkAudioOutputCallback {
    virtual HRESULT RenderAudioSamples(DlSupported) = 0;
    virtual HRESULT QueryInterface(REFIID, LPVOID*) = 0;
    virtual ULONG AddRef() = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLinkOutput {
    virtual HRESULT DoesSupportVideoMode(int, BMDDisplayMode, int, int, int,
                                         BMDDisplayMode*, DlSupported*) = 0;
    virtual HRESULT GetDisplayModeIterator(IDeckLinkDisplayModeIterator**) = 0;
    virtual HRESULT GetDisplayMode(BMDDisplayMode, IDeckLinkDisplayMode**) = 0;
    virtual HRESULT EnableVideoOutput(BMDDisplayMode, int) = 0;
    virtual HRESULT DisableVideoOutput() = 0;
    virtual HRESULT CreateVideoFrame(int, int, int, int, int, IDeckLinkMutableVideoFrame**) = 0;
    virtual HRESULT DisplayVideoFrameSync(IDeckLinkVideoFrame*) = 0;
    virtual HRESULT ScheduleVideoFrame(IDeckLinkVideoFrame*, BMDTimeValue, BMDTimeValue, BMDTimeScale) { return E_FAIL; }
    virtual HRESULT SetScheduledFrameCompletionCallback(IDeckLinkVideoOutputCallback*) { return S_OK; }
    virtual HRESULT EnableAudioOutput(int, int, unsigned int, int) { return E_FAIL; }
    virtual HRESULT DisableAudioOutput() { return S_OK; }
    virtual HRESULT SetAudioCallback(IDeckLinkAudioOutputCallback*) { return S_OK; }
    virtual HRESULT BeginAudioPreroll() { return E_FAIL; }
    virtual HRESULT EndAudioPreroll() { return E_FAIL; }
    virtual HRESULT GetBufferedAudioSampleFrameCount(unsigned int*) { return E_FAIL; }
    virtual HRESULT ScheduleAudioSamples(void*, unsigned int, BMDTimeValue, BMDTimeScale, unsigned int*) { return E_FAIL; }
    virtual HRESULT StartScheduledPlayback(BMDTimeValue, BMDTimeScale, double) { return E_FAIL; }
    virtual HRESULT StopScheduledPlayback(BMDTimeValue, BMDTimeValue*, BMDTimeScale) { return S_OK; }
    virtual HRESULT GetScheduledStreamTime(BMDTimeScale, BMDTimeValue*, double*) { return E_FAIL; }
    virtual ULONG AddRef() { return 1; }
    virtual HRESULT GetHardwareReferenceClock(BMDTimeScale, BMDTimeValue*, BMDTimeValue*, BMDTimeValue*) = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLinkStatus {
    virtual HRESULT GetInt(int, int64_t*) = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLink {
    virtual HRESULT GetDisplayName(DlString*) = 0;
    virtual HRESULT QueryInterface(REFIID, LPVOID*) = 0;
    virtual ULONG AddRef() = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLinkDeviceNotificationCallback {
    virtual HRESULT DeckLinkDeviceArrived(IDeckLink*) = 0;
    virtual HRESULT DeckLinkDeviceRemoved(IDeckLink*) = 0;
    virtual HRESULT QueryInterface(REFIID, LPVOID*) = 0;
    virtual ULONG AddRef() = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLinkDiscovery {
    virtual HRESULT InstallDeviceNotifications(IDeckLinkDeviceNotificationCallback*) = 0;
    virtual ULONG Release() = 0;
};
struct IDeckLinkAPIInformation {
    virtual HRESULT GetString(int, DlString*) = 0;
    virtual ULONG Release() = 0;
};

inline bool fake_ready = true;
inline IDeckLinkDiscovery* fake_discovery = nullptr;
inline int discovery_starts = 0;
inline bool dl_thread_ready() { return fake_ready; }
inline bool dl_is_unknown(REFIID iid) { return iid == 0; }
inline bool dl_same_iid(REFIID a, REFIID b) { return a == b; }
inline void copy_dlstring(DlString text, char* buf, int32_t len) {
    if (len <= 0) return;
    std::strncpy(buf, text == nullptr ? "" : text, static_cast<size_t>(len) - 1);
    buf[len - 1] = 0;
}
inline void free_dlstring(DlString) {}
inline IDeckLinkDiscovery* dl_create_discovery() { return fake_discovery; }
inline IDeckLinkAPIInformation* dl_create_api_information() { return nullptr; }
template<class Start> void dl_start_discovery(Start start) { ++discovery_starts; start(); }
