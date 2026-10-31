/* SPDX-License-Identifier: Apache-2.0 */
#ifndef FIRECRAB_WORKSPACE_H
#define FIRECRAB_WORKSPACE_H
int firecrab_workspace_files(int argc, char **argv);
int firecrab_workspace_open(int argc, char **argv, int *root, int *marker, int *lock);
int firecrab_workspace_directory(int root, const char *path);
int firecrab_workspace_terminal(int argc, char **argv);
#endif
