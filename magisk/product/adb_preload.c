// Loaded only in the real adbd. Do not change ro.debuggable for Android itself.
#include <dlfcn.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/system_properties.h>

__attribute__((constructor)) static void isolate_preload(void) {
    // adbd's shell/exec children must not inherit our property overrides.
    unsetenv("LD_PRELOAD");
}

bool __android_log_is_debuggable(void) { return true; }

int __system_property_get(const char *name, char *value) {
    if (strcmp(name, "ro.debuggable") == 0) {
        memcpy(value, "1", 2);
        return 1;
    }
    int (*real_get)(const char *, char *) = dlsym(RTLD_NEXT, "__system_property_get");
    if (real_get) return real_get(name, value);
    value[0] = '\0';
    return 0;
}

struct callback_context {
    void (*callback)(void *, const char *, const char *, uint32_t);
    void *cookie;
};

static void property_callback(void *cookie, const char *name, const char *value, uint32_t serial) {
    struct callback_context *context = cookie;
    context->callback(context->cookie, name,
                      strcmp(name, "ro.debuggable") == 0 ? "1" : value, serial);
}

void __system_property_read_callback(const prop_info *info,
        void (*callback)(void *, const char *, const char *, uint32_t), void *cookie) {
    void (*real_read)(const prop_info *,
        void (*)(void *, const char *, const char *, uint32_t), void *) =
        dlsym(RTLD_NEXT, "__system_property_read_callback");
    if (!real_read) abort();
    struct callback_context context = {callback, cookie};
    real_read(info, property_callback, &context);
}

int property_get(const char *name, char *value, const char *fallback) {
    int length = __system_property_get(name, value);
    if (length || !fallback) return length;
    size_t size = strnlen(fallback, PROP_VALUE_MAX - 1);
    memcpy(value, fallback, size);
    value[size] = '\0';
    return (int)size;
}
