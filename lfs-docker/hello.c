#include <unistd.h>
int main(void) {
    write(1, "HELLO FROM USERSPACE\n", 21);
    write(2, "STDERR TEST\n", 12);
    return 0;
}
