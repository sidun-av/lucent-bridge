/*
 * lucent-start.exe: start a Windows program the way Windows Explorer starts
 * it for an administrator with UAC on, i.e. with the *filtered* token.
 *
 *   wine lucent-start.exe "C:\Program Files\App\app.exe" [args...]
 *
 * Why: Wine gives every process a token in which BUILTIN\Administrators is
 * an enabled group, even the "limited" (TokenElevationTypeLimited) token
 * that processes started from Linux get (server/token.c,
 * token_create_admin). CheckTokenMembership(Administrators) is therefore
 * TRUE for everything, so programs that ask "am I elevated?" that way (the
 * MSDN pattern, used e.g. by Notepad++) think they run as Administrator and
 * say so in their title bar. On Windows, an administrator's normal token has
 * the group as deny-only; only an elevated process has it enabled.
 *
 * This helper makes the same filtered token with CreateRestrictedToken
 * (Administrators -> SE_GROUP_USE_FOR_DENY_ONLY) and starts the program with
 * it. Nothing else changes: the token keeps its elevation type, so a program
 * whose manifest asks for requireAdministrator / highestAvailable is still
 * elevated by Wine's loader through the linked (full) token, as on Windows.
 * Installers are not started through this helper at all (lucent-bridge
 * decides), so they keep the token they always had.
 *
 * If anything fails, the program is started normally: being shown as
 * "Administrator" is better than not starting.
 *
 * The exit code is the program's. GUI subsystem: no console is created.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include <windows.h>

/* Skip one command-line argument (Windows quoting rules, as used by
 * CommandLineToArgvW for argv[0]) and the blanks after it. */
static WCHAR *skip_argument(WCHAR *p)
{
    BOOL quoted = FALSE;

    while (*p == L' ' || *p == L'\t') p++;
    for (; *p; p++)
    {
        if (*p == L'"') quoted = !quoted;
        else if (!quoted && (*p == L' ' || *p == L'\t')) break;
    }
    while (*p == L' ' || *p == L'\t') p++;
    return p;
}

static HANDLE filtered_token(void)
{
    SID_IDENTIFIER_AUTHORITY nt = {SECURITY_NT_AUTHORITY};
    HANDLE token = NULL, filtered = NULL;
    PSID admins = NULL;
    SID_AND_ATTRIBUTES disable;

    if (!OpenProcessToken(GetCurrentProcess(),
                          TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT
                              | TOKEN_ADJUST_SESSIONID,
                          &token))
        return NULL;
    if (AllocateAndInitializeSid(&nt, 2, SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_ADMINS, 0, 0, 0, 0,
                                 0, 0, &admins))
    {
        disable.Sid = admins;
        disable.Attributes = 0;
        if (!CreateRestrictedToken(token, 0, 1, &disable, 0, NULL, 0, NULL, &filtered))
            filtered = NULL;
        FreeSid(admins);
    }
    CloseHandle(token);
    return filtered;
}

int WINAPI wWinMain(HINSTANCE instance, HINSTANCE previous, LPWSTR unused, int show)
{
    WCHAR *command = skip_argument(GetCommandLineW());
    STARTUPINFOW si;
    PROCESS_INFORMATION pi;
    HANDLE token;
    DWORD code = 1;
    BOOL started = FALSE;

    (void)instance;
    (void)previous;
    (void)unused;
    (void)show;
    if (!*command) return ERROR_INVALID_PARAMETER;

    /* Pass on how we were asked to show the window. */
    GetStartupInfoW(&si);
    si.cb = sizeof(si);
    si.lpReserved = NULL;
    si.lpDesktop = NULL;
    si.lpTitle = NULL;
    si.cbReserved2 = 0;
    si.lpReserved2 = NULL;
    /* GUI programs only; nothing is inherited (Wine's own diagnostics go
     * to the Unix stderr regardless). */
    si.dwFlags &= ~STARTF_USESTDHANDLES;
    si.hStdInput = si.hStdOutput = si.hStdError = NULL;

    if ((token = filtered_token()))
    {
        started = CreateProcessAsUserW(token, NULL, command, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi);
        CloseHandle(token);
    }
    if (!started && !CreateProcessW(NULL, command, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi))
        return GetLastError() ? (int)GetLastError() : 1;

    CloseHandle(pi.hThread);
    WaitForSingleObject(pi.hProcess, INFINITE);
    if (!GetExitCodeProcess(pi.hProcess, &code)) code = 1;
    CloseHandle(pi.hProcess);
    return (int)code;
}
