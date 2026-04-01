#ifndef _MATH_H
#define _MATH_H

/* Minimal math.h stub — doom only includes this for legacy reasons */
#define M_PI 3.14159265358979323846

static inline double fabs(double x) { return x < 0.0 ? -x : x; }
static inline double floor(double x) { return (double)(long long)x; }
static inline double ceil(double x)  {
    long long t = (long long)x;
    return (x > (double)t) ? (double)(t + 1) : (double)t;
}
static inline double sqrt(double x) {
    if (x <= 0.0) return 0.0;
    double g = x / 2.0;
    for (int i = 0; i < 64; i++) g = (g + x / g) / 2.0;
    return g;
}
static inline double atan2(double y, double x) {
    (void)y; (void)x; return 0.0; /* not needed by doom gameplay */
}

#endif /* _MATH_H */
