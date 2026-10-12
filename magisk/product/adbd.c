// Debug-boot wrapper, bind-mounted into the stock adbd APEX by egysk-adb.sh.
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
    if (setenv("LD_PRELOAD", "/apex/com.android.adbd/bin/libegysk-adb.so", 1) == 0)
        execv("/apex/com.android.adbd/bin/adbd.real", args);
    perror("egysk: execute real adbd");
    return 127;
}
