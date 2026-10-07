#include "include/opts.h"

const int opts_version = 3;

int opts_GetFPS(void) { return 60; }
int opts_getFps(void) { return 30; }
double opts_ApplyVolume(opts_Settings settings, double sample) { return settings.volumeLevel * sample; }
