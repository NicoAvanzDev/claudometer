#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shellapi.h>
#include <wtsapi32.h>
#include <string>
#include <atomic>
#include <d2d1.h>
#include <dwrite.h>
#include <filesystem>
#include <fstream>
#include <mutex>
#include <regex>
#include <sstream>
#include <thread>
#include <vector>
#include <winhttp.h>
#include "Resource.h"

#pragma comment(lib, "d2d1.lib")
#pragma comment(lib, "dwrite.lib")
#pragma comment(lib, "winhttp.lib")
#pragma comment(lib, "wtsapi32.lib")

ID2D1Factory* g_d2dFactory = nullptr;

IDWriteFactory* g_dwFactory = nullptr;
IDWriteTextFormat* g_textFormat = nullptr;
IDWriteTextFormat* g_smallTextFormat = nullptr;
IDWriteTextFormat* g_percentTextFormat = nullptr;

struct WidgetWindow
{
    HWND hwnd = nullptr;
    HWND taskbar = nullptr;
    ID2D1HwndRenderTarget* rt = nullptr;
    ID2D1SolidColorBrush* bgBrush = nullptr;
    ID2D1SolidColorBrush* textBrush = nullptr;
    ID2D1SolidColorBrush* mutedTextBrush = nullptr;
    ID2D1SolidColorBrush* trackBrush = nullptr;
    ID2D1SolidColorBrush* sessionBrush = nullptr;
    ID2D1SolidColorBrush* weeklyBrush = nullptr;
};

static std::vector<WidgetWindow*> g_widgets;
static HICON g_claudeCodeIcon = nullptr;
static HWINEVENTHOOK g_foregroundHook = nullptr;
static constexpr UINT_PTR TIMER_ID = 1;
static constexpr UINT WM_USAGE_UPDATED = WM_APP + 1;
static constexpr DWORD USAGE_POLL_INTERVAL_MS = 5 * 60 * 1000;
static constexpr DWORD TIMER_INTERVAL_MS = 60 * 1000;
static constexpr int WIDGET_WIDTH = 164;
static constexpr int WIDGET_HEIGHT = 36;
static constexpr float SESSION_ROW_TOP = -0.5f;
static constexpr float WEEKLY_ROW_TOP = 18.0f;

struct UsageSnapshot
{
    int sessionPercent = 0;
    int weeklyPercent = 0;
    int sessionResetMinutes = 0;
    int weeklyResetMinutes = 0;
    std::wstring status = L"starting";
    bool ok = false;
};

static UsageSnapshot g_usage;
static std::mutex g_usageMutex;
static std::atomic_bool g_fetchInFlight = false;
static std::atomic_bool g_shutdown = false;
static int g_widgetCount = 0;
static DWORD g_lastFetchTick = 0;
static std::thread g_fetchThread;

static std::string ReadFileToString(const std::filesystem::path& path)
{
    std::ifstream file(path, std::ios::binary);
    if (!file)
        return {};

    std::ostringstream buffer;
    buffer << file.rdbuf();
    return buffer.str();
}

static std::wstring GetEnvString(const wchar_t* name)
{
    DWORD size = GetEnvironmentVariableW(name, nullptr, 0);
    if (size == 0)
        return {};

    std::wstring value(size, L'\0');
    DWORD written = GetEnvironmentVariableW(name, value.data(), size);
    if (written == 0)
        return {};

    value.resize(written);
    return value;
}

