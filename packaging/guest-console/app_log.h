/* SPDX-License-Identifier: Apache-2.0 */
#ifndef FIRECRAB_APP_LOG_H
#define FIRECRAB_APP_LOG_H

/* Supervise argv with separate stdout/stderr pipes; telemetry cannot block it. */
int firecrab_app_log_run(int argc, char **argv);

#endif
