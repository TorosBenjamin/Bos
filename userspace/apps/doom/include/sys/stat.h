#ifndef _SYS_STAT_H
#define _SYS_STAT_H

#include <sys/types.h>

struct stat {
    unsigned long st_size;
    unsigned int  st_mode;
};

#define S_ISDIR(m)  (((m) & 0xF000) == 0x4000)
#define S_ISREG(m)  (((m) & 0xF000) == 0x8000)
#define S_IRWXU 0700
#define S_IRWXG 0070
#define S_IRWXO 0007

int mkdir(const char *path, mode_t mode);
int stat(const char *path, struct stat *buf);

#endif /* _SYS_STAT_H */
