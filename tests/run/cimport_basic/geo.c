#include "geo.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

struct geo_World {
    geo_Shape shapes[GEO_MAX_SHAPES];
    size_t count;
};

geo_World *geo_world_new(void) { return calloc(1, sizeof(geo_World)); }

void geo_world_free(geo_World *world) { free(world); }

int geo_world_add(geo_World *world, geo_Shape shape) {
    if (world->count == GEO_MAX_SHAPES) return -1;
    world->shapes[world->count] = shape;
    return (int)world->count++;
}

size_t geo_world_count(const geo_World *world) { return world->count; }

float geo_world_total(const geo_World *world, geo_Measure measure) {
    float total = 0.0f;
    for (size_t i = 0; i < world->count; i++) total += measure(&world->shapes[i]);
    return total;
}

static float geo_size_of(const geo_Shape *shape) { return shape->size; }

geo_Measure geo_default_measure(void) { return geo_size_of; }

geo_Point geo_point_add(geo_Point a, geo_Point b) { return (geo_Point){ a.x + b.x, a.y + b.y }; }

void geo_point_scale(geo_Point *p, float by) {
    p->x *= by;
    p->y *= by;
}

const char *geo_kind_name(geo_Kind kind) { return kind == GEO_KIND_CIRCLE ? "circle" : "square"; }

int geo_format(char *out, size_t size, const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    int n = vsnprintf(out, size, fmt, args);
    va_end(args);
    return n;
}

double geo_value_sum(geo_Value v, int as_int) { return as_int ? (double)v.i : v.d; }