static std::vector<std::filesystem::path> CredentialCandidates()
{
    if (auto overridePath = GetEnvString(L"CLAUDE_CREDENTIALS_PATH"); !overridePath.empty())
        return { overridePath };

    if (auto configDir = GetEnvString(L"CLAUDE_CONFIG_DIR"); !configDir.empty())
        return { std::filesystem::path(configDir) / L".credentials.json" };

    std::wstring home = GetEnvString(L"USERPROFILE");
    if (home.empty())
        return {};

    std::vector<std::filesystem::path> paths;
    paths.push_back(std::filesystem::path(home) / L".claude" / L".credentials.json");

    if (auto localAppData = GetEnvString(L"LOCALAPPDATA"); !localAppData.empty())
        paths.push_back(std::filesystem::path(localAppData) / L"Claude" / L".credentials.json");

    if (auto appData = GetEnvString(L"APPDATA"); !appData.empty())
        paths.push_back(std::filesystem::path(appData) / L"Claude" / L".credentials.json");

    return paths;
}

static std::string ExtractAccessToken(const std::string& blob)
{
    static const std::regex tokenRegex(R"ccusage("accessToken"\s*:\s*"([^"]+)")ccusage");
    std::smatch match;
    if (std::regex_search(blob, match, tokenRegex) && match.size() > 1)
        return match[1].str();

    static const std::regex rawTokenRegex(R"(^[A-Za-z0-9_\-.~+/=]{20,}$)");
    if (std::regex_match(blob, rawTokenRegex))
        return blob;

    return {};
}

static std::string ReadClaudeToken()
{
    for (const auto& path : CredentialCandidates())
    {
        std::string token = ExtractAccessToken(ReadFileToString(path));
        if (!token.empty())
            return token;
    }

    return {};
}

static std::wstring ToWideAscii(const std::string& text)
{
    return std::wstring(text.begin(), text.end());
}

static std::wstring QueryResponseHeader(HINTERNET request, const wchar_t* headerName)
{
    DWORD size = 0;
    WinHttpQueryHeaders(
        request,
        WINHTTP_QUERY_CUSTOM,
        headerName,
        WINHTTP_NO_OUTPUT_BUFFER,
        &size,
        WINHTTP_NO_HEADER_INDEX
    );

    if (GetLastError() != ERROR_INSUFFICIENT_BUFFER || size == 0)
        return {};

    std::wstring value(size / sizeof(wchar_t), L'\0');
    if (!WinHttpQueryHeaders(
        request,
        WINHTTP_QUERY_CUSTOM,
        headerName,
        value.data(),
        &size,
        WINHTTP_NO_HEADER_INDEX))
    {
        return {};
    }

    value.resize(size / sizeof(wchar_t));
    while (!value.empty() && value.back() == L'\0')
        value.pop_back();
    return value;
}

static int PercentFromHeader(const std::wstring& value)
{
    if (value.empty())
        return 0;

    wchar_t* end = nullptr;
    double utilization = wcstod(value.c_str(), &end);
    if (end == value.c_str())
        return 0;

    int percent = static_cast<int>(utilization * 100.0 + 0.5);
    if (percent < 0) return 0;
    if (percent > 999) return 999;
    return percent;
}

static int ResetMinutesFromHeader(const std::wstring& value)
{
    if (value.empty())
        return 0;

    wchar_t* end = nullptr;
    double resetAt = wcstod(value.c_str(), &end);
    if (end == value.c_str())
        return 0;

    FILETIME ft;
    GetSystemTimeAsFileTime(&ft);
    ULARGE_INTEGER nowTicks{};
    nowTicks.LowPart = ft.dwLowDateTime;
    nowTicks.HighPart = ft.dwHighDateTime;

    constexpr unsigned long long windowsToUnixTicks = 116444736000000000ULL;
    double now = static_cast<double>(nowTicks.QuadPart - windowsToUnixTicks) / 10000000.0;
    double minutes = (resetAt - now) / 60.0;
    return minutes > 0 ? static_cast<int>(minutes + 0.5) : 0;
}

static bool QueryClaudeUsage(UsageSnapshot& snapshot)
{
    const std::string token = ReadClaudeToken();
    if (token.empty())
    {
        snapshot.status = L"no token";
        snapshot.ok = false;
        return false;
    }

    HINTERNET session = WinHttpOpen(
        L"CCTaskBarUsage/1.0",
        WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
        WINHTTP_NO_PROXY_NAME,
        WINHTTP_NO_PROXY_BYPASS,
        0
    );
    if (!session)
    {
        snapshot.status = L"http init";
        snapshot.ok = false;
        return false;
    }

    WinHttpSetTimeouts(session, 5000, 5000, 10000, 20000);

    HINTERNET connect = WinHttpConnect(session, L"api.anthropic.com", INTERNET_DEFAULT_HTTPS_PORT, 0);
    if (!connect)
    {
        WinHttpCloseHandle(session);
        snapshot.status = L"http connect";
        snapshot.ok = false;
        return false;
    }

    HINTERNET request = WinHttpOpenRequest(
        connect,
        L"POST",
        L"/v1/messages",
        nullptr,
        WINHTTP_NO_REFERER,
        WINHTTP_DEFAULT_ACCEPT_TYPES,
        WINHTTP_FLAG_SECURE
    );
    if (!request)
    {
        WinHttpCloseHandle(connect);
        WinHttpCloseHandle(session);
        snapshot.status = L"http request";
        snapshot.ok = false;
        return false;
    }

    const char body[] =
        "{\"model\":\"claude-haiku-4-5-20251001\","
        "\"max_tokens\":1,"
        "\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}";

    std::wstring headers =
        L"anthropic-version: 2023-06-01\r\n"
        L"anthropic-beta: oauth-2025-04-20\r\n"
        L"content-type: application/json\r\n"
        L"user-agent: claude-code/2.1.5\r\n"
        L"authorization: Bearer " + ToWideAscii(token) + L"\r\n";

    BOOL sent = WinHttpSendRequest(
        request,
        headers.c_str(),
        static_cast<DWORD>(headers.length()),
        const_cast<char*>(body),
        static_cast<DWORD>(sizeof(body) - 1),
        static_cast<DWORD>(sizeof(body) - 1),
        0
    );

    BOOL received = sent ? WinHttpReceiveResponse(request, nullptr) : FALSE;
    if (!received)
    {
        WinHttpCloseHandle(request);
        WinHttpCloseHandle(connect);
        WinHttpCloseHandle(session);
        snapshot.status = L"http failed";
        snapshot.ok = false;
        return false;
    }

    DWORD statusCode = 0;
    DWORD statusSize = sizeof(statusCode);
    WinHttpQueryHeaders(
        request,
        WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
        WINHTTP_HEADER_NAME_BY_INDEX,
        &statusCode,
        &statusSize,
        WINHTTP_NO_HEADER_INDEX
    );

    if (statusCode == 401 || statusCode == 403)
    {
        snapshot.status = L"login";
        snapshot.ok = false;
    }
    else if (statusCode >= 400)
    {
        snapshot.status = L"api " + std::to_wstring(statusCode);
        snapshot.ok = false;
    }
    else
    {
        std::wstring sessionUtil = QueryResponseHeader(request, L"anthropic-ratelimit-unified-5h-utilization");
        std::wstring sessionReset = QueryResponseHeader(request, L"anthropic-ratelimit-unified-5h-reset");
        std::wstring weeklyUtil = QueryResponseHeader(request, L"anthropic-ratelimit-unified-7d-utilization");
        std::wstring weeklyReset = QueryResponseHeader(request, L"anthropic-ratelimit-unified-7d-reset");
        std::wstring status = QueryResponseHeader(request, L"anthropic-ratelimit-unified-5h-status");

        snapshot.sessionPercent = PercentFromHeader(sessionUtil);
        snapshot.weeklyPercent = PercentFromHeader(weeklyUtil);
        snapshot.sessionResetMinutes = ResetMinutesFromHeader(sessionReset);
        snapshot.weeklyResetMinutes = ResetMinutesFromHeader(weeklyReset);
        snapshot.status = status.empty() ? L"ok" : status;
        snapshot.ok = true;
    }

    WinHttpCloseHandle(request);
    WinHttpCloseHandle(connect);
    WinHttpCloseHandle(session);
    return snapshot.ok;
}

static void UpdateUsageState(const UsageSnapshot& snapshot)
{
    {
        std::lock_guard<std::mutex> lock(g_usageMutex);
        g_usage = snapshot;
    }

    if (!g_shutdown)
    {
        for (WidgetWindow* widget : g_widgets)
            if (widget && widget->hwnd)
                PostMessageW(widget->hwnd, WM_USAGE_UPDATED, 0, 0);
    }
}

static bool IsWorkstationLocked()
{
    HDESK inputDesktop = OpenInputDesktop(0, FALSE, DESKTOP_SWITCHDESKTOP);
    if (!inputDesktop)
        return true;

    BOOL switchable = SwitchDesktop(inputDesktop);
    CloseDesktop(inputDesktop);
    return switchable == FALSE;
}

static void StartUsageFetchIfDue(bool force = false)
{
    if (IsWorkstationLocked())
        return;

    DWORD now = GetTickCount();
    if (!force && g_lastFetchTick != 0 && now - g_lastFetchTick < USAGE_POLL_INTERVAL_MS)
        return;

    bool expected = false;
    if (!g_fetchInFlight.compare_exchange_strong(expected, true))
        return;

    g_lastFetchTick = now;

    if (g_fetchThread.joinable())
        g_fetchThread.join();

    g_fetchThread = std::thread([]()
        {
            UsageSnapshot snapshot;
            QueryClaudeUsage(snapshot);
            UpdateUsageState(snapshot);
            g_fetchInFlight = false;
        });
}

static HRESULT CreateGraphicsResources(WidgetWindow* widget)
{
    if (widget->rt)
        return S_OK;

    RECT rc;
    GetClientRect(widget->hwnd, &rc);

    D2D1_SIZE_U size = D2D1::SizeU(
        rc.right - rc.left,
        rc.bottom - rc.top
    );

    HRESULT hr = g_d2dFactory->CreateHwndRenderTarget(
        D2D1::RenderTargetProperties(),
        D2D1::HwndRenderTargetProperties(widget->hwnd, size),
        &widget->rt
    );

    if (FAILED(hr)) return hr;

    widget->rt->CreateSolidColorBrush(D2D1::ColorF(0.12f, 0.12f, 0.12f, 1.0f), &widget->bgBrush);
    widget->rt->CreateSolidColorBrush(D2D1::ColorF(D2D1::ColorF::White), &widget->textBrush);
    widget->rt->CreateSolidColorBrush(D2D1::ColorF(0.70f, 0.70f, 0.70f, 1.0f), &widget->mutedTextBrush);
    widget->rt->CreateSolidColorBrush(D2D1::ColorF(0.24f, 0.24f, 0.24f, 1.0f), &widget->trackBrush);
    widget->rt->CreateSolidColorBrush(D2D1::ColorF(0.93f, 0.53f, 0.27f, 1.0f), &widget->sessionBrush);
    widget->rt->CreateSolidColorBrush(D2D1::ColorF(0.36f, 0.66f, 0.95f, 1.0f), &widget->weeklyBrush);

    return S_OK;
}

static void DrawUsageRow(WidgetWindow* widget, const wchar_t* label, int percent, float top, ID2D1SolidColorBrush* fillBrush, float width)
{
    std::wstring percentText = std::to_wstring(percent) + L"%";
    D2D1_RECT_F labelRect = D2D1::RectF(45.0f, top, 74.0f, top + 13.0f);
    D2D1_RECT_F percentRect = D2D1::RectF(76.0f, top, width - 8.0f, top + 13.0f);
    widget->rt->DrawTextW(label, static_cast<UINT32>(wcslen(label)), g_textFormat, labelRect, widget->textBrush);
    widget->rt->DrawTextW(percentText.c_str(), static_cast<UINT32>(percentText.length()), g_percentTextFormat, percentRect, widget->textBrush);

    D2D1_RECT_F track = D2D1::RectF(45.0f, top + 14.5f, width - 8.0f, top + 17.5f);
    widget->rt->FillRoundedRectangle(D2D1::RoundedRect(track, 1.5f, 1.5f), widget->trackBrush);

    float clamped = static_cast<float>(percent);
    if (clamped < 0.0f) clamped = 0.0f;
    if (clamped > 100.0f) clamped = 100.0f;
    float barWidth = (track.right - track.left) * (clamped / 100.0f);
    D2D1_RECT_F fill = D2D1::RectF(track.left, track.top, track.left + barWidth, track.bottom);
    widget->rt->FillRoundedRectangle(D2D1::RoundedRect(fill, 1.5f, 1.5f), fillBrush);
}

static void DiscardGraphicsResources(WidgetWindow* widget)
{
    if (widget->weeklyBrush) { widget->weeklyBrush->Release(); widget->weeklyBrush = nullptr; }
    if (widget->sessionBrush) { widget->sessionBrush->Release(); widget->sessionBrush = nullptr; }
    if (widget->trackBrush) { widget->trackBrush->Release(); widget->trackBrush = nullptr; }
    if (widget->mutedTextBrush) { widget->mutedTextBrush->Release(); widget->mutedTextBrush = nullptr; }
    if (widget->textBrush) { widget->textBrush->Release(); widget->textBrush = nullptr; }
    if (widget->bgBrush) { widget->bgBrush->Release(); widget->bgBrush = nullptr; }
    if (widget->rt) { widget->rt->Release(); widget->rt = nullptr; }
}

static void OnPaint(HWND hwnd)
{
    WidgetWindow* widget = reinterpret_cast<WidgetWindow*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
    if (!widget)
        return;

    PAINTSTRUCT ps;
    BeginPaint(hwnd, &ps);

    if (SUCCEEDED(CreateGraphicsResources(widget)))
    {
        UsageSnapshot snapshot;
        {
            std::lock_guard<std::mutex> lock(g_usageMutex);
            snapshot = g_usage;
        }

        widget->rt->BeginDraw();

        auto size = widget->rt->GetSize();
        widget->rt->FillRectangle(D2D1::RectF(0, 0, size.width, size.height), widget->bgBrush);

        if (snapshot.ok)
        {
            DrawUsageRow(widget, L"5h", snapshot.sessionPercent, SESSION_ROW_TOP, widget->sessionBrush, size.width);
            DrawUsageRow(widget, L"7d", snapshot.weeklyPercent, WEEKLY_ROW_TOP, widget->weeklyBrush, size.width);
        }
        else
        {
            std::wstring text = L"Claude " + snapshot.status;
            widget->rt->DrawTextW(
                text.c_str(),
                static_cast<UINT32>(text.length()),
                g_smallTextFormat,
                D2D1::RectF(45.0f, 0.0f, size.width - 6.0f, size.height),
                widget->mutedTextBrush
            );
        }

        HRESULT hr = widget->rt->EndDraw();
        if (hr == D2DERR_RECREATE_TARGET)
        {
            DiscardGraphicsResources(widget);
            InvalidateRect(hwnd, nullptr, FALSE);
        }
        else if (SUCCEEDED(hr) && g_claudeCodeIcon)
        {
            DrawIconEx(ps.hdc, 4, 2, g_claudeCodeIcon, 32, 32, 0, nullptr, DI_NORMAL);
        }
    }

    EndPaint(hwnd, &ps);
}

static void PositionOverTaskbar(WidgetWindow* widget, bool restoreTopmost)
{
    RECT rc{};
    GetWindowRect(widget->taskbar, &rc);

    int taskbarWidth = rc.right - rc.left;
    int taskbarHeight = rc.bottom - rc.top;
    int x = rc.left + 8;
    int y = rc.top + ((taskbarHeight - WIDGET_HEIGHT) / 2);

    if (taskbarWidth < taskbarHeight)
    {
        x = rc.left + ((taskbarWidth - WIDGET_WIDTH) / 2);
        y = rc.top + 8;
    }

    SetWindowPos(
        widget->hwnd,
        restoreTopmost ? HWND_TOPMOST : nullptr,
        x,
        y,
        WIDGET_WIDTH,
        WIDGET_HEIGHT,
        SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW | (restoreTopmost ? 0 : SWP_NOZORDER)
    );
}

static bool IsTaskbarWindow(HWND hwnd)
{
    wchar_t className[64]{};
    GetClassNameW(hwnd, className, static_cast<int>(sizeof(className) / sizeof(className[0])));
    return wcscmp(className, L"Shell_TrayWnd") == 0 || wcscmp(className, L"Shell_SecondaryTrayWnd") == 0;
}

static void RestoreWidgetsAboveTaskbars()
{
    for (WidgetWindow* widget : g_widgets)
        if (widget && widget->hwnd && IsWindow(widget->hwnd))
            PositionOverTaskbar(widget, true);
}

static void CALLBACK ForegroundWinEventProc(
    HWINEVENTHOOK,
    DWORD,
    HWND hwnd,
    LONG,
    LONG,
    DWORD,
    DWORD)
{
    for (WidgetWindow* widget : g_widgets)
        if (widget && widget->hwnd == hwnd)
            return;

    if (hwnd)
        RestoreWidgetsAboveTaskbars();
}

static LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    switch (msg)
    {
    case WM_NCCREATE:
    {
        auto create = reinterpret_cast<CREATESTRUCTW*>(lp);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
        return TRUE;
    }

    case WM_TIMER:
        StartUsageFetchIfDue();
        return 0;

    case WM_WTSSESSION_CHANGE:
        if (wp == WTS_SESSION_UNLOCK)
            StartUsageFetchIfDue(true);
        return 0;

    case WM_USAGE_UPDATED:
        InvalidateRect(hwnd, nullptr, TRUE);
        return 0;

    case WM_PAINT:
        OnPaint(hwnd);
        return 0;

    case WM_DESTROY:
        KillTimer(hwnd, TIMER_ID);
        WTSUnRegisterSessionNotification(hwnd);

        if (WidgetWindow* widget = reinterpret_cast<WidgetWindow*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA)))
            DiscardGraphicsResources(widget);

        if (--g_widgetCount == 0)
        {
            g_shutdown = true;
            if (g_foregroundHook)
            {
                UnhookWinEvent(g_foregroundHook);
                g_foregroundHook = nullptr;
            }

            if (g_fetchThread.joinable())
                g_fetchThread.join();

            if (g_smallTextFormat) g_smallTextFormat->Release();
            if (g_percentTextFormat) g_percentTextFormat->Release();
            if (g_textFormat) g_textFormat->Release();
            if (g_dwFactory) g_dwFactory->Release();
            if (g_d2dFactory) g_d2dFactory->Release();
            if (g_claudeCodeIcon) DestroyIcon(g_claudeCodeIcon);

            PostQuitMessage(0);
        }
        return 0;
    }

    return DefWindowProcW(hwnd, msg, wp, lp);
}

