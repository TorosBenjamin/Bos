/*
 * bos_libc.c — minimal C library helpers for Doom on Bos OS
 *
 * Provides printf-family functions (which need va_list in C).
 * All other libc symbols are implemented in Rust (main.rs).
 */
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>

/* Forward declaration of the Rust-implemented vsnprintf helper */
/* We implement vsnprintf here in C to avoid Rust va_list complexity */

static void buf_putc(char **p, char *end, char c)
{
    if (*p < end) *(*p)++ = c;
}

static int buf_puts(char **p, char *end, const char *s, int width, int left)
{
    if (!s) s = "(null)";
    int len = 0;
    const char *t = s;
    while (*t++) len++;

    int pad = (width > len) ? (width - len) : 0;

    if (!left)
        for (int i = 0; i < pad; i++) buf_putc(p, end, ' ');

    for (int i = 0; i < len; i++) buf_putc(p, end, s[i]);

    if (left)
        for (int i = 0; i < pad; i++) buf_putc(p, end, ' ');

    return (pad + len);
}

static int buf_putn(char **p, char *end, unsigned long val, int base,
                    int upper, int is_signed, int width, int zero_pad, int prec)
{
    char tmp[24];
    int n = 0, neg = 0;
    unsigned long uval;

    if (is_signed && (long)val < 0) { neg = 1; uval = (unsigned long)(-(long)val); }
    else uval = val;

    if (uval == 0) { tmp[n++] = '0'; }
    while (uval > 0) {
        int d = (int)(uval % (unsigned long)base);
        tmp[n++] = (char)(d < 10 ? '0' + d : (upper ? 'A' : 'a') + d - 10);
        uval /= (unsigned long)base;
    }

    /* Precision for integers = minimum number of digits; pad with leading zeros */
    int prec_pad = (prec > n) ? (prec - n) : 0;

    int digits = n + neg + prec_pad;
    int pad = (width > digits) ? (width - digits) : 0;
    char pc = zero_pad ? '0' : ' ';

    if (neg && zero_pad) buf_putc(p, end, '-');
    for (int i = 0; i < pad; i++) buf_putc(p, end, pc);
    if (neg && !zero_pad) buf_putc(p, end, '-');
    for (int i = 0; i < prec_pad; i++) buf_putc(p, end, '0');
    for (int i = n - 1; i >= 0; i--) buf_putc(p, end, tmp[i]);

    return digits + pad;
}

int vsnprintf(char *buf, size_t size, const char *fmt, va_list args)
{
    if (!buf || size == 0) return 0;
    char *p   = buf;
    char *end = buf + size - 1; /* leave room for NUL */
    int total = 0;

    while (*fmt) {
        if (*fmt != '%') {
            buf_putc(&p, end, *fmt++);
            total++;
            continue;
        }
        fmt++; /* skip '%' */

        /* Flags */
        int left = 0, zero = 0, plus = 0;
        while (*fmt == '-' || *fmt == '0' || *fmt == '+' || *fmt == ' ' || *fmt == '#') {
            if (*fmt == '-') left = 1;
            else if (*fmt == '0') zero = 1;
            else if (*fmt == '+') plus = 1;
            fmt++;
        }
        /* Width */
        int width = 0;
        if (*fmt == '*') { width = va_arg(args, int); fmt++; }
        else while (*fmt >= '0' && *fmt <= '9') { width = width * 10 + (*fmt++ - '0'); }
        /* Precision (ignored beyond skipping) */
        int prec = -1;
        if (*fmt == '.') {
            fmt++;
            prec = 0;
            if (*fmt == '*') { prec = va_arg(args, int); fmt++; }
            else while (*fmt >= '0' && *fmt <= '9') { prec = prec * 10 + (*fmt++ - '0'); }
        }
        /* Length modifier */
        int is_long = 0, is_longlong = 0;
        if (*fmt == 'l') { is_long = 1; fmt++; }
        if (*fmt == 'l') { is_longlong = 1; fmt++; }
        if (*fmt == 'h') fmt++;
        if (*fmt == 'h') fmt++;
        (void)is_longlong;

        switch (*fmt) {
        case 'd': case 'i': {
            long v = is_long ? va_arg(args, long) : (long)va_arg(args, int);
            total += buf_putn(&p, end, (unsigned long)v, 10, 0, 1, width, zero && !left, prec);
            break;
        }
        case 'u': {
            unsigned long v = is_long ? va_arg(args, unsigned long) : (unsigned long)va_arg(args, unsigned int);
            total += buf_putn(&p, end, v, 10, 0, 0, width, zero && !left, prec);
            break;
        }
        case 'x': {
            unsigned long v = is_long ? va_arg(args, unsigned long) : (unsigned long)va_arg(args, unsigned int);
            total += buf_putn(&p, end, v, 16, 0, 0, width, zero && !left, prec);
            break;
        }
        case 'X': {
            unsigned long v = is_long ? va_arg(args, unsigned long) : (unsigned long)va_arg(args, unsigned int);
            total += buf_putn(&p, end, v, 16, 1, 0, width, zero && !left, prec);
            break;
        }
        case 'o': {
            unsigned long v = is_long ? va_arg(args, unsigned long) : (unsigned long)va_arg(args, unsigned int);
            total += buf_putn(&p, end, v, 8, 0, 0, width, zero && !left, prec);
            break;
        }
        case 'p': {
            unsigned long v = (unsigned long)(uintptr_t)va_arg(args, void *);
            buf_putc(&p, end, '0'); buf_putc(&p, end, 'x');
            total += 2 + buf_putn(&p, end, v, 16, 0, 0, 0, 0, 0);
            break;
        }
        case 's': {
            const char *s = va_arg(args, const char *);
            total += buf_puts(&p, end, s, width, left);
            break;
        }
        case 'c': {
            char c = (char)va_arg(args, int);
            buf_putc(&p, end, c);
            total++;
            break;
        }
        case '%':
            buf_putc(&p, end, '%');
            total++;
            break;
        default:
            buf_putc(&p, end, '%');
            buf_putc(&p, end, *fmt);
            total += 2;
            break;
        }
        if (*fmt) fmt++;
    }

    *p = '\0';
    return total;
}

