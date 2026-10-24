/* SPDX-License-Identifier: Apache-2.0 */
#ifndef FIRECRAB_CONSOLE_RESIZE_H
#define FIRECRAB_CONSOLE_RESIZE_H

/* One fixed-size FCR1 request; replies FCA1 + dimensions or FCE1 + errno. */
int firecrab_console_resize(int stream, int tty);

#endif
