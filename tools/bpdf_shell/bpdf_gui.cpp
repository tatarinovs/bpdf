#include "bpdf_shell_common.h"
#include <commctrl.h>
#include <commdlg.h>

static const wchar_t* BPDF_GUI_CLASS = L"BpdfGuiWindow";

// Worker messages carry the run id: output of a cancelled run is dropped
#define WM_BPDF_APPEND            (WM_APP + 1) // wParam: run id, lParam: malloc'ed text
#define WM_BPDF_FINISHED          (WM_APP + 2) // wParam: exit code, lParam: run id
#define WM_BPDF_START_AUTO_ACTION (WM_APP + 3)

#ifndef WM_DPICHANGED
#define WM_DPICHANGED 0x02E0
#endif

// A single action shows its window only if it runs longer than this
#define ID_TIMER_SHOW 101
#define SHOW_DELAY_MS 500

#define ID_BTN_CLOSE   201
#define ID_BTN_FOLDER  202
#define ID_BTN_BROWSE  203
#define ID_BTN_BACK    204
#define ID_ACTION_BASE 300

enum GuiState {
    STATE_PALETTE = 0,
    STATE_RUNNING = 1,
    STATE_FINISHED = 2
};

// -----------------------------------------------------------------------------
// GUI Context & Application State
// -----------------------------------------------------------------------------

struct GuiContext {
    HWND hwnd = NULL;
    GuiState state = STATE_PALETTE;
    bool isPaletteSession = false;   // false: started for one action (menu item, TC button)
    bool filesEmbeddedInCmd = false; // the files are already in the command after "--"

    // Palette controls
    HWND hTargetTitle = NULL;
    HWND hTargetSub = NULL;
    HWND hBtnBrowse = NULL;
    HWND hActionBtns[MAX_SHELL_ACTIONS] = {};
    HIMAGELIST hActionIml[MAX_SHELL_ACTIONS] = {};
    MenuItemDef actionDefs[MAX_SHELL_ACTIONS] = {};
    size_t actionCount = 0;

    // Progress controls
    HWND hTitleStatic = NULL;
    HWND hStatusStatic = NULL;
    HWND hProgress = NULL;
    HWND hEdit = NULL;
    HWND hBtnFolder = NULL;
    HWND hBtnBack = NULL;
    HWND hBtnClose = NULL;

    HFONT hFontTitle = NULL;
    HFONT hFontSub = NULL;
    HFONT hFontUi = NULL;
    HFONT hFontEdit = NULL;
    HBRUSH hBrushEdit = NULL;
    HICON hIcon = NULL;
    int dpi = 96;

    WStringList files;
    wchar_t targetExe[MAX_PATH * 2] = {};
    StrW workDir;
    wchar_t tempManifest[MAX_PATH * 2] = {};

    GuiMode guiMode = GUI_AUTO;

    HANDLE hProcess = NULL;
    HANDLE hJob = NULL;
    HANDLE hWorkerThread = NULL;
    LONG runId = 0;

    int autoActionIndex = -1;
    bool hasCustomAction = false;
    MenuItemDef customAction = {};
    StrW customCmd;

    int scale(int val) const { return MulDiv(val, dpi, 96); }

    // Kills the running bpdf.exe with its helpers; output still in flight is dropped
    void stopWorker()
    {
        runId++;
        if (hJob) {
            TerminateJobObject(hJob, 1);
            CloseHandle(hJob);
            hJob = NULL;
        }
        if (hProcess) {
            TerminateProcess(hProcess, 1);
            CloseHandle(hProcess);
            hProcess = NULL;
        }
        if (hWorkerThread) {
            WaitForSingleObject(hWorkerThread, 5000);
            CloseHandle(hWorkerThread);
            hWorkerThread = NULL;
        }
    }

    void deleteTempManifest()
    {
        if (tempManifest[0]) {
            DeleteFileW(tempManifest);
            tempManifest[0] = L'\0';
        }
    }

    void clearPaletteButtons()
    {
        for (size_t i = 0; i < MAX_SHELL_ACTIONS; i++) {
            if (hActionBtns[i]) {
                DestroyWindow(hActionBtns[i]);
                hActionBtns[i] = NULL;
            }
            if (hActionIml[i]) {
                ImageList_Destroy(hActionIml[i]);
                hActionIml[i] = NULL;
            }
        }
        actionCount = 0;
    }

    ~GuiContext()
    {
        stopWorker();
        deleteTempManifest();
        clearPaletteButtons();
        if (hIcon) DestroyIcon(hIcon);
        if (hFontTitle) DeleteObject(hFontTitle);
        if (hFontSub) DeleteObject(hFontSub);
        if (hFontUi) DeleteObject(hFontUi);
        if (hFontEdit) DeleteObject(hFontEdit);
        if (hBrushEdit) DeleteObject(hBrushEdit);
    }
};

static void FlushPendingAppendMessages(HWND hwnd)
{
    MSG msg;
    while (PeekMessageW(&msg, hwnd, WM_BPDF_APPEND, WM_BPDF_APPEND, PM_REMOVE)) {
        free((void*)msg.lParam);
    }
}

// -----------------------------------------------------------------------------
// DPI, Fonts & Icons
// -----------------------------------------------------------------------------

static int WindowDpi(HWND hwnd)
{
    typedef UINT (WINAPI *GetDpiForWindowFn)(HWND);
    GetDpiForWindowFn getDpi = (GetDpiForWindowFn)(void*)GetProcAddress(GetModuleHandleW(L"user32.dll"), "GetDpiForWindow");
    if (getDpi) {
        UINT dpi = getDpi(hwnd);
        if (dpi) return (int)dpi;
    }
    int dpi = 96;
    HDC hdc = GetDC(NULL);
    if (hdc) {
        dpi = GetDeviceCaps(hdc, LOGPIXELSY);
        ReleaseDC(NULL, hdc);
    }
    return dpi > 0 ? dpi : 96;
}

static HFONT CreateUiFont(const GuiContext* ctx, int size, int weight, const wchar_t* face)
{
    return CreateFontW(-ctx->scale(size), 0, 0, 0, weight, FALSE, FALSE, FALSE, DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH, face);
}

// (Re)creates the fonts for ctx->dpi and assigns them to the controls
static void ApplyDpiFonts(GuiContext* ctx)
{
    HFONT old[] = { ctx->hFontTitle, ctx->hFontSub, ctx->hFontUi, ctx->hFontEdit };
    ctx->hFontTitle = CreateUiFont(ctx, 18, FW_SEMIBOLD, L"Segoe UI");
    ctx->hFontSub = CreateUiFont(ctx, 12, FW_NORMAL, L"Segoe UI");
    ctx->hFontUi = CreateUiFont(ctx, 13, FW_NORMAL, L"Segoe UI");
    ctx->hFontEdit = CreateUiFont(ctx, 12, FW_NORMAL, L"Consolas");

    struct { HWND hwnd; HFONT font; } controls[] = {
        { ctx->hTargetTitle, ctx->hFontTitle }, { ctx->hTitleStatic, ctx->hFontTitle },
        { ctx->hTargetSub, ctx->hFontSub }, { ctx->hStatusStatic, ctx->hFontUi },
        { ctx->hEdit, ctx->hFontEdit }, { ctx->hBtnClose, ctx->hFontUi },
        { ctx->hBtnFolder, ctx->hFontUi }, { ctx->hBtnBrowse, ctx->hFontUi },
        { ctx->hBtnBack, ctx->hFontUi },
    };
    for (size_t i = 0; i < ARRAYSIZE(controls); i++) {
        if (controls[i].hwnd) SendMessageW(controls[i].hwnd, WM_SETFONT, (WPARAM)controls[i].font, TRUE);
    }
    for (size_t i = 0; i < ARRAYSIZE(old); i++) {
        if (old[i]) DeleteObject(old[i]);
    }
}

static void SetWindowIconIndex(GuiContext* ctx, int index)
{
    HICON icon = NULL;
    if (!ctx->targetExe[0] || index < 0 || ExtractIconExW(ctx->targetExe, index, &icon, NULL, 1) == 0 || !icon) {
        return;
    }
    SendMessageW(ctx->hwnd, WM_SETICON, ICON_BIG, (LPARAM)icon);
    SendMessageW(ctx->hwnd, WM_SETICON, ICON_SMALL, (LPARAM)icon);
    if (ctx->hIcon) DestroyIcon(ctx->hIcon);
    ctx->hIcon = icon;
}

// -----------------------------------------------------------------------------
// Layout
// -----------------------------------------------------------------------------

static int PaletteRows(const GuiContext* ctx)
{
    int rows = (ctx->actionCount > 5) ? (int)((ctx->actionCount + 1) / 2) : (int)ctx->actionCount;
    return rows < 1 ? 1 : rows;
}

// Top of the Browse/Close row under the action buttons
static int PaletteFooterTop(const GuiContext* ctx)
{
    int rows = PaletteRows(ctx);
    return ctx->scale(72) + rows * ctx->scale(36) + (rows - 1) * ctx->scale(8) + ctx->scale(18);
}