int snprintf(char *buf, size_t size, const char *fmt, ...)
{
    va_list args;
    va_start(args, fmt);
    int r = vsnprintf(buf, size, fmt, args);
    va_end(args);
    return r;
}

int sprintf(char *buf, const char *fmt, ...)
{
    va_list args;
    va_start(args, fmt);
    int r = vsnprintf(buf, (size_t)65536, fmt, args);
    va_end(args);
    return r;
}

/* printf/fprintf: discard output (no stdout on Bos userspace) */
int printf(const char *fmt, ...)
{
    (void)fmt;
    return 0;
}

/* Emit error messages via the kernel debug log so we can see I_Error reasons.
 * Declared in Rust as #[no_mangle] pub extern "C" fn __doom_debug_str. */
extern void __doom_debug_str(const char *s, size_t len);

/* vfprintf: for stderr (or any non-stdout), format and emit to debug log */
int vfprintf(void *stream, const char *fmt, va_list args)
{
    /* Always emit so we can see I_Error messages */
    static char errbuf[256];
    int n = vsnprintf(errbuf, sizeof(errbuf), fmt, args);
    if (n > 0)
        __doom_debug_str(errbuf, (size_t)(n < 255 ? n : 255));
    return n;
}

int fprintf(void *stream, const char *fmt, ...)
{
    (void)stream; (void)fmt;
    return 0;
}

int puts(const char *s) { (void)s; return 0; }
int putchar(int c) { return c; }

/* sscanf — minimal: handles %d, %s, %x */
int sscanf(const char *str, const char *fmt, ...)
{
    va_list args;
    va_start(args, fmt);
    int count = 0;
    const char *s = str;

    while (*fmt && *s) {
        if (*fmt != '%') {
            if (*fmt == *s) { fmt++; s++; }
            else break;
            continue;
        }
        fmt++;
        switch (*fmt) {
        case 'd': case 'i': {
            int *dst = va_arg(args, int *);
            while (*s == ' ' || *s == '\t') s++;
            int neg = 0, v = 0;
            if (*s == '-') { neg = 1; s++; }
            else if (*s == '+') s++;
            while (*s >= '0' && *s <= '9') { v = v * 10 + (*s++ - '0'); }
            *dst = neg ? -v : v;
            count++;
            break;
        }
        case 's': {
            char *dst = va_arg(args, char *);
            while (*s == ' ' || *s == '\t') s++;
            while (*s && *s != ' ' && *s != '\t' && *s != '\n') *dst++ = *s++;
            *dst = '\0';
            count++;
            break;
        }
        default:
            /* unsupported specifier — stop */
            goto done;
        }
        fmt++;
    }
done:
    va_end(args);
    return count;
}
