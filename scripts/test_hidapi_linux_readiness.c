/* Synthetic actual-backend test: only pipes/eventfds, never enumerate/open HID. */
#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <unistd.h>
static atomic_int fail_wait;
static atomic_int interrupt_wait;
static atomic_int fail_join;
static int test_epoll_wait(int fd, struct epoll_event *events, int count, int timeout);
static int test_pthread_join(pthread_t thread, void **result);
#define epoll_wait test_epoll_wait
#define pthread_join test_pthread_join
#ifndef HIDAPI_LINUX_SOURCE
#define HIDAPI_LINUX_SOURCE "../vendor/.cache/sources/hidapi-2.6.7/etc/hidapi/linux/hid.c"
#endif
#include HIDAPI_LINUX_SOURCE
#undef epoll_wait
#undef pthread_join
static int test_epoll_wait(int fd, struct epoll_event *events, int count, int timeout) {
    if (atomic_exchange(&fail_wait, 0)) { errno = EIO; return -1; }
    if (atomic_exchange(&interrupt_wait, 0)) { errno = EINTR; return -1; }
    return epoll_wait(fd, events, count, timeout);
}
static int test_pthread_join(pthread_t thread, void **result) {
    if (atomic_exchange(&fail_join, 0)) return EINVAL;
    return pthread_join(thread, result);
}
struct signal_state { int fd; atomic_int calls; atomic_int block; atomic_int release; };
static void ready(void *context) {
    struct signal_state *state = context;
    atomic_fetch_add(&state->calls, 1);
    uint64_t one = 1;
    assert(write(state->fd, &one, sizeof(one)) == sizeof(one));
    while (atomic_load(&state->block) && !atomic_load(&state->release))
        usleep(1000); /* Test-only gated callback for unregister/join race. */
}
static void wait_ready(struct signal_state *state) {
    struct pollfd p = { .fd = state->fd, .events = POLLIN };
    assert(poll(&p, 1, 1000) == 1);
    uint64_t value;
    assert(read(state->fd, &value, sizeof(value)) == sizeof(value));
}
static struct signal_state signal_new(void) {
    struct signal_state s = { .fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK) };
    assert(s.fd >= 0);
    return s;
}
static hid_device *pipe_device(int *writer) {
    int fds[2];
    assert(pipe(fds) == 0);
    hid_device *dev = new_hid_device();
    assert(dev);
    dev->device_handle = fds[0];
    *writer = fds[1];
    return dev;
}
struct retire_state { hid_device *dev; atomic_int done; };
static void *retire(void *context) {
    struct retire_state *r = context;
    assert(hid_linux_set_input_ready_callback(r->dev, NULL, NULL) == 0);
    atomic_store(&r->done, 1);
    return NULL;
}
int main(void) {
    struct signal_state first = signal_new(), second = signal_new();
    int writer;
    hid_device *dev = pipe_device(&writer);
    unsigned char report[] = {1, 2, 3}, output[32];

    /* Existing readiness is delivered, no report is consumed by observer. */
    assert(write(writer, report, sizeof(report)) == sizeof(report));
    assert(hid_linux_set_input_ready_callback(dev, ready, &first) == 0);
    wait_ready(&first);
    int count = atomic_load(&first.calls);
    usleep(30000);
    assert(atomic_load(&first.calls) == count); /* No level-trigger spin. */
    assert(hid_read_timeout(dev, output, sizeof(output), 0) == sizeof(report));
    assert(memcmp(report, output, sizeof(report)) == 0);
    assert(hid_read_timeout(dev, output, sizeof(output), 0) == 0);

    /* EINTR is retried; replacement synchronously retires previous context. */
    assert(hid_linux_set_input_ready_callback(dev, NULL, NULL) == 0);
    atomic_store(&interrupt_wait, 1);
    assert(hid_linux_set_input_ready_callback(dev, ready, &second) == 0);
    assert(write(writer, report, sizeof(report)) == sizeof(report));
    wait_ready(&second);
    assert(atomic_load(&first.calls) == count);
    assert(hid_read_timeout(dev, output, sizeof(output), 0) == sizeof(report));
    assert(memcmp(report, output, sizeof(report)) == 0);

    /* Joined unregister is prompt while idle; no callback after it returns. */
    assert(hid_linux_set_input_ready_callback(dev, NULL, NULL) == 0);
    int retired = atomic_load(&second.calls);
    assert(write(writer, report, sizeof(report)) == sizeof(report));
    usleep(30000);
    assert(atomic_load(&second.calls) == retired);
    assert(hid_read_timeout(dev, output, sizeof(output), 0) == sizeof(report));
    for (int i = 0; i < 100; ++i) {
        assert(hid_linux_set_input_ready_callback(dev, ready, &first) == 0);
        assert(hid_linux_set_input_ready_callback(dev, NULL, NULL) == 0);
    }

    /* Unregister waits for an in-flight callback before context release. */
    atomic_store(&first.block, 1);
    assert(hid_linux_set_input_ready_callback(dev, ready, &first) == 0);
    assert(write(writer, report, sizeof(report)) == sizeof(report));
    wait_ready(&first);
    struct retire_state r = { .dev = dev };
    pthread_t retirer;
    assert(pthread_create(&retirer, NULL, retire, &r) == 0);
    usleep(30000);
    assert(!atomic_load(&r.done));
    atomic_store(&first.release, 1);
    assert(pthread_join(retirer, NULL) == 0);
    assert(atomic_load(&r.done));
    atomic_store(&first.block, 0);
    assert(hid_read_timeout(dev, output, sizeof(output), 0) == sizeof(report));

    /* A failed join retains all state for a safe retry, never frees live state. */
    assert(hid_linux_set_input_ready_callback(dev, ready, &first) == 0);
    atomic_store(&fail_join, 1);
    assert(hid_linux_set_input_ready_callback(dev, NULL, NULL) == -1);
    assert(dev->input_ready_waiter != NULL);
    assert(hid_linux_set_input_ready_callback(dev, NULL, NULL) == 0);
    assert(dev->input_ready_waiter == NULL);

    /* Fatal wait errors signal and become a read error instead of lost wake. */
    atomic_store(&fail_wait, 1);
    assert(hid_linux_set_input_ready_callback(dev, ready, &second) == 0);
    wait_ready(&second);
    assert(hid_read_timeout(dev, output, sizeof(output), 0) == -1);
    assert(hid_linux_set_input_ready_callback(dev, NULL, NULL) == 0);

    /* HUP wakes the sole reader and surfaces disconnect. */
    assert(hid_linux_set_input_ready_callback(dev, ready, &second) == 0);
    close(writer);
    wait_ready(&second);
    assert(hid_read_timeout(dev, output, sizeof(output), 0) == -1);
    hid_close(dev);

    /* Setup failure leaves no worker; the runtime may use polling fallback. */
    dev = new_hid_device();
    assert(dev);
    assert(hid_linux_set_input_ready_callback(dev, ready, &first) == -1);
    assert(dev->input_ready_waiter == NULL);
    assert(!atomic_load(&dev->input_ready_failed));
    hid_close(dev);
    close(first.fd); close(second.fd);
    puts("Linux hidraw readiness: queue, sole reader, idle, EINTR, replace, cancel, race, join-failure, wait-failure, HUP, fallback passed");
    return 0;
}
