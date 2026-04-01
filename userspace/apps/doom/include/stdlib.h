#ifndef _STDLIB_H
#define _STDLIB_H

#include <stddef.h>

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
#define RAND_MAX 0x7FFFFFFF

void  *malloc(size_t size);
void   free(void *ptr);
void  *realloc(void *ptr, size_t size);
void  *calloc(size_t count, size_t size);

int    atoi(const char *s);
long   atol(const char *s);
double atof(const char *s);
long   strtol(const char *s, char **endptr, int base);
unsigned long strtoul(const char *s, char **endptr, int base);

int    abs(int n);
long   labs(long n);

void   exit(int status) __attribute__((noreturn));
int    atexit(void (*func)(void));

void   qsort(void *base, size_t count, size_t size,
             int (*cmp)(const void *, const void *));

char  *getenv(const char *name);
int    system(const char *cmd);
int    mkdir(const char *path, unsigned int mode);

int    rand(void);
void   srand(unsigned int seed);

#endif /* _STDLIB_H */