static HDWP Place(HDWP hdwp, HWND hwnd, int x, int y, int w, int h, bool visible)
{
    if (!hdwp || !hwnd) return hdwp;
    return DeferWindowPos(hdwp, hwnd, NULL, x, y, w, h,
        SWP_NOZORDER | SWP_NOACTIVATE | (visible ? SWP_SHOWWINDOW : SWP_HIDEWINDOW));
}

// Positions every control and shows only those of the current state
static void LayoutControls(GuiContext* ctx)
{
    RECT rc;
    GetClientRect(ctx->hwnd, &rc);
    int w = rc.right - rc.left;
    int h = rc.bottom - rc.top;

    int m = ctx->scale(16);
    int btnW = ctx->scale(110);
    int btnH = ctx->scale(28);
    bool palette = ctx->state == STATE_PALETTE;
    bool finished = ctx->state == STATE_FINISHED;

    HDWP hdwp = BeginDeferWindowPos(16 + MAX_SHELL_ACTIONS);

    // Palette
    hdwp = Place(hdwp, ctx->hTargetTitle, m, m, w - m * 2, ctx->scale(24), palette);
    hdwp = Place(hdwp, ctx->hTargetSub, m, m + ctx->scale(26), w - m * 2, ctx->scale(18), palette);

    int cols = (ctx->actionCount > 5) ? 2 : 1;
    int gap = ctx->scale(8);
    int colW = (w - m * 2 - gap * (cols - 1)) / cols;
    int actionH = ctx->scale(36);
    for (size_t i = 0; i < ctx->actionCount; i++) {
        int x = m + (int)(i % cols) * (colW + gap);
        int y = ctx->scale(72) + (int)(i / cols) * (actionH + gap);
        hdwp = Place(hdwp, ctx->hActionBtns[i], x, y, colW, actionH, palette);
    }
    hdwp = Place(hdwp, ctx->hBtnBrowse, m, PaletteFooterTop(ctx), ctx->scale(140), btnH, palette);

    // Progress
    bool hasProgress = ctx->guiMode != GUI_INFO;
    int editY = ctx->scale(hasProgress ? 80 : 58);
    int bottomY = h - m - btnH;
    int editH = bottomY - editY - ctx->scale(10);
    if (editH < ctx->scale(40)) editH = ctx->scale(40);

    hdwp = Place(hdwp, ctx->hTitleStatic, m, ctx->scale(12), w - m * 2, ctx->scale(22), !palette);
    hdwp = Place(hdwp, ctx->hStatusStatic, m, ctx->scale(36), w - m * 2, ctx->scale(18), !palette);
    hdwp = Place(hdwp, ctx->hProgress, m, ctx->scale(58), w - m * 2, ctx->scale(14), hasProgress && ctx->state == STATE_RUNNING);
    hdwp = Place(hdwp, ctx->hEdit, m, editY, w - m * 2, editH, !palette);
    hdwp = Place(hdwp, ctx->hBtnBack, m, bottomY, ctx->scale(150), btnH, finished);
    hdwp = Place(hdwp, ctx->hBtnFolder, m + ctx->scale(158), bottomY, ctx->scale(130), btnH, finished);

    int closeY = palette ? PaletteFooterTop(ctx) : bottomY;
    hdwp = Place(hdwp, ctx->hBtnClose, w - m - btnW, closeY, btnW, btnH, true);

    if (hdwp) EndDeferWindowPos(hdwp);
}

// Client width 0 keeps the current one
static void SetClientSize(GuiContext* ctx, int clientW, int clientH)
{
    RECT client;
    GetClientRect(ctx->hwnd, &client);
    RECT adj = { 0, 0, clientW ? clientW : client.right, clientH };
    AdjustWindowRectEx(&adj, (DWORD)GetWindowLongPtrW(ctx->hwnd, GWL_STYLE), FALSE,
        (DWORD)GetWindowLongPtrW(ctx->hwnd, GWL_EXSTYLE));
    SetWindowPos(ctx->hwnd, NULL, 0, 0, adj.right - adj.left, adj.bottom - adj.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
}

// Client width that fits the widest action title in its column, not wider
// than the monitor; never narrower than the current window
static int PaletteClientWidth(GuiContext* ctx)
{
    int iconSize = ctx->scale(16);
    int textW = 0;
    HDC hdc = GetDC(ctx->hwnd);
    HGDIOBJ oldFont = SelectObject(hdc, ctx->hFontUi);
    for (size_t i = 0; i < ctx->actionCount; i++) {
        SIZE size;
        const wchar_t* title = ctx->actionDefs[i].title;
        if (GetTextExtentPoint32W(hdc, title, lstrlenW(title), &size) && size.cx > textW) textW = size.cx;
    }
    SelectObject(hdc, oldFont);
    ReleaseDC(ctx->hwnd, hdc);

    // Icon margins as set in LoadPaletteForFiles, the text, a right padding
    int buttonW = ctx->scale(8) + iconSize + ctx->scale(8) + textW + ctx->scale(16);
    int cols = (ctx->actionCount > 5) ? 2 : 1;
    int width = ctx->scale(16) * 2 + cols * buttonW + (cols - 1) * ctx->scale(8);

    RECT client;
    GetClientRect(ctx->hwnd, &client);
    if (width < client.right) width = client.right;

    MONITORINFO mi = { sizeof(mi) };
    if (GetMonitorInfoW(MonitorFromWindow(ctx->hwnd, MONITOR_DEFAULTTONEAREST), &mi)) {
        int maxW = (mi.rcWork.right - mi.rcWork.left) * 9 / 10;
        if (width > maxW) width = maxW;
    }
    return width;
}

static void CenterOnMonitor(HWND hwnd)
{
    RECT rc;
    GetWindowRect(hwnd, &rc);
    MONITORINFO mi = { sizeof(mi) };
    if (!GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY), &mi)) return;
    int x = mi.rcWork.left + ((mi.rcWork.right - mi.rcWork.left) - (rc.right - rc.left)) / 2;
    int y = mi.rcWork.top + ((mi.rcWork.bottom - mi.rcWork.top) - (rc.bottom - rc.top)) / 2;
    SetWindowPos(hwnd, NULL, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
}

// -----------------------------------------------------------------------------
// Background Worker Thread (Pipe Streaming)
// -----------------------------------------------------------------------------

struct WorkerArgs {
    HWND hwnd;
    HANDLE pipe;
    HANDLE process;
    LONG runId;
};

// Length of the prefix that does not end inside a UTF-8 sequence
static int Utf8CompletePrefix(const unsigned char* p, int n)
{
    int i = n - 1, back = 0;
    while (i >= 0 && back < 3 && (p[i] & 0xC0) == 0x80) { i--; back++; }
    if (i < 0) return n;
    unsigned char c = p[i];
    int need = c < 0x80 ? 1 : (c & 0xE0) == 0xC0 ? 2 : (c & 0xF0) == 0xE0 ? 3 : (c & 0xF8) == 0xF0 ? 4 : 1;
    return (n - i >= need) ? n : i;
}

// Decodes a chunk (UTF-8, or the OEM code page of console tools) with LF -> CRLF
// for the edit control and posts it to the window
static void PostOutput(const WorkerArgs& a, const char* bytes, int len)
{
    if (len <= 0) return;
    UINT codePage = CP_UTF8;
    DWORD flags = MB_ERR_INVALID_CHARS;
    int n = MultiByteToWideChar(codePage, flags, bytes, len, NULL, 0);
    if (n <= 0) {
        codePage = GetOEMCP();
        flags = 0;
        n = MultiByteToWideChar(codePage, flags, bytes, len, NULL, 0);
        if (n <= 0) return;
    }

    // Decoded into the upper half, expanded downwards: writing position 2i+1
    // never reaches the unread character n+i+1
    wchar_t* text = (wchar_t*)malloc((2 * n + 1) * sizeof(wchar_t));
    if (!text) return;
    const wchar_t* src = text + n;
    MultiByteToWideChar(codePage, flags, bytes, len, text + n, n);
    int j = 0;
    for (int i = 0; i < n; i++) {
        wchar_t c = src[i];
        if (c == L'\n' && (i == 0 || src[i - 1] != L'\r')) text[j++] = L'\r';
        text[j++] = c;
    }
    text[j] = L'\0';
    if (!PostMessageW(a.hwnd, WM_BPDF_APPEND, (WPARAM)a.runId, (LPARAM)text)) free(text);
}

