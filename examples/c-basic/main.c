#include <assert.h>
#include <stdio.h>
static int add(int a, int b) { return a + b; }
int main(void) {
    assert(add(10, 20) == 30);
    assert(add(-5, 2) == -3);
    puts("C arithmetic checks passed");
    return 0;
}
