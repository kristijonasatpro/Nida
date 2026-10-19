// Exercise the real Windows COM adapter without a DeckLink driver or card.
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <string>
#include "../../src/internal/decklink/platform.hpp"

int main() {
    std::thread mta([] {
        assert(dl_thread_ready());
        assert(dl_thread_ready());
        APTTYPE apartment;
        APTTYPEQUALIFIER qualifier;
        assert(CoGetApartmentType(&apartment, &qualifier) == S_OK);
        assert(apartment == APTTYPE_MTA);
    });
    mta.join();

    std::thread sta([] {
        assert(SUCCEEDED(CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED)));
        // A conflicting apartment is refused; it must not be uninitialized
        // by the adapter. Discovery itself must still run in a separate MTA.
        assert(!dl_thread_ready());
        APTTYPE apartment;
        APTTYPEQUALIFIER qualifier;
        assert(CoGetApartmentType(&apartment, &qualifier) == S_OK);
        assert(apartment == APTTYPE_STA || apartment == APTTYPE_MAINSTA);
        bool called = false;
        dl_start_discovery([&called] {
            assert(dl_thread_ready());
            called = true;
        });
        assert(called);
        CoUninitialize();
    });
    sta.join();

    BSTR text = SysAllocString(L"\u017d\u0105sis \U0001f680");
    assert(text != nullptr);
    char buf[64];
    copy_dlstring(text, buf, sizeof(buf));
    assert(std::string(buf) == u8"\u017d\u0105sis \U0001f680");
    copy_dlstring(text, buf, 2);
    assert(buf[0] == 0); // A two-byte glyph does not fit beside the NUL.
    copy_dlstring(text, buf, 3);
    assert(std::string(buf) == u8"\u017d");
    copy_dlstring(nullptr, buf, sizeof(buf));
    assert(buf[0] == 0);
    free_dlstring(text);
}
