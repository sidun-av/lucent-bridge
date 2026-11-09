/* GUI test program: shows in its window title what a program sees about its
 * own rights, the two ways programs ask:
 *   admins=1    CheckTokenMembership(BUILTIN\Administrators) (Notepad++'s
 *               "[Administrator]" test)
 *   elevated=1  GetTokenInformation(TokenElevation)
 * Built twice by the E2E: as is (manifest: asInvoker) and with a manifest
 * that asks for requireAdministrator (admincheck-admin.exe). Closes on
 * WM_CLOSE or after two minutes. */
#include <windows.h>

static LRESULT CALLBACK proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_TIMER || msg == WM_CLOSE)
    {
        DestroyWindow(hwnd);
        return 0;
    }
    if (msg == WM_DESTROY)
    {
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(hwnd, msg, wp, lp);
}

int WINAPI wWinMain(HINSTANCE inst, HINSTANCE prev, LPWSTR cmd, int show)
{
    SID_IDENTIFIER_AUTHORITY nt = {SECURITY_NT_AUTHORITY};
    PSID admins;
    BOOL member = FALSE;
    TOKEN_ELEVATION elevation = {0};
    TOKEN_ELEVATION_TYPE type = 0;
    HANDLE token;
    DWORD size;
    WCHAR title[128];
    WNDCLASSW wc = {0};
    HWND hwnd;
    MSG msg;

    (void)prev;
    (void)cmd;
    if (AllocateAndInitializeSid(&nt, 2, SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_ADMINS, 0, 0, 0, 0,
                                 0, 0, &admins))
    {
        if (!CheckTokenMembership(NULL, admins, &member)) member = FALSE;
        FreeSid(admins);
    }
    if (OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token))
    {
        GetTokenInformation(token, TokenElevation, &elevation, sizeof(elevation), &size);
        GetTokenInformation(token, TokenElevationType, &type, sizeof(type), &size);
        CloseHandle(token);
    }
    wsprintfW(title, L"LB admin check: admins=%d elevated=%d type=%d", member ? 1 : 0,
              elevation.TokenIsElevated ? 1 : 0, (int)type);

    wc.lpfnWndProc = proc;
    wc.hInstance = inst;
    wc.lpszClassName = L"LbAdminCheck";
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    RegisterClassW(&wc);
    hwnd = CreateWindowW(L"LbAdminCheck", title, WS_OVERLAPPEDWINDOW, 40, 40, 520, 160, NULL, NULL, inst,
                         NULL);
    ShowWindow(hwnd, show);
    SetTimer(hwnd, 1, 120000, NULL);
    while (GetMessageW(&msg, NULL, 0, 0) > 0)
    {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    return member ? 11 : 10;
}
