#include <unistd.h>
#include <sys/mount.h>
int main(void) {
    write(1, "L init\n", 7);
    mount("proc", "/proc", "proc", 0, NULL);
    mount("tmpfs", "/tmp", "tmpfs", 0, NULL);
    execl("/bin/dyn_test", "/bin/dyn_test", NULL);
    write(1, "dyn failed, trying static\n", 27);
    execl("/bin/hello", "/bin/hello", NULL);
    write(1, "all failed\n", 11);
    return 1;
}