// The run ends when bpdf.exe exits, not when the pipe closes: a program it
// starts (a browser window) may hold a copy of the pipe for much longer
static DWORD WINAPI WorkerThreadProc(LPVOID param)
{
    WorkerArgs a = *(WorkerArgs*)param;
    free(param);

    char buf[4096 + 4];
    int carry = 0;
    for (;;) {
        bool exited = WaitForSingleObject(a.process, 0) == WAIT_OBJECT_0;
        DWORD avail = 0;
        if (!PeekNamedPipe(a.pipe, NULL, 0, NULL, &avail, NULL)) break;
        if (avail == 0) {
            if (exited) break;
            WaitForSingleObject(a.process, 50);
            continue;
        }

        DWORD read = 0;
        if (!ReadFile(a.pipe, buf + carry, avail < 4096 ? avail : 4096, &read, NULL) || read == 0) break;
        int len = carry + (int)read;
        // An unfinished UTF-8 sequence or a CR without its LF waits for the next chunk
        int done = Utf8CompletePrefix((const unsigned char*)buf, len);
        if (done > 0 && buf[done - 1] == '\r') done--;
        PostOutput(a, buf, done);
        carry = len - done;
        memmove(buf, buf + done, carry);
    }
    PostOutput(a, buf, carry);
    CloseHandle(a.pipe);

    DWORD exitCode = 1;
    WaitForSingleObject(a.process, INFINITE);
    GetExitCodeProcess(a.process, &exitCode);
    CloseHandle(a.process);
    PostMessageW(a.hwnd, WM_BPDF_FINISHED, exitCode, (LPARAM)a.runId);
    return 0;
}

// Starts bpdf.exe with its output piped to a worker thread
static bool SpawnProcess(GuiContext* ctx, wchar_t* cmdLine, wchar_t* environment)
{
    SECURITY_ATTRIBUTES sa = { sizeof(sa), NULL, TRUE };
    HANDLE readPipe = NULL;
    HANDLE writePipe = NULL;
    if (!CreatePipe(&readPipe, &writePipe, &sa, 0)) return false;
    SetHandleInformation(readPipe, HANDLE_FLAG_INHERIT, 0);
    HANDLE nul = CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0, NULL);

    STARTUPINFOW si = { sizeof(si) };
    si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_HIDE;
    si.hStdInput = (nul != INVALID_HANDLE_VALUE) ? nul : NULL;
    si.hStdOutput = writePipe;
    si.hStdError = writePipe;

    PROCESS_INFORMATION pi = {};
    BOOL created = CreateProcessW(NULL, cmdLine, NULL, NULL, TRUE,
        CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT, environment,
        ctx->workDir.empty() ? NULL : ctx->workDir.c_str(), &si, &pi);
    CloseHandle(writePipe);
    if (nul != INVALID_HANDLE_VALUE) CloseHandle(nul);
    if (!created) {
        CloseHandle(readPipe);
        return false;
    }

    // Cancelling takes bpdf's helpers (FFmpeg, PowerShell, Office) down with it;
    // a browser window it opens for the user leaves the job (BREAKAWAY_OK)
    ctx->hJob = CreateJobObjectW(NULL, NULL);
    if (ctx->hJob) {
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits = {};
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        SetInformationJobObject(ctx->hJob, JobObjectExtendedLimitInformation, &limits, sizeof(limits));
        AssignProcessToJobObject(ctx->hJob, pi.hProcess);
    }
    ResumeThread(pi.hThread);
    CloseHandle(pi.hThread);
    ctx->hProcess = pi.hProcess;

    WorkerArgs* args = (WorkerArgs*)malloc(sizeof(WorkerArgs));
    if (args && DuplicateHandle(GetCurrentProcess(), pi.hProcess, GetCurrentProcess(), &args->process,
                                0, FALSE, DUPLICATE_SAME_ACCESS)) {
        args->hwnd = ctx->hwnd;
        args->pipe = readPipe;
        args->runId = ctx->runId;
        ctx->hWorkerThread = CreateThread(NULL, 0, WorkerThreadProc, args, 0, NULL);
        if (ctx->hWorkerThread) return true;
        CloseHandle(args->process);
    }
    free(args);
    CloseHandle(readPipe);
    ctx->stopWorker();
    return false;
}

// "NAME=value\0" entries of vars, then the rest of our environment
static bool BuildEnvironment(const StrW& vars, StrW& out)
{
    wchar_t* parent = GetEnvironmentStringsW();
    const wchar_t* e = parent;
    while (e && *e) e += wcslen(e) + 1;
    size_t parentLen = e ? (size_t)(e - parent) : 0;

    // One allocation: a reallocation would leave copies of the passwords behind
    bool ok = out.reserve(vars.len + parentLen + 1);
    if (ok) {
        out.append(vars.data, vars.len);
        for (e = parent; e && *e; e += wcslen(e) + 1) {
            bool overridden = false;
            for (const wchar_t* v = vars.data; v < vars.data + vars.len && !overridden; v += wcslen(v) + 1) {
                size_t nameLen = wcscspn(v, L"=");
                overridden = _wcsnicmp(e, v, nameLen) == 0 && e[nameLen] == L'=';
            }
            if (!overridden) out.append(e, wcslen(e) + 1);
        }
        out.append(L'\0');
    }
    if (parent) FreeEnvironmentStringsW(parent);
    return ok;
}

// -----------------------------------------------------------------------------
// Interactive Parameter Prompt (InputBox)
// -----------------------------------------------------------------------------

#define ID_IB_EDIT 1001

struct InputBoxParams {
    const wchar_t* prompt;
    const wchar_t* defVal;
    wchar_t outVal[512];
    bool isPassword;
    HFONT hFont;
    int dpi;
    bool okPressed;
};

static LRESULT CALLBACK InputBoxWndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam)
{
    InputBoxParams* p = (InputBoxParams*)GetWindowLongPtrW(hwnd, GWLP_USERDATA);

    switch (msg) {
    case WM_CREATE: {
        CREATESTRUCTW* cs = (CREATESTRUCTW*)lParam;
        p = (InputBoxParams*)cs->lpCreateParams;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (LONG_PTR)p);

        int dpi = p->dpi;
        int m = MulDiv(14, dpi, 96);
        int w = MulDiv(400, dpi, 96);
        int promptH = MulDiv(34, dpi, 96);
        int editH = MulDiv(24, dpi, 96);
        int btnW = MulDiv(80, dpi, 96);
        int btnH = MulDiv(26, dpi, 96);

        HWND hPrompt = CreateWindowExW(0, L"STATIC", p->prompt,
            WS_CHILD | WS_VISIBLE | SS_LEFT,
            m, m, w - m * 2, promptH, hwnd, NULL, cs->hInstance, NULL);

        DWORD editStyle = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL;
        if (p->isPassword) editStyle |= ES_PASSWORD;

        HWND hEdit = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", p->defVal,
            editStyle,
            m, m + promptH + MulDiv(4, dpi, 96), w - m * 2, editH, hwnd, (HMENU)(INT_PTR)ID_IB_EDIT, cs->hInstance, NULL);

        int btnY = m + promptH + editH + MulDiv(14, dpi, 96);

        HWND hBtnOk = CreateWindowExW(0, L"BUTTON", L"OK",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON,
            w - m - btnW * 2 - MulDiv(8, dpi, 96), btnY, btnW, btnH, hwnd, (HMENU)IDOK, cs->hInstance, NULL);

        HWND hBtnCancel = CreateWindowExW(0, L"BUTTON", Tr(L"Отмена", L"Cancel"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON,
            w - m - btnW, btnY, btnW, btnH, hwnd, (HMENU)IDCANCEL, cs->hInstance, NULL);

        HWND controls[] = { hPrompt, hEdit, hBtnOk, hBtnCancel };
        for (size_t i = 0; i < ARRAYSIZE(controls); i++) {
            if (p->hFont) SendMessageW(controls[i], WM_SETFONT, (WPARAM)p->hFont, TRUE);
        }
        SendMessageW(hEdit, EM_SETSEL, 0, -1);
        SetFocus(hEdit);
        return 0;
    }

    case WM_COMMAND: {
        int id = LOWORD(wParam);
        if (id == IDOK || id == IDCANCEL) {
            if (p && id == IDOK) {
                GetDlgItemTextW(hwnd, ID_IB_EDIT, p->outVal, ARRAYSIZE(p->outVal));
                p->okPressed = true;
            }
            DestroyWindow(hwnd);
            return 0;
        }
        break;
    }

    case WM_CLOSE:
        DestroyWindow(hwnd);
        return 0;
    }

    return DefWindowProcW(hwnd, msg, wParam, lParam);
}

