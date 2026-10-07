#ifndef SHAPES_H
#define SHAPES_H

#include <stdint.h>

typedef struct sh_Point {
    float x;
    float y;
} sh_Point;

/* C decides the padding between these fields. */
typedef struct sh_Packet {
    char tag;
    double weight;
    short count;
    sh_Point points[3];
    const char *label;
    int vertexCount;
} sh_Packet;

/* Every field of a union starts at offset 0. */
typedef union sh_Value {
    int64_t i;
    double d;
    char bytes[12];
} sh_Value;

/* A struct with a tag but no typedef. */
struct sh_pair {
    int a;
    char b;
};

/* Only C knows what is inside. */
typedef struct sh_Hidden sh_Hidden;

sh_Hidden *sh_hidden_get(void);
int sh_pair_sum(struct sh_pair p);

#endif
