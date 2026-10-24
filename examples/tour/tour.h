/* A small C library for the tour's `cimport`: header-only, so nothing else
 * needs building. */
#ifndef TOUR_H
#define TOUR_H

#define TOUR_VERSION "1.0"
#define TOUR_MAX_ITEMS 16

/** A point on the plane; the tour maps it to its own `Vec2`. */
typedef struct tour_Point {
    float x;
    float y;
} tour_Point;

/** Called once per value by `tour_sum_mapped`. */
typedef int (*tour_Visit)(int value);

/** The greatest common divisor of two non-negative numbers. */
static inline int tour_gcd(int a, int b) {
    while (b != 0) {
        int t = a % b;
        a = b;
        b = t;
    }
    return a;
}

/** The point halfway between `a` and `b`. */
static inline tour_Point tour_midpoint(tour_Point a, tour_Point b) {
    return (tour_Point){ (a.x + b.x) / 2.0f, (a.y + b.y) / 2.0f };
}

/** The sum of `visit(xs[i])` for the first `n` values. */
static inline int tour_sum_mapped(const int *xs, int n, tour_Visit visit) {
    int sum = 0;
    for (int i = 0; i < n; i++) {
        sum += visit(xs[i]);
    }
    return sum;
}

#endif