// Modal input dialog; works with a hidden owner (action started before the window is shown)
static bool ShowInputBox(HWND hwndOwner, const wchar_t* title, const wchar_t* prompt, const wchar_t* defVal, wchar_t* outVal, size_t maxLen, bool isPassword, HFONT hFont, int dpi)
{
    static bool s_classRegistered = false;
    HINSTANCE hInst = GetModuleHandleW(NULL);
    const wchar_t* IB_CLASS = L"BpdfInputBox";

    if (!s_classRegistered) {
        WNDCLASSEXW wc = { sizeof(wc) };
        wc.lpfnWndProc = InputBoxWndProc;
        wc.hInstance = hInst;
        wc.hCursor = LoadCursorW(NULL, IDC_ARROW);
        wc.hbrBackground = (HBRUSH)(COLOR_BTNFACE + 1);
        wc.lpszClassName = IB_CLASS;
        RegisterClassExW(&wc);
        s_classRegistered = true;
    }

    InputBoxParams params = {};
    params.prompt = prompt;
    params.defVal = defVal;
    params.isPassword = isPassword;
    params.hFont = hFont;
    params.dpi = (dpi > 0) ? dpi : 96;

    RECT rc = { 0, 0, MulDiv(400, params.dpi, 96), MulDiv(135, params.dpi, 96) };
    DWORD dwStyle = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    bool ownerVisible = hwndOwner && IsWindowVisible(hwndOwner);
    DWORD dwExStyle = WS_EX_DLGMODALFRAME | (ownerVisible ? 0 : WS_EX_APPWINDOW);
    AdjustWindowRectEx(&rc, dwStyle, FALSE, dwExStyle);
    int winW = rc.right - rc.left;
    int winH = rc.bottom - rc.top;

    RECT area;
    if (ownerVisible) {
        GetWindowRect(hwndOwner, &area);
    } else {
        MONITORINFO mi = { sizeof(mi) };
        GetMonitorInfoW(MonitorFromWindow(hwndOwner, MONITOR_DEFAULTTOPRIMARY), &mi);
        area = mi.rcWork;
    }

    HWND hDlg = CreateWindowExW(
        dwExStyle, IB_CLASS, title ? title : L"bpdf", dwStyle,
        area.left + ((area.right - area.left) - winW) / 2,
        area.top + ((area.bottom - area.top) - winH) / 2,
        winW, winH, hwndOwner, NULL, hInst, &params);
    if (!hDlg) return false;

    if (ownerVisible) EnableWindow(hwndOwner, FALSE);
    ShowWindow(hDlg, SW_SHOW);
    SetForegroundWindow(hDlg);

    MSG msg;
    while (IsWindow(hDlg)) {
        BOOL got = GetMessageW(&msg, NULL, 0, 0);
        if (got == 0) {
            // WM_QUIT belongs to the main loop
            PostQuitMessage((int)msg.wParam);
            DestroyWindow(hDlg);
            params.okPressed = false;
            break;
        }
        if (got == -1) break;
        if (msg.message == WM_KEYDOWN && msg.wParam == VK_ESCAPE) {
            DestroyWindow(hDlg);
            break;
        }
        if (!IsDialogMessageW(hDlg, &msg)) {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    if (ownerVisible) {
        EnableWindow(hwndOwner, TRUE);
        SetForegroundWindow(hwndOwner);
    }

    if (params.okPressed) lstrcpynW(outVal, params.outVal, (int)maxLen);
    SecureZeroMemory(params.outVal, sizeof(params.outVal));
    return params.okPressed;
}

// -----------------------------------------------------------------------------
// Command Template Expansion
// -----------------------------------------------------------------------------

struct Placeholders {
    StrW file, dir, name, ext, exeDir;
};

static const wchar_t* const kPlaceholders[] = {
    L"{file}", L"{dir}", L"{name}", L"{ext}", L"{exe_dir}", L"{tool_dir}"
};

// Index in kPlaceholders of the placeholder at p, -1 if there is none
static int MatchPlaceholder(const wchar_t* p, size_t* tagLen)
{
    for (int i = 0; i < (int)ARRAYSIZE(kPlaceholders); i++) {
        size_t n = wcslen(kPlaceholders[i]);
        if (wcsncmp(p, kPlaceholders[i], n) == 0) {
            *tagLen = n;
            return i;
        }
    }
    return -1;
}

static const wchar_t* PlaceholderValue(const Placeholders& ph, int index)
{
    const StrW* values[] = { &ph.file, &ph.dir, &ph.name, &ph.ext, &ph.exeDir, &ph.exeDir };
    return values[index]->c_str();
}

static void AssignFullPath(StrW& out, const wchar_t* path)
{
    DWORD n = GetFullPathNameW(path, 0, NULL, NULL);
    if (n == 0 || !out.reserve(n) || GetFullPathNameW(path, n, out.data, NULL) >= n) {
        out.assign(path);
        return;
    }
    out.len = wcslen(out.data);
}

// {file} {dir} {name} {ext} of the first selected file; with no files {dir} is
// the current directory. A selected folder is its own {dir}
static void FillPlaceholders(const GuiContext* ctx, Placeholders& ph)
{
    ph.exeDir.assign(ctx->targetExe);
    PathRemoveFileSpecW(ph.exeDir.data);
    ph.exeDir.len = wcslen(ph.exeDir.data);

    if (ctx->files.empty()) {
        DWORD n = GetCurrentDirectoryW(0, NULL);
        if (n && ph.dir.reserve(n) && GetCurrentDirectoryW(n, ph.dir.data) < n) {
            ph.dir.len = wcslen(ph.dir.data);
        }
        return;
    }

    AssignFullPath(ph.file, ctx->files[0]);
    StrW& full = ph.file;
    while (full.len > 3 && (full.data[full.len - 1] == L'\\' || full.data[full.len - 1] == L'/')) {
        full.truncate(full.len - 1);
    }

    if (full.len == 3 && full.data[1] == L':') { // drive root "D:\"
        ph.dir.assign(full.data);
        ph.name.assign(full.data, 1);
        return;
    }

    const wchar_t* fileName = PathFindFileNameW(full.data);
    DWORD attrs = GetFileAttributesW(full.data);
    if (attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_DIRECTORY)) {
        ph.dir.assign(full.data);
        ph.name.assign(fileName);
        return;
    }

    size_t dirLen = fileName - full.data;
    if (dirLen > 3) dirLen--; // without the slash, except in "D:\"
    ph.dir.assign(full.data, dirLen);

    const wchar_t* dot = PathFindExtensionW(fileName);
    if (dot[0] && dot != fileName) {
        ph.name.assign(fileName, dot - fileName);
        ph.ext.assign(dot + 1);
    } else {
        ph.name.assign(fileName);
    }
}

// A value inserted into the command: escaped inside "...", quoted when needed outside
static void AppendValue(StrW& out, const wchar_t* value, bool inQuotes, wchar_t next)
{
    if (inQuotes) AppendEscaped(out, value, next == L'"');
    else AppendArg(out, value);
}

// The '}' closing the '{' at p, NULL if unbalanced
static const wchar_t* FindClosingBrace(const wchar_t* p)
{
    int depth = 0;
    for (; *p; ++p) {
        if (*p == L'{') depth++;
        else if (*p == L'}' && --depth == 0) return p;
    }
    return NULL;
}

// Dialog for "Prompt:Default" between content and end; placeholders in the
// default are expanded, braces in the prompt such as {n} are kept
static bool AskValue(GuiContext* ctx, const wchar_t* title, const wchar_t* content, const wchar_t* end,
                     bool isPass, const Placeholders& ph, wchar_t* value, size_t valueLen)
{
    const wchar_t* sep = NULL;
    int depth = 0;
    for (const wchar_t* c = content; c < end && !sep; ++c) {
        if (*c == L'{') depth++;
        else if (*c == L'}') depth--;
        else if (*c == L':' && depth == 0) sep = c;
    }

    wchar_t prompt[256];
    size_t promptLen = (sep ? sep : end) - content;
    lstrcpynW(prompt, content, (int)(promptLen + 1 < ARRAYSIZE(prompt) ? promptLen + 1 : ARRAYSIZE(prompt)));

    StrW defVal;
    for (const wchar_t* c = sep ? sep + 1 : end; c < end;) {
        size_t tagLen = 0;
        int index = (*c == L'{') ? MatchPlaceholder(c, &tagLen) : -1;
        if (index >= 0 && c + tagLen <= end) {
            defVal.append(PlaceholderValue(ph, index));
            c += tagLen;
        } else {
            defVal.append(*c++);
        }
    }
    return ShowInputBox(ctx->hwnd, title, prompt, defVal.c_str(), value, valueLen, isPass, ctx->hFontUi, ctx->dpi);
}

// For `--password {ask_pass:...}` or `--password "{ask_pass:...}"` of a command
// that also reads the password from its environment: removes the option from
// out and returns that variable. Every command has variables of its own, so a
// password left in an environment never reaches another command
static const wchar_t* TakePasswordOption(StrW& out, bool inQuotes, wchar_t next)
{
    static const wchar_t* const kOptions[][3] = {
        { L"protect", L"--password", L"BPDF_PROTECT_PASSWORD" },
        { L"protect", L"--owner-password", L"BPDF_PROTECT_OWNER_PASSWORD" },
        { L"unlock", L"--password", L"BPDF_UNLOCK_PASSWORD" },
    };

    size_t end = out.len;
    if (inQuotes) {
        // Only a quote opened right before the prompt and closed right after it
        if (end == 0 || out.data[end - 1] != L'"' || next != L'"') return NULL;
        end--;
    }
    size_t optEnd = end;
    while (optEnd > 0 && (out.data[optEnd - 1] == L' ' || out.data[optEnd - 1] == L'\t')) optEnd--;
    if (optEnd == end) return NULL;
    size_t start = optEnd;
    while (start > 0 && out.data[start - 1] != L' ' && out.data[start - 1] != L'\t') start--;

    // The bpdf command is the first word that is not an option
    const wchar_t* command = out.data;
    for (;;) {
        command += wcsspn(command, L" \t");
        if (*command != L'-') break;
        command += wcscspn(command, L" \t");
    }
    size_t commandLen = wcscspn(command, L" \t");

    for (size_t i = 0; i < ARRAYSIZE(kOptions); i++) {
        size_t n = wcslen(kOptions[i][1]);
        if (commandLen == wcslen(kOptions[i][0]) && wcsncmp(command, kOptions[i][0], commandLen) == 0 &&
            optEnd - start == n && wcsncmp(out.data + start, kOptions[i][1], n) == 0) {
            out.truncate(start);
            return kOptions[i][2];
        }
    }
    return NULL;
}

// Expands the placeholders and the {ask:Prompt:Default} / {ask_pass:Prompt}
// dialogs of a cmd template in one pass, tracking quotes by the
// CommandLineToArgvW rules so every value is escaped for its position.
// A password for --password/--owner-password goes to secrets ("NAME=value\0")
// instead of the command line, which other programs and logs can read.
// False when a dialog was cancelled.
static bool ExpandTemplate(GuiContext* ctx, const wchar_t* title, const wchar_t* tpl, const Placeholders& ph,
                           StrW& out, StrW& secrets, bool& hasFile)
{
    bool inQuotes = false;
    size_t backslashes = 0;
    const wchar_t* p = tpl;
    while (*p) {
        size_t tagLen = 0;
        int index = (*p == L'{') ? MatchPlaceholder(p, &tagLen) : -1;
        if (index >= 0) {
            const wchar_t* value = PlaceholderValue(ph, index);
            hasFile |= (index == 0);
            p += tagLen;
            AppendValue(out, value, inQuotes, *p);
            // "{dir}\name" for a file in the root "D:\"
            size_t valueLen = wcslen(value);
            if (*p == L'\\' && valueLen && value[valueLen - 1] == L'\\') p++;
            backslashes = 0;
            continue;
        }

        bool isPass = wcsncmp(p, L"{ask_pass:", 10) == 0;
        const wchar_t* end = (isPass || wcsncmp(p, L"{ask:", 5) == 0) ? FindClosingBrace(p) : NULL;
        if (end) {
            wchar_t value[512] = {};
            if (!AskValue(ctx, title, p + (isPass ? 10 : 5), end, isPass, ph, value, ARRAYSIZE(value))) {
                return false;
            }
            p = end + 1;
            const wchar_t* text = isPass ? value : Trim(value);
            const wchar_t* envName = isPass ? TakePasswordOption(out, inQuotes, *p) : NULL;
            if (envName) {
                secrets.append(envName);
                secrets.append(L'=');
                secrets.append(text);
                secrets.append(L'\0');
                if (inQuotes) {
                    p++; // the closing quote went with the option
                    inQuotes = false;
                }
            } else {
                AppendValue(out, text, inQuotes, *p);
            }
            SecureZeroMemory(value, sizeof(value));
            backslashes = 0;
            continue;
        }

        if (*p == L'"' && backslashes % 2 == 0) inQuotes = !inQuotes;
        backslashes = (*p == L'\\') ? backslashes + 1 : 0;
        out.append(*p++);
    }
    return true;
}

// -----------------------------------------------------------------------------
// Load Actions for Selected Files & Adjust Window Size
// -----------------------------------------------------------------------------

static void LoadPaletteForFiles(GuiContext* ctx)
{
    ctx->clearPaletteButtons();

    wchar_t subInfo[512] = {};
    if (ctx->files.empty()) {
        SetWindowTextW(ctx->hTargetTitle, Tr(L"Файлы не выбраны", L"No files selected"));
        lstrcpynW(subInfo, Tr(L"Перетащите файлы сюда (Drag & Drop) или нажмите «Выбрать файлы...»", L"Drop files here or click \"Choose files...\""), ARRAYSIZE(subInfo));
    } else if (ctx->files.size() == 1) {
        const wchar_t* path = ctx->files[0];
        SetWindowTextW(ctx->hTargetTitle, PathFindFileNameW(path));

        WIN32_FILE_ATTRIBUTE_DATA fad;
        if (!GetFileAttributesExW(path, GetFileExInfoStandard, &fad)) {
            lstrcpynW(subInfo, path, ARRAYSIZE(subInfo));
        } else if (fad.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
            wnsprintfW(subInfo, ARRAYSIZE(subInfo), Tr(L"Папка: %s", L"Folder: %s"), path);
        } else {
            ULONGLONG size = ((ULONGLONG)fad.nFileSizeHigh << 32) | fad.nFileSizeLow;
            const ULONGLONG mb = 1024 * 1024;
            if (size >= mb) {
                wnsprintfW(subInfo, ARRAYSIZE(subInfo), Tr(L"Размер: %u.%u МБ  |  %s", L"Size: %u.%u MB  |  %s"),
                    (DWORD)(size / mb), (DWORD)((size % mb) * 10 / mb), path);
            } else {
                wnsprintfW(subInfo, ARRAYSIZE(subInfo), Tr(L"Размер: %u КБ  |  %s", L"Size: %u KB  |  %s"), (DWORD)(size / 1024), path);
            }
        }
    } else {
        wchar_t summary[64];
        wnsprintfW(summary, ARRAYSIZE(summary), Tr(L"Выбрано файлов: %d", L"Files selected: %d"), (int)ctx->files.size());
        SetWindowTextW(ctx->hTargetTitle, summary);

        StrW dir;
        dir.assign(ctx->files[0]);
        PathRemoveFileSpecW(dir.data);
        wnsprintfW(subInfo, ARRAYSIZE(subInfo), Tr(L"Папка: %s", L"Folder: %s"), dir.c_str());
    }

    if (!ctx->files.empty()) {
        wchar_t section[64];
        GetSectionForFiles(ctx->files, section, ARRAYSIZE(section));

        MenuItemDef items[MAX_INI_ITEMS];
        size_t count = LoadMenuItems(section, items, ARRAYSIZE(items));
        if (count == 0) {
            wnsprintfW(subInfo, ARRAYSIZE(subInfo), Tr(L"Нет действий: bpdf_shell_ru.ini (bpdf_shell.ini) не найден или в нём нет раздела [%s]", L"No actions: bpdf_shell.ini is missing or has no [%s] section"), section);
        } else if (!ctx->targetExe[0]) {
            lstrcpynW(subInfo, Tr(L"Предупреждение: bpdf.exe не найден рядом или в PATH", L"Warning: bpdf.exe was not found beside the program or in PATH"), ARRAYSIZE(subInfo));
        }

        HINSTANCE hInst = (HINSTANCE)GetWindowLongPtrW(ctx->hwnd, GWLP_HINSTANCE);
        int iconSize = ctx->scale(16);
        for (size_t i = 0; i < count && ctx->actionCount < MAX_SHELL_ACTIONS; i++) {
            if (items[i].iconIndex == -1) continue;

            // Text left-aligned right after the icon, like in a menu: a long
            // title never runs over the icon
            size_t index = ctx->actionCount;
            HWND hBtn = CreateWindowExW(0, L"BUTTON", items[i].title,
                WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON | BS_LEFT,
                0, 0, 0, 0, ctx->hwnd, (HMENU)(INT_PTR)(ID_ACTION_BASE + index), hInst, NULL);
            if (!hBtn) continue;
            SendMessageW(hBtn, WM_SETFONT, (WPARAM)ctx->hFontUi, FALSE);

            HICON hIcon = NULL;
            if (ctx->targetExe[0]) {
                UINT got = PrivateExtractIconsW(ctx->targetExe, items[i].iconIndex, iconSize, iconSize, &hIcon, NULL, 1, 0);
                if (got == 0 || got == 0xFFFFFFFF) hIcon = NULL;
            }
            if (hIcon) {
                HIMAGELIST himl = ImageList_Create(iconSize, iconSize, ILC_COLOR32 | ILC_MASK, 1, 1);
                if (himl) {
                    ImageList_AddIcon(himl, hIcon);
                    BUTTON_IMAGELIST bil = {};
                    bil.himl = himl;
                    bil.margin.left = ctx->scale(8);
                    bil.margin.right = ctx->scale(8);
                    bil.uAlign = BUTTON_IMAGELIST_ALIGN_LEFT;
                    SendMessageW(hBtn, BCM_SETIMAGELIST, 0, (LPARAM)&bil);
                    ctx->hActionIml[index] = himl;
                }
                DestroyIcon(hIcon);
            }

            ctx->actionDefs[index] = items[i];
            ctx->hActionBtns[index] = hBtn;
            ctx->actionCount++;
        }
    }
    SetWindowTextW(ctx->hTargetSub, subInfo);

    SetClientSize(ctx, PaletteClientWidth(ctx),PaletteFooterTop(ctx) + ctx->scale(28) + ctx->scale(16));
    LayoutControls(ctx);
}

// -----------------------------------------------------------------------------
// State Transitions
// -----------------------------------------------------------------------------

static void AppendLog(GuiContext* ctx, const wchar_t* text)
{
    int len = GetWindowTextLengthW(ctx->hEdit);
    SendMessageW(ctx->hEdit, EM_SETSEL, len, len);
    SendMessageW(ctx->hEdit, EM_REPLACESEL, FALSE, (LPARAM)text);
    SendMessageW(ctx->hEdit, EM_SCROLLCARET, 0, 0);
}

static void ShowMainWindow(GuiContext* ctx)
{
    KillTimer(ctx->hwnd, ID_TIMER_SHOW);
    if (IsWindowVisible(ctx->hwnd)) return;
    ShowWindow(ctx->hwnd, SW_SHOW);
    SetForegroundWindow(ctx->hwnd);
}

// bpdf prints warnings as "Warning: ..." lines (stderr goes to the same log)
static bool LogHasWarnings(GuiContext* ctx)
{
    int len = GetWindowTextLengthW(ctx->hEdit);
    if (len <= 0) return false;
    wchar_t* text = (wchar_t*)malloc((len + 1) * sizeof(wchar_t));
    if (!text) return false;
    GetWindowTextW(ctx->hEdit, text, len + 1);
    const wchar_t* prefix = L"Warning: ";
    bool found = wcsncmp(text, prefix, wcslen(prefix)) == 0;
    for (const wchar_t* nl = wcschr(text, L'\n'); nl && !found; nl = wcschr(nl + 1, L'\n')) {
        found = wcsncmp(nl + 1, prefix, wcslen(prefix)) == 0;
    }
    free(text);
    return found;
}

static void ReturnToPalette(GuiContext* ctx)
{
    ctx->stopWorker();
    ctx->deleteTempManifest();

    ctx->state = STATE_PALETTE;
    ctx->isPaletteSession = true;
    ctx->filesEmbeddedInCmd = false;

    SetWindowIconIndex(ctx, 0);
    SetWindowTextW(ctx->hBtnClose, Tr(L"Закрыть", L"Close"));
    SetWindowTextW(ctx->hwnd, Tr(L"bpdf — Панель действий", L"bpdf — Actions"));
    LoadPaletteForFiles(ctx);
}

// Ends a single-action session (its window may not even be shown yet)
static void AbortAction(GuiContext* ctx)
{
    if (!ctx->isPaletteSession) PostMessageW(ctx->hwnd, WM_CLOSE, 0, 0);
}

static void StartActionExecution(GuiContext* ctx, const MenuItemDef& item, const wchar_t* cmdTemplate)
{
    if (!ctx->targetExe[0] && !FindBpdfExe(ctx->targetExe, ARRAYSIZE(ctx->targetExe))) {
        MessageBoxW(
            ctx->hwnd,
            Tr(L"Исполняемый файл bpdf.exe не найден.\n"
               L"Поместите bpdf.exe рядом с программой или добавьте путь к нему в переменную PATH.",
               L"bpdf.exe was not found.\n"
               L"Put bpdf.exe beside this program or add its folder to PATH."),
            L"bpdf",
            MB_ICONERROR
        );
        AbortAction(ctx);
        return;
    }

    ctx->stopWorker();
    ctx->deleteTempManifest();

    Placeholders ph;
    FillPlaceholders(ctx, ph);

    StrW cmd;
    StrW secrets;
    secrets.reserve(1024); // no reallocation leaves copies of passwords behind
    bool hasFile = false;
    if (!ExpandTemplate(ctx, item.title, cmdTemplate, ph, cmd, secrets, hasFile)) {
        secrets.wipe();
        AbortAction(ctx);
        return;
    }

    StrW cmdLine;
    AppendQuotedArg(cmdLine, ctx->targetExe);
    cmdLine.append(L" --no-pause ");
    cmdLine.append(cmd.c_str());
    cmd.wipe();

    // Files go last unless the command already has them ({file} or after "--")
    if (!hasFile && !ctx->filesEmbeddedInCmd && !ctx->files.empty()) {
        cmdLine.append(L' ');
        if (ctx->files.size() == 1) {
            AppendQuotedArg(cmdLine, ctx->files[0]);
        } else if (CreateManifestFile(ctx->files, ctx->tempManifest, ARRAYSIZE(ctx->tempManifest))) {
            StrW atManifest;
            atManifest.append(L'@');
            atManifest.append(ctx->tempManifest);
            AppendQuotedArg(cmdLine, atManifest.c_str());
        } else {
            MessageBoxW(ctx->hwnd, Tr(L"Не удалось создать список файлов", L"Could not create the file list"), L"bpdf", MB_ICONERROR);
            secrets.wipe();
            AbortAction(ctx);
            return;
        }
    }

    StrW environment;
    bool envOk = secrets.empty() || BuildEnvironment(secrets, environment);
    secrets.wipe();
    if (!envOk) {
        MessageBoxW(ctx->hwnd, Tr(L"Недостаточно памяти для запуска bpdf.exe", L"Not enough memory to start bpdf.exe"), L"bpdf", MB_ICONERROR);
        AbortAction(ctx);
        return;
    }

    ctx->workDir.assign(ph.dir.c_str());
    ctx->guiMode = item.guiMode;
    ctx->state = STATE_RUNNING;

    SetWindowIconIndex(ctx, item.iconIndex);
    SetWindowTextW(ctx->hTitleStatic, item.title);
    wchar_t text[256];
    wnsprintfW(text, ARRAYSIZE(text), Tr(L"Выполняется: %s...", L"Running: %s..."), item.title);
    SetWindowTextW(ctx->hStatusStatic, text);
    wnsprintfW(text, ARRAYSIZE(text), L"bpdf — %s", item.title);
    SetWindowTextW(ctx->hwnd, text);
    SetWindowTextW(ctx->hEdit, L"");
    SetWindowTextW(ctx->hBtnClose, Tr(L"Отмена", L"Cancel"));
    SendMessageW(ctx->hProgress, PBM_SETMARQUEE, TRUE, 30);

    RECT client;
    GetClientRect(ctx->hwnd, &client);
    if (client.bottom < ctx->scale(400)) SetClientSize(ctx, 0, ctx->scale(400));
    LayoutControls(ctx);

    // A quick action finishes before the window would appear: nothing flashes
    if (!IsWindowVisible(ctx->hwnd)) SetTimer(ctx->hwnd, ID_TIMER_SHOW, SHOW_DELAY_MS, NULL);

    bool spawned = SpawnProcess(ctx, cmdLine.data, environment.empty() ? NULL : environment.data);
    environment.wipe();
    cmdLine.wipe();
    if (!spawned) {
        AppendLog(ctx, Tr(L"Не удалось запустить bpdf.exe\r\n", L"Could not start bpdf.exe\r\n"));
        PostMessageW(ctx->hwnd, WM_BPDF_FINISHED, 1, (LPARAM)ctx->runId);
    }
}

static void BrowseForFiles(GuiContext* ctx)
{
    const DWORD bufLen = 32768;
    wchar_t* buf = (wchar_t*)calloc(bufLen, sizeof(wchar_t));
    if (!buf) return;

    OPENFILENAMEW ofn = { sizeof(ofn) };
    ofn.hwndOwner = ctx->hwnd;
    const wchar_t* supported = L"*.pdf;*.docx;*.xlsx;*.pptx;*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.tiff;*.tif;*.avif;*.heic;*.epub;*.cbz;*.fb2;*.txt;*.md;*.html";
    const wchar_t* images = L"*.png;*.jpg;*.jpeg;*.bmp;*.tiff;*.webp;*.avif;*.heic";
    wchar_t filter[1024];
    int filterLen = wnsprintfW(filter, ARRAYSIZE(filter), Tr(
        L"Все поддерживаемые файлы|%s|PDF документы (*.pdf)|*.pdf|Изображения|%s|Все файлы (*.*)|*.*|",
        L"All supported files|%s|PDF documents (*.pdf)|*.pdf|Images|%s|All files (*.*)|*.*|"), supported, images);
    // The filter is a list of NUL-separated strings ending with two NULs
    for (int i = 0; i < filterLen; i++) if (filter[i] == L'|') filter[i] = L'\0';
    ofn.lpstrFilter = filter;
    ofn.lpstrFile = buf;
    ofn.nMaxFile = bufLen;
    ofn.Flags = OFN_ALLOWMULTISELECT | OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;

    if (GetOpenFileNameW(&ofn)) {
        ctx->files.clear();
        ctx->filesEmbeddedInCmd = false;
        // One file: its full path; several: the folder, then the names
        const wchar_t* dir = buf;
        const wchar_t* name = dir + wcslen(dir) + 1;
        if (!*name) ctx->files.add(dir);
        StrW full;
        for (; *name; name += wcslen(name) + 1) {
            full.assign(dir);
            if (full.len && full.data[full.len - 1] != L'\\') full.append(L'\\');
            full.append(name);
            ctx->files.add(full.c_str());
        }
        LoadPaletteForFiles(ctx);
    }
    free(buf);
}

// -----------------------------------------------------------------------------
// Window Procedure
// -----------------------------------------------------------------------------

static HWND CreateChild(GuiContext* ctx, const wchar_t* cls, const wchar_t* text, DWORD style, int id, DWORD exStyle = 0)
{
    return CreateWindowExW(exStyle, cls, text, WS_CHILD | style, 0, 0, 0, 0, ctx->hwnd,
        (HMENU)(INT_PTR)id, (HINSTANCE)GetWindowLongPtrW(ctx->hwnd, GWLP_HINSTANCE), NULL);
}

static LRESULT CALLBACK GuiWndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam)
{
    GuiContext* ctx = (GuiContext*)GetWindowLongPtrW(hwnd, GWLP_USERDATA);

    switch (msg) {
    case WM_CREATE: {
        ctx = (GuiContext*)((CREATESTRUCTW*)lParam)->lpCreateParams;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (LONG_PTR)ctx);
        ctx->hwnd = hwnd;
        ctx->dpi = WindowDpi(hwnd);
        ctx->hBrushEdit = CreateSolidBrush(RGB(24, 24, 24));

        DWORD label = SS_LEFT | SS_ENDELLIPSIS | SS_NOPREFIX;
        DWORD button = WS_TABSTOP | BS_PUSHBUTTON;
        ctx->hTargetTitle = CreateChild(ctx, L"STATIC", L"", label, 0);
        ctx->hTargetSub = CreateChild(ctx, L"STATIC", L"", label, 0);
        ctx->hTitleStatic = CreateChild(ctx, L"STATIC", L"bpdf", label, 0);
        ctx->hStatusStatic = CreateChild(ctx, L"STATIC", L"", label, 0);
        ctx->hProgress = CreateChild(ctx, PROGRESS_CLASSW, NULL, PBS_MARQUEE, 0);
        ctx->hEdit = CreateChild(ctx, L"EDIT", L"", WS_VSCROLL | ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY, 0, WS_EX_CLIENTEDGE);
        SendMessageW(ctx->hEdit, EM_SETLIMITTEXT, 0, 0);
        ctx->hBtnClose = CreateChild(ctx, L"BUTTON", Tr(L"Закрыть", L"Close"), button, ID_BTN_CLOSE);
        ctx->hBtnFolder = CreateChild(ctx, L"BUTTON", Tr(L"Открыть папку", L"Open folder"), button, ID_BTN_FOLDER);
        ctx->hBtnBrowse = CreateChild(ctx, L"BUTTON", Tr(L"Выбрать файлы...", L"Choose files..."), button, ID_BTN_BROWSE);
        ctx->hBtnBack = CreateChild(ctx, L"BUTTON", Tr(L"← К действиям", L"← Back to actions"), button, ID_BTN_BACK);
        ApplyDpiFonts(ctx);

        SetWindowIconIndex(ctx, ctx->hasCustomAction ? ctx->customAction.iconIndex : 0);
        DragAcceptFiles(hwnd, TRUE);

        SetWindowPos(hwnd, NULL, 0, 0, ctx->scale(580), ctx->scale(400), SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        LoadPaletteForFiles(ctx);

        // An action given on the command line starts once the message loop runs,
        // so its dialogs are not shown from inside CreateWindow
        bool autoAction = ctx->hasCustomAction ||
            (ctx->autoActionIndex >= 0 && (size_t)ctx->autoActionIndex < ctx->actionCount);
        ctx->isPaletteSession = !autoAction;
        if (autoAction) PostMessageW(hwnd, WM_BPDF_START_AUTO_ACTION, 0, 0);
        return 0;
    }

    case WM_DPICHANGED: {
        if (!ctx) break;
        ctx->dpi = HIWORD(wParam);
        ApplyDpiFonts(ctx);
        const RECT* r = (const RECT*)lParam;
        SetWindowPos(hwnd, NULL, r->left, r->top, r->right - r->left, r->bottom - r->top, SWP_NOZORDER | SWP_NOACTIVATE);
        if (ctx->state == STATE_PALETTE) LoadPaletteForFiles(ctx); // icons of the new size
        else LayoutControls(ctx);
        return 0;
    }

    case WM_DROPFILES: {
        HDROP hDrop = (HDROP)wParam;
        UINT fileCount = DragQueryFileW(hDrop, 0xFFFFFFFF, NULL, 0);
        if (fileCount > 0 && ctx && ctx->state == STATE_PALETTE) {
            ctx->files.clear();
            ctx->filesEmbeddedInCmd = false;
            StrW path;
            for (UINT i = 0; i < fileCount; i++) {
                UINT len = DragQueryFileW(hDrop, i, NULL, 0);
                if (len > 0 && path.reserve(len) && DragQueryFileW(hDrop, i, path.data, len + 1) > 0) {
                    ctx->files.add(path.data);
                }
            }
            LoadPaletteForFiles(ctx);
        }
        DragFinish(hDrop);
        return 0;
    }

    case WM_SIZE:
        if (ctx) LayoutControls(ctx);
        return 0;

    case WM_CTLCOLOREDIT:
    case WM_CTLCOLORSTATIC:
        if (ctx && (HWND)lParam == ctx->hEdit) {
            HDC hdc = (HDC)wParam;
            SetTextColor(hdc, RGB(220, 220, 220));
            SetBkColor(hdc, RGB(24, 24, 24));
            return (LRESULT)ctx->hBrushEdit;
        }
        break;

    case WM_BPDF_APPEND:
        if (ctx && (LONG)wParam == ctx->runId) AppendLog(ctx, (const wchar_t*)lParam);
        free((void*)lParam);
        return 0;

    case WM_BPDF_START_AUTO_ACTION:
        if (!ctx) return 0;
        if (ctx->hasCustomAction) {
            StartActionExecution(ctx, ctx->customAction, ctx->customCmd.c_str());
        } else {
            const MenuItemDef& item = ctx->actionDefs[ctx->autoActionIndex];
            StartActionExecution(ctx, item, item.cmd);
        }
        return 0;

    case WM_BPDF_FINISHED: {
        if (!ctx || (LONG)lParam != ctx->runId) return 0;
        bool warnings = wParam == 0 && LogHasWarnings(ctx);

        // A single action that went cleanly has nothing to show: the result is
        // in the file panel. Errors, warnings, info and keep stay on screen
        if (wParam == 0 && !warnings && ctx->guiMode == GUI_AUTO && !ctx->isPaletteSession) {
            DestroyWindow(hwnd);
            return 0;
        }

        ctx->state = STATE_FINISHED;
        SendMessageW(ctx->hProgress, PBM_SETMARQUEE, FALSE, 0);
        SetWindowTextW(ctx->hBtnBack, ctx->isPaletteSession ? Tr(L"← К действиям", L"← Back to actions") : Tr(L"Панель действий...", L"Actions..."));
        SetWindowTextW(ctx->hBtnClose, Tr(L"Закрыть", L"Close"));
        SetWindowTextW(ctx->hStatusStatic,
            wParam != 0 ? Tr(L"Завершено с ошибками (проверьте журнал вывода)", L"Finished with errors (see the log)") :
            warnings ? Tr(L"Готово, но есть предупреждения (см. журнал вывода)", L"Done, with warnings (see the log)") :
            Tr(L"Готово! Операция успешно завершена.", L"Done! The operation completed successfully."));
        LayoutControls(ctx);
        ShowMainWindow(ctx);
        // Enter takes the default way on: back to the palette or close
        SetFocus(ctx->isPaletteSession ? ctx->hBtnBack : ctx->hBtnClose);
        return 0;
    }

    case WM_TIMER:
        if (wParam != ID_TIMER_SHOW || !ctx) break;
        ShowMainWindow(ctx);
        return 0;

    case WM_COMMAND: {
        if (!ctx) break;
        int id = LOWORD(wParam);

        if (id >= ID_ACTION_BASE && id < ID_ACTION_BASE + MAX_SHELL_ACTIONS) {
            size_t index = id - ID_ACTION_BASE;
            if (ctx->state == STATE_PALETTE && index < ctx->actionCount) {
                const MenuItemDef& item = ctx->actionDefs[index];
                StartActionExecution(ctx, item, item.cmd);
            }
            return 0;
        }

        switch (id) {
        case ID_BTN_BACK:
            ReturnToPalette(ctx);
            return 0;
        case ID_BTN_BROWSE:
            if (ctx->state == STATE_PALETTE) BrowseForFiles(ctx);
            return 0;
        case ID_BTN_FOLDER:
            if (!ctx->workDir.empty()) ShellExecuteW(NULL, L"open", ctx->workDir.c_str(), NULL, NULL, SW_SHOWNORMAL);
            return 0;
        case ID_BTN_CLOSE:
        case IDCANCEL:
            DestroyWindow(hwnd);
            return 0;
        case IDOK: // Enter
            if (ctx->state == STATE_FINISHED) {
                if (ctx->isPaletteSession) ReturnToPalette(ctx);
                else DestroyWindow(hwnd);
            }
            return 0;
        }
        break;
    }

    case WM_DESTROY:
        if (ctx) ctx->stopWorker();
        FlushPendingAppendMessages(hwnd);
        PostQuitMessage(0);
        return 0;
    }

    return DefWindowProcW(hwnd, msg, wParam, lParam);
}

