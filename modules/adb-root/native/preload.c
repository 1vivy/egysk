// Module-local debuggability, following tiann/adb_root's preload interface.
#include <dlfcn.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

__attribute__((constructor)) static void isolate_preload(void) {
    // Shell/exec children must not inherit the adbd-only override.
    unsetenv("LD_PRELOAD");
}

bool __android_log_is_debuggable(void) { return true; }

int property_get(const char *name, char *value, const char *fallback) {
    if (strcmp(name, "ro.debuggable") == 0 && value) {
        memcpy(value, "1", 2);
        return 1;
    }
    int (*original)(const char *, char *, const char *) = dlsym(RTLD_NEXT, "property_get");
    if (!original) abort();
    return original(name, value, fallback);
}
