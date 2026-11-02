/* SPDX-License-Identifier: Apache-2.0 */
#ifndef FIRECRAB_WORKSPACE_PROCESS_H
#define FIRECRAB_WORKSPACE_PROCESS_H
#include <stdint.h>
#include <stddef.h>
#include <sys/types.h>
uint32_t fcp_n32(const unsigned char *p);
uint64_t fcp_n64(const unsigned char *p);
void fcp_p32(unsigned char *p, uint32_t n);
void fcp_p64(unsigned char *p, uint64_t n);
int fcp_io_all(int fd, void *buf, size_t length, int writing);
int fcp_dir_at(int parent, const char *name, mode_t mode, int create);
int fcp_file_at(int dir, const char *name, int flags);
int fcp_read_exact(int dir, const char *name, void *bytes, size_t n);
int fcp_atomic_record(int dir, const char *name, const void *data, size_t size);
uint64_t fcp_ticks(pid_t pid);
int fcp_boot_id(unsigned char id[16]);
int fcp_terminate(uid_t uid);
int fcp_reserve_uid(int marker, const unsigned char id[16], uid_t *uid);
int fcp_group_prepare(int root, int marker, const char *project, gid_t *gid);
void fcp_close_except(int a, int b, int c, int d, int e, int f);
#endif
