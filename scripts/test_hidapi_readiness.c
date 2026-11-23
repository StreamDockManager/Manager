/* Synthetic backend test only: never enumerates/opens hardware or runs IOKit. */
#include "../vendor/.cache/sources/hidapi-2.6.7/etc/hidapi/mac/hid.c"
#include <assert.h>
#include <stdatomic.h>

static atomic_int calls;
static atomic_int finish;
static void ready(void *context) {
    hid_device *dev = context;
    /* Registration and delivery must retain the same input mutex. */
    assert(pthread_mutex_trylock(&dev->mutex) == EBUSY);
    atomic_fetch_add(&calls, 1);
}
static void *deliver(void *context) {
    hid_device *dev = context;
    while (!atomic_load(&finish)) {
        pthread_mutex_lock(&dev->mutex);
        notify_input_ready(dev);
        pthread_mutex_unlock(&dev->mutex);
    }
    return NULL;
}
int main(void) {
    hid_device *dev = new_hid_device();
    assert(dev);
    hid_darwin_set_input_ready_callback(dev, ready, dev);
    assert(atomic_load(&calls) == 0);
    uint8_t report[] = { 1, 2, 3 };
    hid_report_callback(dev, 0, NULL, kIOHIDReportTypeInput, 0, report, 3);
    assert(atomic_load(&calls) == 1);
    /* Notification never consumes or alters the queued report. */
    assert(dev->input_reports && dev->input_reports->len == 3);
    assert(memcmp(dev->input_reports->data, report, 3) == 0);
    hid_darwin_set_input_ready_callback(dev, NULL, NULL);
    hid_darwin_set_input_ready_callback(dev, ready, dev);
    assert(atomic_load(&calls) == 2); /* Already queued data is signaled. */
    pthread_mutex_lock(&dev->mutex);
    return_data(dev, NULL, 0);
    dev->disconnected = 1;
    pthread_mutex_unlock(&dev->mutex);
    hid_darwin_set_input_ready_callback(dev, ready, dev);
    assert(atomic_load(&calls) == 3); /* Already disconnected is signaled. */
    dev->disconnected = 0;
    dev->shutdown_thread = 1;
    hid_darwin_set_input_ready_callback(dev, ready, dev);
    assert(atomic_load(&calls) == 4); /* Already shutting down is signaled. */
    pthread_t emitter;
    assert(pthread_create(&emitter, NULL, deliver, dev) == 0);
    /* Synchronous unregister permits freeing callback state on return. */
    hid_darwin_set_input_ready_callback(dev, NULL, NULL);
    int retired = atomic_load(&calls);
    atomic_store(&finish, 1);
    pthread_join(emitter, NULL);
    assert(atomic_load(&calls) == retired);
    free_hid_device(dev);
    return 0;
}
