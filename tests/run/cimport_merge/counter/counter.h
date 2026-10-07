#ifndef COUNTER_H
#define COUNTER_H

#define COUNTER_LIMIT 3

typedef struct Counter {
    int count;
    int limit;
} Counter;

void CounterReset(Counter *c);
int CounterStep(Counter *c);

#endif
