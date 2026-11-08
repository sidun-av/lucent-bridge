/* Console program for first-run timing: prints who and where it runs. */
#include <stdio.h>
#include <stdlib.h>

int main(void)
{
    printf("hello from Windows: USERNAME=%s USERPROFILE=%s\n", getenv("USERNAME"),
           getenv("USERPROFILE"));
    return 0;
}
