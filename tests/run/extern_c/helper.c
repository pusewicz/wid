#include <stdint.h>

int32_t wid_double(int32_t x);

int32_t helper_add(int32_t a, int32_t b) { return a + b; }

int32_t call_back_into_wid(int32_t x) { return wid_double(x) + 1; }
