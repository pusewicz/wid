/* miniaudio's implementation, compiled once with every program that imports
   vendor:miniaudio. GCC can't parse the block syntax in Apple's CoreAudio
   headers, so GCC builds on macOS go without the CoreAudio backend. */
#if defined(__APPLE__) && defined(__GNUC__) && !defined(__clang__)
#define MA_NO_COREAUDIO
#endif
#define MINIAUDIO_IMPLEMENTATION
#include "miniaudio.h"
