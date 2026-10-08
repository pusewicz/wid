#ifndef GEO_H
#define GEO_H

/// The area of a circle with radius `r`.
///
/// Negative radii give the area of the positive one.
double circle_area(double r);

/** A rectangle. */
typedef struct GeoRect {
    float w; ///< The width.
    float h;
} GeoRect;

#define GEO_VERSION 3

#endif