// -----------------------------------------------------------------------------
// Entry Point (wWinMain)
// -----------------------------------------------------------------------------

static void LoadFilesFromListFile(const wchar_t* listPath, WStringList& outFiles)
{
    StrW listText;
    if (!ReadTextFile(listPath, listText)) return;
    wchar_t* p = listText.data;
    while (p && *p) {
        wchar_t* nl = wcschr(p, L'\n');
        if (nl) *nl++ = L'\0';
        wchar_t* t = Trim(p);
        if (*t) outFiles.add(t);
        p = nl;
    }
}

static void ShowHelp()
{
    MessageBoxW(
        NULL,
        Tr(
            L"bpdf-gui — Графический интерфейс действий и прогресса для bpdf\n\n"
            L"Использование:\n"
            L"  bpdf-gui.exe [опции GUI] -- <команда bpdf> [файлы / @список] [параметры]\n"
            L"  bpdf-gui.exe [файлы / @список] --cmd \"команда [параметры]\"\n"
            L"  bpdf-gui.exe --item <номер> [файлы / @список]\n"
            L"  bpdf-gui.exe [файлы / @список]  (открывает палитру действий)\n\n"
            L"Опции GUI: --title <текст>, --icon <номер>, --gui auto|info|keep\n\n"
            L"Примеры:\n"
            L"  bpdf-gui.exe --icon 7 -- rotate doc.pdf -a \"{ask:Угол:90}\" -i\n"
            L"  bpdf-gui.exe --icon 1 -- merge @filelist.txt --optimize -s A4\n"
            L"  bpdf-gui.exe doc.pdf -- watermark \"{file}\" \"{ask:Текст:КОПИЯ}\" --angle 45 -i\n\n"
            L"Параметры диалогов в командах:\n"
            L"  {ask:Подсказка:Дефолт} — диалог ввода значения\n"
            L"  {ask_pass:Подсказка}   — ввод пароля со звёздочками; после --password\n"
            L"                           команд protect и unlock передаётся bpdf через\n"
            L"                           окружение, а не командную строку\n\n"
            L"Кнопки Total Commander:\n"
            L"  Команда:    bpdf-gui.exe --icon 7\n"
            L"  Параметры:  -- rotate @\"%UL\" -a \"{ask:Угол поворота (90, 180, 270, portrait):90}\" -i",
            L"bpdf-gui — actions and progress window for bpdf\n\n"
            L"Usage:\n"
            L"  bpdf-gui.exe [GUI options] -- <bpdf command> [files / @list] [options]\n"
            L"  bpdf-gui.exe [files / @list] --cmd \"command [options]\"\n"
            L"  bpdf-gui.exe --item <number> [files / @list]\n"
            L"  bpdf-gui.exe [files / @list]  (opens the action palette)\n\n"
            L"GUI options: --title <text>, --icon <number>, --gui auto|info|keep\n\n"
            L"Examples:\n"
            L"  bpdf-gui.exe --icon 7 -- rotate doc.pdf -a \"{ask:Angle:90}\" -i\n"
            L"  bpdf-gui.exe --icon 1 -- merge @filelist.txt --optimize -s A4\n"
            L"  bpdf-gui.exe doc.pdf -- watermark \"{file}\" \"{ask:Text:COPY}\" --angle 45 -i\n\n"
            L"Dialog parameters in commands:\n"
            L"  {ask:Prompt:Default} — input dialog for a value\n"
            L"  {ask_pass:Prompt}    — masked password input; after --password of\n"
            L"                         protect and unlock it reaches bpdf through\n"
            L"                         the environment, not the command line\n\n"
            L"Total Commander buttons:\n"
            L"  Command:     bpdf-gui.exe --icon 7\n"
            L"  Parameters:  -- rotate @\"%UL\" -a \"{ask:Rotation angle (90, 180, 270, portrait):90}\" -i"
        ),
        L"bpdf-gui",
        MB_OK | MB_ICONINFORMATION
    );
}

