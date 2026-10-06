/* Interoperability probe uses the installed libei, independently of Roost's reis. */
#include <libei.h>
#include <poll.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    struct ei *ei = ei_new_sender(NULL);
    if (!ei || ei_setup_backend_fd(ei, atoi(argv[1])) != 0) return 3;
    bool keyboard = false, pointer = false;
    for (int iteration = 0; iteration < 100; iteration++) {
        ei_dispatch(ei);
        struct ei_event *event;
        while ((event = ei_get_event(ei))) {
            enum ei_event_type type = ei_event_get_type(event);
            if (type == EI_EVENT_DISCONNECT) return 4;
            if (type == EI_EVENT_SEAT_ADDED)
                ei_seat_bind_capabilities(ei_event_get_seat(event), EI_DEVICE_CAP_KEYBOARD,
                    EI_DEVICE_CAP_POINTER_ABSOLUTE, EI_DEVICE_CAP_BUTTON, NULL);
            if (type == EI_EVENT_DEVICE_RESUMED) {
                struct ei_device *device = ei_event_get_device(event);
                ei_device_start_emulating(device, 1);
                if (!pointer && ei_device_has_capability(device, EI_DEVICE_CAP_POINTER_ABSOLUTE)
                    && ei_device_has_capability(device, EI_DEVICE_CAP_BUTTON)) {
                    ei_device_pointer_motion_absolute(device, 640, 400);
                    ei_device_frame(device, ei_now(ei));
                    ei_device_button_button(device, 0x110, true);
                    ei_device_frame(device, ei_now(ei));
                    ei_device_button_button(device, 0x110, false);
                    ei_device_frame(device, ei_now(ei));
                    pointer = true;
                }
                if (!keyboard && ei_device_has_capability(device, EI_DEVICE_CAP_KEYBOARD)) {
                    ei_device_keyboard_key(device, 30, true);
                    ei_device_frame(device, ei_now(ei));
                    ei_device_keyboard_key(device, 30, false);
                    ei_device_frame(device, ei_now(ei));
                    keyboard = true;
                }
            }
            ei_event_unref(event);
        }
        if (keyboard && pointer) {
            /* Keep the connection alive until all frames have been dispatched. */
            for (int i = 0; i < 10; i++) { ei_dispatch(ei); usleep(20000); }
            puts("libei keyboard and absolute pointer/button frames sent");
            ei_unref(ei);
            return 0;
        }
        struct pollfd fd = {.fd = ei_get_fd(ei), .events = POLLIN};
        poll(&fd, 1, 100);
    }
    return 5;
}
