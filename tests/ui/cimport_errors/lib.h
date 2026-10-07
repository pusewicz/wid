#ifndef LIB_H
#define LIB_H

#define LIB_TWICE(x) ((x) * 2)

typedef struct lib_Pair { float a; float b; } lib_Pair;
typedef struct lib_Handle lib_Handle;
typedef struct lib_Flags { unsigned on : 1; int count; } lib_Flags;

double lib_log(double x);
void lib_Log(const char *message);
int lib_print(const char *fmt, ...);
lib_Handle *lib_open(void);

#endif
