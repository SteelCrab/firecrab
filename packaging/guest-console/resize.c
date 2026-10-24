/* SPDX-License-Identifier: Apache-2.0 */
#include "resize.h"
#include <errno.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

static int transfer(int fd, unsigned char *bytes, size_t length, int writing) {
    size_t done = 0;
    while (done < length) {
        ssize_t count = writing ? write(fd, bytes + done, length - done)
                                : read(fd, bytes + done, length - done);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) return -1;
        done += (size_t)count;
    }
    return 0;
}

int firecrab_console_resize(int stream, int tty) {
    unsigned char frame[8];
    if (transfer(stream, frame, sizeof frame, 0)) return -1;
    unsigned rows = ((unsigned)frame[4] << 8) | frame[5];
    unsigned cols = ((unsigned)frame[6] << 8) | frame[7];
    int error = 0;
    if (memcmp(frame, "FCR1", 4) || rows < 2 || rows > 1000 || cols < 2 || cols > 1000) {
        error = EINVAL;
    } else {
        struct winsize size = {0};
        size.ws_row = (unsigned short)rows;
        size.ws_col = (unsigned short)cols;
        if (ioctl(tty, TIOCSWINSZ, &size)) error = errno;
    }
    if (error) {
        memcpy(frame, "FCE1", 4);
        uint32_t code = (uint32_t)error;
        for (int index = 7; index >= 4; index--) {
            frame[index] = (unsigned char)code;
            code >>= 8;
        }
    } else {
        memcpy(frame, "FCA1", 4);
    }
    return transfer(stream, frame, sizeof frame, 1);
}