// False for --help. After "--" the rest is the bpdf command; the paths and
// @lists right after its name are the selection the palette returns to
static bool ParseArguments(GuiContext* ctx)
{
    int argc = 0;
    LPWSTR* argv = CommandLineToArgvW(GetCommandLineW(), &argc);
    if (!argv) return true;

    StrW title;
    int icon = 0;
    bool hasGuiMode = false;
    GuiMode guiMode = GUI_AUTO;
    const wchar_t* listFile = NULL;
    bool deleteList = false;
    bool help = false;

    for (int i = 1; i < argc && !help; i++) {
        const wchar_t* arg = argv[i];
        bool hasValue = i + 1 < argc;
        if (StrCmpW(arg, L"--") == 0) {
            bool takeFiles = ctx->files.empty();
            ctx->customCmd.clear();
            for (int j = i + 1; j < argc; j++) {
                const wchar_t* a = argv[j];
                if (j > i + 1) ctx->customCmd.append(L' ');
                AppendArg(ctx->customCmd, a);
                if (j == i + 1 || !takeFiles) continue;
                if (a[0] == L'@' && a[1]) LoadFilesFromListFile(a + 1, ctx->files);
                else if (a[0] != L'-' && GetFileAttributesW(a) != INVALID_FILE_ATTRIBUTES) ctx->files.add(a);
                else takeFiles = false;
                if (takeFiles) ctx->filesEmbeddedInCmd = true;
            }
            break;
        }
        if (StrCmpIW(arg, L"--help") == 0 || StrCmpIW(arg, L"-h") == 0 || StrCmpW(arg, L"/?") == 0) {
            help = true;
        } else if (StrCmpIW(arg, L"--item") == 0 && hasValue) {
            ctx->autoActionIndex = StrToIntW(argv[++i]);
        } else if (StrCmpIW(arg, L"--cmd") == 0 && hasValue) {
            ctx->customCmd.assign(argv[++i]);
        } else if (StrCmpIW(arg, L"--title") == 0 && hasValue) {
            title.assign(argv[++i]);
        } else if (StrCmpIW(arg, L"--icon") == 0 && hasValue) {
            icon = StrToIntW(argv[++i]);
        } else if (StrCmpIW(arg, L"--gui") == 0 && hasValue) {
            const wchar_t* mode = argv[++i];
            hasGuiMode = true;
            guiMode = StrCmpIW(mode, L"info") == 0 ? GUI_INFO : StrCmpIW(mode, L"keep") == 0 ? GUI_KEEP : GUI_AUTO;
        } else if (StrCmpIW(arg, L"--delete-manifest") == 0) {
            deleteList = true;
        } else if (arg[0] == L'@' && arg[1]) {
            LoadFilesFromListFile(arg + 1, ctx->files);
            listFile = arg + 1;
        } else {
            ctx->files.add(arg);
        }
    }

    // The list from the shell extension is ours to delete once read
    if (deleteList && listFile) DeleteFileW(listFile);
    LocalFree(argv);

    if (help) {
        ShowHelp();
        return false;
    }

    if (!ctx->customCmd.empty()) {
        MenuItemDef& action = ctx->customAction;
        ctx->hasCustomAction = true;
        action.iconIndex = icon >= 0 ? icon : 0;

        wchar_t command[64] = {};
        const wchar_t* c = ctx->customCmd.c_str();
        while (*c == L' ' || *c == L'\t') c++;
        for (size_t n = 0; c[n] && c[n] != L' ' && c[n] != L'\t' && n < ARRAYSIZE(command) - 1; n++) {
            command[n] = c[n];
        }
        if (!title.empty()) lstrcpynW(action.title, title.c_str(), ARRAYSIZE(action.title));
        else wnsprintfW(action.title, ARRAYSIZE(action.title), L"bpdf: %s", command[0] ? command : Tr(L"команда", L"command"));

        if (hasGuiMode) action.guiMode = guiMode;
        else action.guiMode = StrCmpIW(command, L"info") == 0 ? GUI_INFO : GUI_AUTO;
    }
    return true;
}

