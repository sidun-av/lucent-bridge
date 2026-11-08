/* Minimal Direct3D 11 program for the end-to-end tests.
 *
 * Opens a 640x480 window, creates a D3D11 device + swap chain, compiles two
 * tiny HLSL shaders at run time (D3DCompile from d3dcompiler_47) and draws a
 * green triangle on a dark-blue background every frame. The window title
 * carries the DXGI adapter description and feature level, so the test can
 * see which GPU/driver actually rendered it.
 *
 * Build: x86_64-w64-mingw32-gcc -O2 -mwindows -o d3d11_sample.exe \
 *            d3d11_sample.c -ld3d11 -ldxgi -luuid
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <d3dcompiler.h>
#include <stdio.h>

typedef HRESULT(WINAPI *compile_fn)(LPCVOID, SIZE_T, LPCSTR, const D3D_SHADER_MACRO *,
                                    ID3DInclude *, LPCSTR, LPCSTR, UINT, UINT, ID3DBlob **,
                                    ID3DBlob **);

static const char shader_src[] =
    "float4 vs(uint id : SV_VertexID) : SV_Position {\n"
    "  float2 p[3] = { float2(0.0, 0.6), float2(0.6, -0.6), float2(-0.6, -0.6) };\n"
    "  return float4(p[id], 0.0, 1.0);\n"
    "}\n"
    "float4 ps() : SV_Target { return float4(0.125, 0.75, 0.25, 1.0); }\n";

static LRESULT CALLBACK wndproc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_CLOSE || m == WM_DESTROY) {
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(h, m, w, l);
}

static int fail(const char *what, HRESULT hr)
{
    char msg[256];
    snprintf(msg, sizeof msg, "%s failed: 0x%08lx", what, (unsigned long)hr);
    fprintf(stderr, "d3d11_sample: %s\n", msg);
    return 1;
}

int WINAPI WinMain(HINSTANCE inst, HINSTANCE prev, LPSTR cmd, int show)
{
    (void)prev, (void)cmd;
    WNDCLASSA wc = {0};
    wc.lpfnWndProc = wndproc;
    wc.hInstance = inst;
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.lpszClassName = "LucentD3D11Sample";
    RegisterClassA(&wc);
    RECT r = {0, 0, 640, 480};
    AdjustWindowRect(&r, WS_OVERLAPPEDWINDOW, FALSE);
    HWND hwnd = CreateWindowA(wc.lpszClassName, "LB D3D11 sample", WS_OVERLAPPEDWINDOW, 0, 0,
                              r.right - r.left, r.bottom - r.top, NULL, NULL, inst, NULL);
    ShowWindow(hwnd, show ? show : SW_SHOW);

    DXGI_SWAP_CHAIN_DESC sd = {0};
    sd.BufferCount = 2;
    sd.BufferDesc.Width = 640;
    sd.BufferDesc.Height = 480;
    sd.BufferDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.OutputWindow = hwnd;
    sd.SampleDesc.Count = 1;
    sd.Windowed = TRUE;
    sd.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
    ID3D11Device *dev;
    ID3D11DeviceContext *ctx;
    IDXGISwapChain *sc;
    D3D_FEATURE_LEVEL fl;
    HRESULT hr = D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, NULL, 0,
                                               D3D11_SDK_VERSION, &sd, &sc, &dev, &fl, &ctx);
    if (FAILED(hr))
        return fail("D3D11CreateDeviceAndSwapChain", hr);

    char title[256] = "LB D3D11 sample";
    IDXGIDevice *xdev;
    IDXGIAdapter *adapter;
    DXGI_ADAPTER_DESC ad;
    if (SUCCEEDED(ID3D11Device_QueryInterface(dev, &IID_IDXGIDevice, (void **)&xdev)) &&
        SUCCEEDED(IDXGIDevice_GetAdapter(xdev, &adapter)) &&
        SUCCEEDED(IDXGIAdapter_GetDesc(adapter, &ad))) {
        snprintf(title, sizeof title, "LB D3D11 sample - %ls - FL %x.%x", ad.Description,
                 (fl >> 12) & 0xf, (fl >> 8) & 0xf);
    }
    SetWindowTextA(hwnd, title);
    printf("%s\n", title);
    fflush(stdout);

    HMODULE dc = LoadLibraryA("d3dcompiler_47.dll");
    compile_fn compile = dc ? (compile_fn)(void *)GetProcAddress(dc, "D3DCompile") : NULL;
    if (!compile)
        return fail("loading D3DCompile", E_FAIL);
    ID3DBlob *vsb, *psb, *err = NULL;
    hr = compile(shader_src, sizeof shader_src - 1, "s", NULL, NULL, "vs", "vs_4_0", 0, 0, &vsb,
                 &err);
    if (FAILED(hr))
        return fail("compiling vs", hr);
    hr = compile(shader_src, sizeof shader_src - 1, "s", NULL, NULL, "ps", "ps_4_0", 0, 0, &psb,
                 &err);
    if (FAILED(hr))
        return fail("compiling ps", hr);
    ID3D11VertexShader *vs;
    ID3D11PixelShader *ps;
    ID3D11Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(vsb),
                                    ID3D10Blob_GetBufferSize(vsb), NULL, &vs);
    ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(psb),
                                   ID3D10Blob_GetBufferSize(psb), NULL, &ps);

    ID3D11Texture2D *bb;
    ID3D11RenderTargetView *rtv;
    hr = IDXGISwapChain_GetBuffer(sc, 0, &IID_ID3D11Texture2D, (void **)&bb);
    if (FAILED(hr))
        return fail("GetBuffer", hr);
    ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)bb, NULL, &rtv);
    D3D11_VIEWPORT vp = {0, 0, 640, 480, 0, 1};
    const float clear[4] = {0.05f, 0.10f, 0.40f, 1.0f};

    for (;;) {
        MSG msg;
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT)
                return 0;
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, NULL);
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, clear);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        IDXGISwapChain_Present(sc, 1, 0);
        Sleep(16);
    }
}
