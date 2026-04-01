#ifndef _UNISTD_H
#define _UNISTD_H

#include <stddef.h>

/* Minimal unistd stub for freestanding environment */
#define STDIN_FILENO  0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2

int isatty(int fd);
int fileno(void *stream);

#endif /* _UNISTD_H */