int WINAPI wWinMain(HINSTANCE hInst, HINSTANCE, LPWSTR, int)
{
    INITCOMMONCONTROLSEX icc = { sizeof(icc), ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES };
    InitCommonControlsEx(&icc);

    GuiContext ctx;
    FindBpdfExe(ctx.targetExe, ARRAYSIZE(ctx.targetExe));
    if (!ParseArguments(&ctx)) return 0;

    WNDCLASSEXW wc = { sizeof(wc) };
    wc.lpfnWndProc = GuiWndProc;
    wc.hInstance = hInst;
    wc.hCursor = LoadCursorW(NULL, IDC_ARROW);
    wc.hbrBackground = (HBRUSH)(COLOR_BTNFACE + 1);
    wc.lpszClassName = BPDF_GUI_CLASS;
    RegisterClassExW(&wc);

    // Created hidden: a single action shows it only when bpdf.exe runs for a
    // while or ends with something to read; a cancelled {ask:...} dialog or a
    // quick clean run leaves no window behind
    HWND hwnd = CreateWindowExW(
        WS_EX_APPWINDOW,
        BPDF_GUI_CLASS,
        Tr(L"bpdf — Панель действий", L"bpdf — Actions"),
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX,
        CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT,
        NULL, NULL, hInst, &ctx
    );
    if (!hwnd) return 1;

    CenterOnMonitor(hwnd);
    if (ctx.isPaletteSession) {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }

    MSG msg;
    while (GetMessageW(&msg, NULL, 0, 0)) {
        if (!IsDialogMessageW(hwnd, &msg)) {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    return 0;
}
