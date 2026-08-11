/* Dynamically-linked test */
#include <stdio.h>
#include <unistd.h>
int main(void) {
    printf("HELLO DYNAMIC FROM USERSPACE\n");
    fflush(stdout);
    write(2, "DYN STDERR OK\n", 14);
    return 0;
}
