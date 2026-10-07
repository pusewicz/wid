#ifndef TINY_H
#define TINY_H

#ifndef TINY_SCALE
#define TINY_SCALE 1
#endif

#define TINY_FACTOR (TINY_SCALE * 1)

int tiny_scaled(int x);

#endif

#ifdef TINY_IMPLEMENTATION
int tiny_scaled(int x) { return x * TINY_SCALE; }
#endif
