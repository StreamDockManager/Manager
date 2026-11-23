/* Synthetic Windows events only: never enumerate/open/read any HID hardware. */
#include "../vendor/.cache/sources/hidapi-2.6.7/etc/hidapi/windows/hid.c"
#include <assert.h>
static HANDLE notified;
static volatile LONG count;
static void ready(void *context) {
    (void)context;
    InterlockedIncrement(&count);
    SetEvent(notified);
}
int main(void) {
    hid_device *dev = new_hid_device();
    assert(dev);
    notified = CreateEvent(NULL, FALSE, FALSE, NULL);
    assert(notified);
    assert(hid_winapi_set_input_ready_callback(dev, ready, NULL) == 0);
    assert(WaitForSingleObject(notified, 50) == WAIT_TIMEOUT);
    SetEvent(dev->ol.hEvent);
    assert(WaitForSingleObject(notified, 2000) == WAIT_OBJECT_0);
    /* The observer must not consume/reset the completion event. */
    assert(WaitForSingleObject(dev->ol.hEvent, 0) == WAIT_OBJECT_0);
    assert(WaitForSingleObject(dev->ol.hEvent, 0) == WAIT_OBJECT_0);
    assert(WaitForSingleObject(notified, 50) == WAIT_TIMEOUT);
    ResetEvent(dev->ol.hEvent);
    SetEvent(dev->input_ready_rearm);
    SetEvent(dev->ol.hEvent);
    assert(WaitForSingleObject(notified, 2000) == WAIT_OBJECT_0);
    assert(hid_winapi_set_input_ready_callback(dev, NULL, NULL) == 0);
    LONG retired = InterlockedCompareExchange(&count, 0, 0);
    SetEvent(dev->ol.hEvent);
    Sleep(20);
    assert(InterlockedCompareExchange(&count, 0, 0) == retired);
    assert(dev->input_ready_thread == NULL);
    CloseHandle(notified);
    free_hid_device(dev);
    return 0;
}
