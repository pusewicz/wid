#ifndef OPTS_H
#define OPTS_H

typedef struct opts_Settings {
    int frameRate;
    double volumeLevel;
} opts_Settings;

extern const int opts_version;

int opts_GetFPS(void);
int opts_getFps(void);
double opts_ApplyVolume(opts_Settings settings, double sample);

#endif