static BOOL CALLBACK EnumTaskbarWindows(HWND hwnd, LPARAM lp)
{
    wchar_t className[64]{};
    GetClassNameW(hwnd, className, static_cast<int>(sizeof(className) / sizeof(className[0])));

    if (wcscmp(className, L"Shell_TrayWnd") == 0 || wcscmp(className, L"Shell_SecondaryTrayWnd") == 0)
    {
        auto taskbars = reinterpret_cast<std::vector<HWND>*>(lp);
        taskbars->push_back(hwnd);
    }

    return TRUE;
}

static std::vector<HWND> FindTaskbars()
{
    std::vector<HWND> taskbars;
    EnumWindows(EnumTaskbarWindows, reinterpret_cast<LPARAM>(&taskbars));

    if (taskbars.empty())
        if (HWND primary = FindWindowW(L"Shell_TrayWnd", nullptr))
            taskbars.push_back(primary);

    return taskbars;
}

static void CreateWidgetForTaskbar(HWND taskbar, const wchar_t* cls, HINSTANCE hInst)
{
    auto widget = new WidgetWindow();
    widget->taskbar = taskbar;

    widget->hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        cls,
        L"Claude Code Usage",
        WS_POPUP,
        0, 0, WIDGET_WIDTH, WIDGET_HEIGHT,
        nullptr,
        nullptr,
        hInst,
        widget
    );

    if (!widget->hwnd)
    {
        delete widget;
        return;
    }

    g_widgets.push_back(widget);
    ++g_widgetCount;
    PositionOverTaskbar(widget, true);
    WTSRegisterSessionNotification(widget->hwnd, NOTIFY_FOR_THIS_SESSION);
    SetTimer(widget->hwnd, TIMER_ID, TIMER_INTERVAL_MS, nullptr);
}

