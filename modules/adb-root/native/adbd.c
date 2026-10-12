// adb-root module wrapper, executed from the stock adbd APEX.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char *args[argc + 2];
    int count = 0;
    args[count++] = argv[0];
    for (int i = 1; i < argc; ++i) {
        if (strncmp(argv[i], "--root_seclabel=", 16) == 0) continue;
        if (strcmp(argv[i], "--root_seclabel") == 0) {
            if (i + 1 < argc) ++i;
            continue;
        }
        args[count++] = argv[i];
    }
    args[count++] = "--root_seclabel=u:r:egysk:s0";
    args[count] = NULL;
    const char *operation = "set LD_PRELOAD";
    if (setenv("LD_PRELOAD", "/apex/com.android.adbd/bin/libadb-root.so", 1) == 0) {
        operation = "exec /apex/com.android.adbd/bin/adbd.real";
        execv("/apex/com.android.adbd/bin/adbd.real", args);
    }
    int error = errno;
    int kmsg = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
    if (kmsg >= 0) {
        dprintf(kmsg, "adb-root: wrapper %s failed: %s (errno=%d)\n",
                operation, strerror(error), error);
        close(kmsg);
    }
    errno = error;
    perror(operation);
    return 127;
}
