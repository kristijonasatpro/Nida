// Platform details for the shared DeckLink device shim. The API interfaces
// come from the SDK on both platforms; no driver DLL is linked or bundled.
#pragma once

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <oleauto.h>
#include <future>
#include <condition_variable>
#include <thread>
#include "DeckLinkAPI.h"

#define NIDA_COM STDMETHODCALLTYPE
using DlString = BSTR;
using DlSupported = BOOL;

// A worker's apartment lasts until it exits, after its output has been
// closed. Never balance an unsuccessful CoInitializeEx, including an STA
// conflict. Rust's output worker opens, uses and closes its device itself.
class DlApartment {
    HRESULT result_ = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
public:
    bool ready() const { return SUCCEEDED(result_); }
    ~DlApartment() { if (ready()) CoUninitialize(); }
};

inline bool dl_thread_ready() {
    thread_local DlApartment apartment;
    return apartment.ready();
}

inline void copy_dlstring(DlString text, char* buf, int32_t len) {
    if (len <= 0) return;
    buf[0] = 0;
    if (text == nullptr) return;
    // Convert the whole BSTR first, then truncate on a UTF-8 boundary.
    const int size = WideCharToMultiByte(CP_UTF8, 0, text,
        static_cast<int>(SysStringLen(text)), nullptr, 0, nullptr, nullptr);
    if (size <= 0) return;
    std::string utf8(static_cast<size_t>(size), '\0');
    if (WideCharToMultiByte(CP_UTF8, 0, text, static_cast<int>(SysStringLen(text)),
        utf8.data(), size, nullptr, nullptr) != size) return;
    size_t copy = std::min(utf8.size(), static_cast<size_t>(len - 1));
    while (copy < utf8.size() && copy > 0 &&
           (static_cast<unsigned char>(utf8[copy]) & 0xc0) == 0x80) --copy;
    std::memcpy(buf, utf8.data(), copy);
    buf[copy] = 0;
}

inline void free_dlstring(DlString text) { SysFreeString(text); }

inline IDeckLinkDiscovery* dl_create_discovery() {
    if (!dl_thread_ready()) return nullptr;
    IDeckLinkDiscovery* discovery = nullptr;
    CoCreateInstance(CLSID_CDeckLinkDiscovery, nullptr, CLSCTX_INPROC_SERVER,
        IID_IDeckLinkDiscovery, reinterpret_cast<void**>(&discovery));
    return discovery;
}

inline IDeckLinkAPIInformation* dl_create_api_information() {
    if (!dl_thread_ready()) return nullptr;
    IDeckLinkAPIInformation* information = nullptr;
    CoCreateInstance(CLSID_CDeckLinkAPIInformation, nullptr, CLSCTX_INPROC_SERVER,
        IID_IDeckLinkAPIInformation, reinterpret_cast<void**>(&information));
    return information;
}

inline bool dl_is_unknown(REFIID iid) { return IsEqualIID(iid, IID_IUnknown); }
inline bool dl_same_iid(REFIID a, REFIID b) { return IsEqualIID(a, b); }

// Discovery is owned by a permanent MTA, independent of a UI thread's STA
// and of a short-lived API caller. All output workers join that same MTA.
// Initialization completes before a caller reads the discovered devices.
template<class Start> void dl_start_discovery(Start start) {
    std::promise<void> ready;
    auto waiting = ready.get_future();
    std::thread([start, ready = std::move(ready)]() mutable {
        if (dl_thread_ready()) start();
        ready.set_value();
        // Process lifetime, like the discovery and known-device storage.
        std::mutex lock;
        std::condition_variable forever;
        std::unique_lock<std::mutex> held(lock);
        forever.wait(held, [] { return false; });
    }).detach();
    waiting.get();
}

#else
#include <Accelerate/Accelerate.h>
#include <CoreFoundation/CFPlugInCOM.h>
#include <CoreFoundation/CoreFoundation.h>
#include "DeckLinkAPI.h"

#define NIDA_COM
using DlString = CFStringRef;
using DlSupported = bool;
inline bool dl_thread_ready() { return true; }
inline void copy_dlstring(DlString text, char* buf, int32_t len) {
    if (len <= 0) return;
    buf[0] = 0;
    if (text != nullptr) CFStringGetCString(text, buf, len, kCFStringEncodingUTF8);
}
inline void free_dlstring(DlString text) { if (text != nullptr) CFRelease(text); }
inline IDeckLinkDiscovery* dl_create_discovery() { return CreateDeckLinkDiscoveryInstance(); }
inline IDeckLinkAPIInformation* dl_create_api_information() {
    return CreateDeckLinkAPIInformationInstance();
}
inline bool dl_same_iid(REFIID a, REFIID b) {
    return std::memcmp(&a, &b, sizeof(REFIID)) == 0;
}
inline bool dl_is_unknown(REFIID iid) {
    return dl_same_iid(iid, CFUUIDGetUUIDBytes(IUnknownUUID));
}
template<class Start> void dl_start_discovery(Start start) { start(); }
#endif