static HRESULT InitGraphics()
{
    HRESULT hr = D2D1CreateFactory(
        D2D1_FACTORY_TYPE_SINGLE_THREADED,
        &g_d2dFactory
    );

    if (FAILED(hr)) return hr;

    hr = DWriteCreateFactory(
        DWRITE_FACTORY_TYPE_SHARED,
        __uuidof(IDWriteFactory),
        reinterpret_cast<IUnknown**>(&g_dwFactory)
    );

    if (FAILED(hr)) return hr;

    hr = g_dwFactory->CreateTextFormat(
        L"Segoe UI Variable",
        nullptr,
        DWRITE_FONT_WEIGHT_SEMI_BOLD,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        10.5f,
        L"en-us",
        &g_textFormat
    );

    if (FAILED(hr)) return hr;

    g_textFormat->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
    g_textFormat->SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

    hr = g_dwFactory->CreateTextFormat(
        L"Segoe UI Variable",
        nullptr,
        DWRITE_FONT_WEIGHT_SEMI_BOLD,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        11.0f,
        L"en-us",
        &g_smallTextFormat
    );

    if (FAILED(hr)) return hr;

    g_smallTextFormat->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
    g_smallTextFormat->SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

    hr = g_dwFactory->CreateTextFormat(
        L"Segoe UI Variable",
        nullptr,
        DWRITE_FONT_WEIGHT_SEMI_BOLD,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        10.5f,
        L"en-us",
        &g_percentTextFormat
    );

    if (FAILED(hr)) return hr;

    g_percentTextFormat->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
    g_percentTextFormat->SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

    return S_OK;
}

int WINAPI wWinMain(HINSTANCE hInst, HINSTANCE, PWSTR, int)
{
    const wchar_t* cls = L"MetricsTaskbarOverlay";

    WNDCLASSW wc{};
    wc.hInstance = hInst;
    wc.lpszClassName = cls;
    wc.lpfnWndProc = WndProc;
    wc.hCursor = LoadCursor(nullptr, IDC_ARROW);

    RegisterClassW(&wc);

    g_claudeCodeIcon = static_cast<HICON>(LoadImageW(
        hInst,
        MAKEINTRESOURCEW(IDI_CLAUDECODE),
        IMAGE_ICON,
        32,
        32,
        LR_DEFAULTCOLOR
    ));

    if (FAILED(InitGraphics()))
        return -1;

    for (HWND taskbar : FindTaskbars())
        CreateWidgetForTaskbar(taskbar, cls, hInst);

    if (g_widgets.empty())
        return -1;

    g_foregroundHook = SetWinEventHook(
        EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_FOREGROUND,
        nullptr,
        ForegroundWinEventProc,
        0,
        0,
        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS
    );

    StartUsageFetchIfDue(true);

    MSG msg{};
    while (GetMessageW(&msg, nullptr, 0, 0))
    {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    return 0;
}
