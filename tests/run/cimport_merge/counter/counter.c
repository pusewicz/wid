#include "counter.h"

void CounterReset(Counter *c) {
    c->count = 0;
    c->limit = COUNTER_LIMIT;
}

int CounterStep(Counter *c) {
    if (c->count < c->limit) {
        c->count += 1;
    }
    return c->count;
}
