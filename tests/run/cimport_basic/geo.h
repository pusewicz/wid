#ifndef GEO_H
#define GEO_H

#include <stddef.h>
#include <stdint.h>

#define GEO_VERSION "1.2"
#define GEO_MAX_SHAPES 64
#define GEO_SCALE 2.5f
#define GEO_ORIGIN ((geo_Point){ 0.0f, 0.0f })
#define GEO_TWICE(x) ((x) * 2)

/** A point on the plane. */
typedef struct geo_Point {
    float x;
    float y;
} geo_Point;

typedef enum geo_Kind {
    GEO_KIND_CIRCLE = 1,
    GEO_KIND_SQUARE = 2,
} geo_Kind;

typedef struct geo_Shape {
    geo_Kind kind;
    geo_Point center;
    float size;
    const char *name;
    int tags[4];
    unsigned visible : 1;
} geo_Shape;

typedef union geo_Value {
    int64_t i;
    double d;
} geo_Value;

/* Hidden state, only reachable through a pointer. */
typedef struct geo_World geo_World;

typedef float (*geo_Measure)(const geo_Shape *shape);

geo_World *geo_world_new(void);
void geo_world_free(geo_World *world);
int geo_world_add(geo_World *world, geo_Shape shape);
size_t geo_world_count(const geo_World *world);
float geo_world_total(const geo_World *world, geo_Measure measure);
geo_Measure geo_default_measure(void);

geo_Point geo_point_add(geo_Point a, geo_Point b);
void geo_point_scale(geo_Point *p, float by);
const char *geo_kind_name(geo_Kind kind);
int geo_format(char *out, size_t size, const char *fmt, ...);
double geo_value_sum(geo_Value v, int as_int);

static inline int geo_square(int x) { return x * x; }

#endif
