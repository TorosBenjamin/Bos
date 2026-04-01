#ifndef _STRING_H
#define _STRING_H

#include <stddef.h>

size_t strlen(const char *s);
char  *strcpy(char *dst, const char *src);
char  *strncpy(char *dst, const char *src, size_t n);
int    strcmp(const char *s1, const char *s2);
int    strncmp(const char *s1, const char *s2, size_t n);
int    strcasecmp(const char *s1, const char *s2);
int    strncasecmp(const char *s1, const char *s2, size_t n);
char  *strcat(char *dst, const char *src);
char  *strncat(char *dst, const char *src, size_t n);
char  *strchr(const char *s, int c);
char  *strrchr(const char *s, int c);
char  *strdup(const char *s);
char  *strstr(const char *haystack, const char *needle);
char  *strtok(char *s, const char *delim);
char  *strerror(int errnum);

void  *memset(void *s, int c, size_t n);
void  *memcpy(void *dst, const void *src, size_t n);
void  *memmove(void *dst, const void *src, size_t n);
int    memcmp(const void *s1, const void *s2, size_t n);

#endif /* _STRING_H */
