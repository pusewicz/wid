#ifndef VEC_H
#define VEC_H

/// A vector in the plane.
typedef struct Vec {
    double x; ///< Across.
    double y; ///< Down.
} Vec;

/// The length of `v`.
double vec_length(Vec v);

#define VEC_DIMENSIONS 2

#endif
