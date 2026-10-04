/* Regenerate sysprof-mark.syscap with the actual libsysprof-capture writer:
 * cc -std=c11 -Wall -Wextra -Werror sysprof-mark.c -o /tmp/sysprof-mark \
 *   $(pkg-config --cflags --libs sysprof-capture-4)
 * /tmp/sysprof-mark sysprof-mark.syscap
 * Recorded using libsysprof-capture 46.0; version-1 mark ABI is also in 51.0.
 */
#include <sysprof-capture.h>
int main(int argc, char **argv) {
  if (argc != 2) return 1;
  SysprofCaptureWriter *writer = sysprof_capture_writer_new(argv[1], 4096);
  if (!writer) return 2;
  if (!sysprof_capture_writer_add_mark(writer, 123456789, 2, 4242, 2000,
      "Clutter", "Clutter::FrameClock::presented()", "presentation was 5 µs earlier")) return 3;
  if (!sysprof_capture_writer_flush(writer)) return 4;
  sysprof_capture_writer_unref(writer);
  return 0;
}
