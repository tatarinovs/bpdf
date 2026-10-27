#include "bpdf_shell_common.h"
#include <commctrl.h>
#include <commdlg.h>

static const wchar_t* BPDF_GUI_CLASS = L"BpdfGuiWindow";

#define WM_BPDF_APPEND   (WM_APP + 1)
#define WM_BPDF_FINISHED (WM_APP + 2)

#define ID_TIMER_AUTOCLOSE 101

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
    HWND hwnd;
    GuiState state;
    bool isPaletteSession;

    // Palette controls
    HWND hTargetTitle;
    HWND hTargetSub;
    HWND hPaletteGroup;
    HWND hActionBtns[24];
    HIMAGELIST hActionIml[24];
    MenuItemDef actionDefs[24];
    size_t actionCount;

    // Progress / Execution controls
    HWND hTitleStatic;
    HWND hStatusStatic;
    HWND hProgress;
    HWND hEdit;
    HWND hBtnClose;
    HWND hBtnFolder;
    HWND hBtnBrowse;
    HWND hBtnBack;

    HFONT hFontTitle;
    HFONT hFontSub;
    HFONT hFontButton;
    HFONT hFontUi;
    HFONT hFontEdit;
    HBRUSH hBrushEdit;
    HICON hIcon;

    WStringList files;
    wchar_t targetExe[MAX_PATH * 2];
    wchar_t currentTitle[128];
    wchar_t* cmdLine;
    wchar_t workDir[MAX_PATH * 2];
    wchar_t tempManifest[MAX_PATH * 2];

    int iconIndex;
    GuiMode guiMode;
    int autoCloseSeconds;
    int dpi;

    HANDLE hProcess;
    HANDLE hWorkerThread;
    volatile LONG isFinished;
    volatile LONG isCancelled;

    GuiContext()
        : hwnd(NULL), state(STATE_PALETTE), isPaletteSession(false),
          hTargetTitle(NULL), hTargetSub(NULL), hPaletteGroup(NULL), actionCount(0),
          hTitleStatic(NULL), hStatusStatic(NULL), hProgress(NULL),
          hEdit(NULL), hBtnClose(NULL), hBtnFolder(NULL), hBtnBrowse(NULL), hBtnBack(NULL),
          hFontTitle(NULL), hFontSub(NULL), hFontButton(NULL), hFontUi(NULL), hFontEdit(NULL),
          hBrushEdit(NULL), hIcon(NULL),
          cmdLine(NULL), iconIndex(0), guiMode(GUI_AUTO), autoCloseSeconds(0), dpi(96),
          hProcess(NULL), hWorkerThread(NULL), isFinished(0), isCancelled(0)
    {
        for (int i = 0; i < 24; i++) {
            hActionBtns[i] = NULL;
            hActionIml[i] = NULL;
        }
        targetExe[0] = L'\0';
        currentTitle[0] = L'\0';
        workDir[0] = L'\0';
        tempManifest[0] = L'\0';
    }

    ~GuiContext()
    {
        if (hWorkerThread) {
            WaitForSingleObject(hWorkerThread, 3000);
            CloseHandle(hWorkerThread);
        }
        if (hProcess) CloseHandle(hProcess);
        if (cmdLine) free(cmdLine);
        if (tempManifest[0]) DeleteFileW(tempManifest);
        if (hIcon) DestroyIcon(hIcon);

        for (size_t i = 0; i < 24; i++) {
            if (hActionIml[i]) ImageList_Destroy(hActionIml[i]);
        }

        if (hFontTitle) DeleteObject(hFontTitle);
        if (hFontSub) DeleteObject(hFontSub);
        if (hFontButton) DeleteObject(hFontButton);
        if (hFontUi) DeleteObject(hFontUi);
        if (hFontEdit) DeleteObject(hFontEdit);
        if (hBrushEdit) DeleteObject(hBrushEdit);
    }

    int scale(int val) const {
        return MulDiv(val, dpi, 96);
    }
};

// -----------------------------------------------------------------------------
// Layout & UI Transitions
// -----------------------------------------------------------------------------

static void LayoutControls(HWND hwnd, GuiContext* ctx)
{
    if (!ctx) return;
    RECT rc;
    GetClientRect(hwnd, &rc);
    int w = rc.right - rc.left;
    int h = rc.bottom - rc.top;

    int m = ctx->scale(16);
    int btnW = ctx->scale(110);
    int btnH = ctx->scale(28);

    HDWP hdwp = BeginDeferWindowPos(32);

    if (ctx->state == STATE_PALETTE) {
        int titleH = ctx->scale(24);
        int subH = ctx->scale(18);
        if (ctx->hTargetTitle) {
            hdwp = DeferWindowPos(hdwp, ctx->hTargetTitle, NULL, m, m, w - m * 2, titleH, SWP_NOZORDER);
        }
        if (ctx->hTargetSub) {
            hdwp = DeferWindowPos(hdwp, ctx->hTargetSub, NULL, m, m + titleH + ctx->scale(2), w - m * 2, subH, SWP_NOZORDER);
        }

        int startY = m + titleH + subH + ctx->scale(14);
        int actionBtnH = ctx->scale(36);
        int gap = ctx->scale(8);

        int cols = (ctx->actionCount > 5) ? 2 : 1;
        int colW = (cols == 2) ? (w - m * 2 - gap) / 2 : (w - m * 2);

        for (size_t i = 0; i < ctx->actionCount; i++) {
            if (ctx->hActionBtns[i]) {
                int col = (int)(i % cols);
                int row = (int)(i / cols);
                int x = m + col * (colW + gap);
                int y = startY + row * (actionBtnH + gap);
                hdwp = DeferWindowPos(hdwp, ctx->hActionBtns[i], NULL, x, y, colW, actionBtnH, SWP_NOZORDER | SWP_SHOWWINDOW);
            }
        }

        int rows = (ctx->actionCount > 5) ? (int)((ctx->actionCount + 1) / 2) : (int)ctx->actionCount;
        if (rows < 1) rows = 1;
        int lastBtnBottom = startY + rows * actionBtnH + (rows - 1) * gap;
        int bottomGap = ctx->scale(18);
        int bottomY = lastBtnBottom + bottomGap;

        if (ctx->hBtnBrowse) {
            hdwp = DeferWindowPos(hdwp, ctx->hBtnBrowse, NULL, m, bottomY, ctx->scale(140), btnH, SWP_NOZORDER | SWP_SHOWWINDOW);
        }
        if (ctx->hBtnClose) {
            hdwp = DeferWindowPos(hdwp, ctx->hBtnClose, NULL, w - m - btnW, bottomY, btnW, btnH, SWP_NOZORDER | SWP_SHOWWINDOW);
        }
    } else {
        // STATE_RUNNING / STATE_FINISHED (Progress Mode)
        int titleH = ctx->scale(22);
        int statusH = ctx->scale(18);
        int progressH = ctx->scale(14);

        if (ctx->hTitleStatic) {
            hdwp = DeferWindowPos(hdwp, ctx->hTitleStatic, NULL, m, ctx->scale(12), w - m * 2, titleH, SWP_NOZORDER | SWP_SHOWWINDOW);
        }
        if (ctx->hStatusStatic) {
            hdwp = DeferWindowPos(hdwp, ctx->hStatusStatic, NULL, m, ctx->scale(36), w - m * 2, statusH, SWP_NOZORDER | SWP_SHOWWINDOW);
        }

        int editY = ctx->scale(58);
        if (ctx->hProgress && ctx->guiMode != GUI_INFO) {
            hdwp = DeferWindowPos(hdwp, ctx->hProgress, NULL, m, ctx->scale(58), w - m * 2, progressH, SWP_NOZORDER | SWP_SHOWWINDOW);
            editY = ctx->scale(80);
        }

        int btnY = h - m - btnH;
        int editH = btnY - editY - ctx->scale(10);
        if (editH < ctx->scale(40)) editH = ctx->scale(40);

        if (ctx->hEdit) {
            hdwp = DeferWindowPos(hdwp, ctx->hEdit, NULL, m, editY, w - m * 2, editH, SWP_NOZORDER | SWP_SHOWWINDOW);
        }

        int curX = m;
        if (ctx->hBtnBack) {
            int backBtnW = ctx->scale(150);
            hdwp = DeferWindowPos(hdwp, ctx->hBtnBack, NULL, curX, btnY, backBtnW, btnH, SWP_NOZORDER | (ctx->isFinished ? SWP_SHOWWINDOW : SWP_HIDEWINDOW));
            curX += backBtnW + ctx->scale(8);
        }
        if (ctx->hBtnFolder) {
            int folderBtnW = ctx->scale(130);
            hdwp = DeferWindowPos(hdwp, ctx->hBtnFolder, NULL, curX, btnY, folderBtnW, btnH, SWP_NOZORDER | (ctx->isFinished ? SWP_SHOWWINDOW : SWP_HIDEWINDOW));
        }
        if (ctx->hBtnClose) {
            hdwp = DeferWindowPos(hdwp, ctx->hBtnClose, NULL, w - m - btnW, btnY, btnW, btnH, SWP_NOZORDER | SWP_SHOWWINDOW);
        }
    }

    EndDeferWindowPos(hdwp);
}

// -----------------------------------------------------------------------------
// Background Worker Thread (Pipe Streaming)
// -----------------------------------------------------------------------------

static DWORD WINAPI WorkerThreadProc(LPVOID param)
{
    GuiContext* ctx = (GuiContext*)param;
    if (!ctx || !ctx->cmdLine) return 1;

    HANDLE hReadPipe = NULL;
    HANDLE hWritePipe = NULL;
    SECURITY_ATTRIBUTES sa = { sizeof(sa), NULL, TRUE };

    if (!CreatePipe(&hReadPipe, &hWritePipe, &sa, 0)) {
        PostMessageW(ctx->hwnd, WM_BPDF_FINISHED, 1, 0);
        return 1;
    }
    SetHandleInformation(hReadPipe, HANDLE_FLAG_INHERIT, 0);

    // Create a NUL stdin handle so bpdf.exe does not inherit console/blocking stdin
    HANDLE hNullInput = CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0, NULL);

    STARTUPINFOW si = { sizeof(si) };
    si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_HIDE;
    si.hStdOutput = hWritePipe;
    si.hStdError = hWritePipe;
    si.hStdInput = (hNullInput != INVALID_HANDLE_VALUE) ? hNullInput : NULL;

    PROCESS_INFORMATION pi = { 0 };

    SetEnvironmentVariableW(L"BPDF_NO_PAUSE", L"1");

    BOOL spawned = CreateProcessW(
        NULL,
        ctx->cmdLine,
        NULL,
        NULL,
        TRUE,
        CREATE_NO_WINDOW,
        NULL,
        ctx->workDir[0] ? ctx->workDir : NULL,
        &si,
        &pi
    );

    CloseHandle(hWritePipe);
    if (hNullInput != INVALID_HANDLE_VALUE) CloseHandle(hNullInput);

    if (!spawned) {
        CloseHandle(hReadPipe);
        PostMessageW(ctx->hwnd, WM_BPDF_FINISHED, 1, 0);
        return 1;
    }

    ctx->hProcess = pi.hProcess;
    CloseHandle(pi.hThread);

    char rawBuf[2048];
    char carryBuf[8];
    int carryLen = 0;
    DWORD bytesRead = 0;

    while (ReadFile(hReadPipe, rawBuf, sizeof(rawBuf) - 8, &bytesRead, NULL) && bytesRead > 0) {
        if (ctx->isCancelled) break;

        char procBuf[sizeof(rawBuf) + 8];
        int procLen = 0;
        if (carryLen > 0) {
            memcpy(procBuf, carryBuf, carryLen);
            procLen = carryLen;
            carryLen = 0;
        }
        memcpy(procBuf + procLen, rawBuf, bytesRead);
        procLen += (int)bytesRead;

        int validLen = procLen;
        while (validLen > 0 && validLen > procLen - 4) {
            unsigned char c = (unsigned char)procBuf[validLen - 1];
            if ((c & 0x80) == 0) break;
            if ((c & 0xC0) == 0xC0) {
                int seqLen = ((c & 0xE0) == 0xC0) ? 2 : ((c & 0xF0) == 0xE0) ? 3 : 4;
                if (procLen - (validLen - 1) < seqLen) {
                    validLen = validLen - 1;
                }
                break;
            }
            validLen--;
        }

        if (validLen < procLen) {
            carryLen = procLen - validLen;
            if (carryLen > (int)sizeof(carryBuf)) carryLen = (int)sizeof(carryBuf);
            memcpy(carryBuf, procBuf + validLen, carryLen);
        }

        if (validLen > 0) {
            int wChars = MultiByteToWideChar(CP_UTF8, 0, procBuf, validLen, NULL, 0);
            if (wChars > 0) {
                wchar_t* wBuf = (wchar_t*)malloc((wChars + 1) * sizeof(wchar_t));
                if (wBuf) {
                    MultiByteToWideChar(CP_UTF8, 0, procBuf, validLen, wBuf, wChars);
                    wBuf[wChars] = L'\0';

                    int normLen = 0;
                    wchar_t* normBuf = NormalizeLineEndings(wBuf, wChars, normLen);
                    free(wBuf);
                    if (normBuf) {
                        if (!PostMessageW(ctx->hwnd, WM_BPDF_APPEND, 0, (LPARAM)normBuf)) {
                            free(normBuf);
                        }
                    }
                }
            }
        }
    }

    CloseHandle(hReadPipe);

    DWORD exitCode = 0;
    WaitForSingleObject(pi.hProcess, INFINITE);
    GetExitCodeProcess(pi.hProcess, &exitCode);

    PostMessageW(ctx->hwnd, WM_BPDF_FINISHED, exitCode, 0);
    return exitCode;
}

// -----------------------------------------------------------------------------
// Load Actions for Selected Files & Adjust Window Size
// -----------------------------------------------------------------------------

static void LoadPaletteForFiles(GuiContext* ctx)
{
    // Clean up old action buttons
    for (size_t i = 0; i < 24; i++) {
        if (ctx->hActionBtns[i]) {
            DestroyWindow(ctx->hActionBtns[i]);
            ctx->hActionBtns[i] = NULL;
        }
        if (ctx->hActionIml[i]) {
            ImageList_Destroy(ctx->hActionIml[i]);
            ctx->hActionIml[i] = NULL;
        }
    }
    ctx->actionCount = 0;

    if (ctx->files.empty()) {
        SetWindowTextW(ctx->hTargetTitle, L"Файлы не выбраны");
        SetWindowTextW(ctx->hTargetSub, L"Перетащите файлы сюда (Drag & Drop) или нажмите «Выбрать файлы...»");
        if (ctx->hBtnBrowse) ShowWindow(ctx->hBtnBrowse, SW_SHOW);
        LayoutControls(ctx->hwnd, ctx);
        return;
    }

    // Set summary in header
    if (ctx->files.size() == 1) {
        wchar_t name[MAX_PATH * 2];
        lstrcpynW(name, PathFindFileNameW(ctx->files[0]), ARRAYSIZE(name));
        SetWindowTextW(ctx->hTargetTitle, name);

        WIN32_FILE_ATTRIBUTE_DATA fad;
        wchar_t subInfo[512] = {0};
        if (GetFileAttributesExW(ctx->files[0], GetFileExInfoStandard, &fad)) {
            if (fad.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
                wnsprintfW(subInfo, ARRAYSIZE(subInfo), L"Папка: %s", ctx->files[0]);
            } else {
                ULARGE_INTEGER uli;
                uli.LowPart = fad.nFileSizeLow;
                uli.HighPart = fad.nFileSizeHigh;
                DWORD sizeMbInt = (DWORD)(uli.QuadPart / (1024 * 1024));
                DWORD sizeMbDec = (DWORD)(((uli.QuadPart % (1024 * 1024)) * 10) / (1024 * 1024));
                if (sizeMbInt > 0) {
                    wnsprintfW(subInfo, ARRAYSIZE(subInfo), L"Размер: %u.%u МБ  |  %s", sizeMbInt, sizeMbDec, ctx->files[0]);
                } else {
                    DWORD sizeKb = (DWORD)(uli.QuadPart / 1024);
                    wnsprintfW(subInfo, ARRAYSIZE(subInfo), L"Размер: %u КБ  |  %s", sizeKb, ctx->files[0]);
                }
            }
        } else {
            lstrcpynW(subInfo, ctx->files[0], ARRAYSIZE(subInfo));
        }
        SetWindowTextW(ctx->hTargetSub, subInfo);
    } else {
        wchar_t summary[256];
        wnsprintfW(summary, ARRAYSIZE(summary), L"Выбрано файлов: %d", (int)ctx->files.size());
        SetWindowTextW(ctx->hTargetTitle, summary);

        wchar_t dir[MAX_PATH * 2];
        lstrcpynW(dir, ctx->files[0], ARRAYSIZE(dir));
        PathRemoveFileSpecW(dir);
        wchar_t subInfo[512];
        wnsprintfW(subInfo, ARRAYSIZE(subInfo), L"Папка: %s", dir);
        SetWindowTextW(ctx->hTargetSub, subInfo);
    }

    // Determine INI section
    wchar_t section[64];
    GetSectionForFiles(ctx->files, section, ARRAYSIZE(section));

    // Read INI
    wchar_t iniPath[MAX_PATH * 2] = {0};
    StrW iniContent;
    if (FindBpdfIni(iniPath, ARRAYSIZE(iniPath))) {
        ReadTextFile(iniPath, iniContent);
    }

    MenuItemDef items[24];
    size_t count = ParseIniSection(iniContent.c_str(), section, items, ARRAYSIZE(items));

    if (count == 0 && iniContent.length() == 0) {
        SetWindowTextW(ctx->hTargetSub, L"Файл конфигурации bpdf_shell.ini не найден");
    }

    // Create buttons for non-separator items
    size_t btnIndex = 0;
    for (size_t i = 0; i < count && btnIndex < 24; i++) {
        if (items[i].iconIndex == -1 || items[i].title[0] == L'\0') {
            continue;
        }

        ctx->actionDefs[btnIndex] = items[i];

        HWND hBtn = CreateWindowExW(
            0,
            L"BUTTON",
            items[i].title,
            WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON | WS_TABSTOP,
            0, 0, 100, 30,
            ctx->hwnd,
            (HMENU)(INT_PTR)(ID_ACTION_BASE + btnIndex),
            (HINSTANCE)GetWindowLongPtrW(ctx->hwnd, GWLP_HINSTANCE),
            NULL
        );

        if (hBtn) {
            SendMessageW(hBtn, WM_SETFONT, (WPARAM)ctx->hFontButton, TRUE);

            // Set button icon if available from targetExe
            if (ctx->targetExe[0] && items[i].iconIndex >= 0) {
                HICON hIconSmall = NULL;
                if (ExtractIconExW(ctx->targetExe, items[i].iconIndex, NULL, &hIconSmall, 1) > 0 && hIconSmall) {
                    HIMAGELIST himl = ImageList_Create(16, 16, ILC_COLOR32 | ILC_MASK, 1, 1);
                    if (himl) {
                        ImageList_AddIcon(himl, hIconSmall);
                        BUTTON_IMAGELIST bil = {0};
                        bil.himl = himl;
                        bil.margin.left = ctx->scale(8);
                        bil.uAlign = BUTTON_IMAGELIST_ALIGN_LEFT;
                        SendMessageW(hBtn, BCM_SETIMAGELIST, 0, (LPARAM)&bil);
                        ctx->hActionIml[btnIndex] = himl;
                    }
                    DestroyIcon(hIconSmall);
                }
            }

            ctx->hActionBtns[btnIndex] = hBtn;
            btnIndex++;
        }
    }
    ctx->actionCount = btnIndex;

    // Auto-adjust window height accurately based on buttons grid
    int rows = (ctx->actionCount > 5) ? (int)((ctx->actionCount + 1) / 2) : (int)ctx->actionCount;
    if (rows < 1) rows = 1;

    int m = ctx->scale(16);
    int titleH = ctx->scale(24);
    int subH = ctx->scale(18);
    int startY = m + titleH + subH + ctx->scale(14);
    int actionBtnH = ctx->scale(36);
    int gap = ctx->scale(8);

    int lastBtnBottom = startY + rows * actionBtnH + (rows - 1) * gap;
    int bottomGap = ctx->scale(18);
    int btnH = ctx->scale(28);
    int neededClientH = lastBtnBottom + bottomGap + btnH + m;

    RECT curRc;
    GetWindowRect(ctx->hwnd, &curRc);
    int curW = curRc.right - curRc.left;

    RECT adjRc = { 0, 0, curW, neededClientH };
    DWORD style = (DWORD)GetWindowLongPtrW(ctx->hwnd, GWL_STYLE);
    DWORD exStyle = (DWORD)GetWindowLongPtrW(ctx->hwnd, GWL_EXSTYLE);
    AdjustWindowRectEx(&adjRc, style, FALSE, exStyle);

    int totalWindowH = adjRc.bottom - adjRc.top;
    SetWindowPos(ctx->hwnd, NULL, 0, 0, curW, totalWindowH, SWP_NOMOVE | SWP_NOZORDER);

    LayoutControls(ctx->hwnd, ctx);
}

// -----------------------------------------------------------------------------
// Return to Action Palette
// -----------------------------------------------------------------------------

static void ReturnToPalette(GuiContext* ctx)
{
    if (!ctx) return;
    KillTimer(ctx->hwnd, ID_TIMER_AUTOCLOSE);

    if (ctx->hWorkerThread) {
        WaitForSingleObject(ctx->hWorkerThread, 1000);
        CloseHandle(ctx->hWorkerThread);
        ctx->hWorkerThread = NULL;
    }
    if (ctx->hProcess) {
        CloseHandle(ctx->hProcess);
        ctx->hProcess = NULL;
    }

    if (ctx->tempManifest[0]) {
        DeleteFileW(ctx->tempManifest);
        ctx->tempManifest[0] = L'\0';
    }

    ctx->state = STATE_PALETTE;
    ctx->isPaletteSession = true;
    ctx->isFinished = 0;
    ctx->isCancelled = 0;

    // Reset window icon back to logo
    if (ctx->targetExe[0]) {
        HICON hLogo = NULL;
        if (ExtractIconExW(ctx->targetExe, 0, &hLogo, NULL, 1) > 0 && hLogo) {
            if (ctx->hIcon) DestroyIcon(ctx->hIcon);
            ctx->hIcon = hLogo;
            SendMessageW(ctx->hwnd, WM_SETICON, ICON_BIG, (LPARAM)ctx->hIcon);
            SendMessageW(ctx->hwnd, WM_SETICON, ICON_SMALL, (LPARAM)ctx->hIcon);
        }
    }

    // Hide execution controls
    if (ctx->hTitleStatic) ShowWindow(ctx->hTitleStatic, SW_HIDE);
    if (ctx->hStatusStatic) ShowWindow(ctx->hStatusStatic, SW_HIDE);
    if (ctx->hProgress) ShowWindow(ctx->hProgress, SW_HIDE);
    if (ctx->hEdit) ShowWindow(ctx->hEdit, SW_HIDE);
    if (ctx->hBtnFolder) ShowWindow(ctx->hBtnFolder, SW_HIDE);
    if (ctx->hBtnBack) ShowWindow(ctx->hBtnBack, SW_HIDE);

    // Clear edit log
    if (ctx->hEdit) SetWindowTextW(ctx->hEdit, L"");

    // Show palette controls
    if (ctx->hTargetTitle) ShowWindow(ctx->hTargetTitle, SW_SHOW);
    if (ctx->hTargetSub) ShowWindow(ctx->hTargetSub, SW_SHOW);
    if (ctx->hBtnBrowse) ShowWindow(ctx->hBtnBrowse, SW_SHOW);
    if (ctx->hBtnClose) {
        ShowWindow(ctx->hBtnClose, SW_SHOW);
        SetWindowTextW(ctx->hBtnClose, L"Закрыть");
    }

    SetWindowTextW(ctx->hwnd, L"bpdf — Панель действий");

    // Reload palette (re-reads file sizes and recreates buttons)
    LoadPaletteForFiles(ctx);
}

// -----------------------------------------------------------------------------
// Transition: Start an Action from Palette or Job
// -----------------------------------------------------------------------------

static void StartActionExecution(GuiContext* ctx, const MenuItemDef& item)
{
    if (ctx->files.empty() || !ctx->targetExe[0]) return;

    // Clean up previous temporary manifest if one was created
    if (ctx->tempManifest[0]) {
        DeleteFileW(ctx->tempManifest);
        ctx->tempManifest[0] = L'\0';
    }

    // Expand command template
    wchar_t fullPath[MAX_PATH * 2] = {0};
    wchar_t dir[MAX_PATH * 2] = {0};
    wchar_t name[MAX_PATH * 2] = {0};
    wchar_t ext[64] = {0};

    lstrcpynW(fullPath, ctx->files[0], ARRAYSIZE(fullPath));
    lstrcpynW(dir, ctx->files[0], ARRAYSIZE(dir));
    PathRemoveFileSpecW(dir);

    SanitizePathForQuotes(fullPath);
    SanitizePathForQuotes(dir);

    lstrcpynW(name, PathFindFileNameW(ctx->files[0]), ARRAYSIZE(name));
    const wchar_t* dot = PathFindExtensionW(name);
    if (dot && dot[0] == L'.') {
        lstrcpynW(ext, dot + 1, ARRAYSIZE(ext));
        PathRemoveExtensionW(name);
    }

    StrW expanded;
    bool hasFilePlaceholder = false;
    ExpandCommand(item.cmd, fullPath, dir, name, ext, expanded, hasFilePlaceholder);

    wchar_t manifestPath[MAX_PATH * 2] = {0};
    StrW fullCmdLine;
    fullCmdLine.append(L"\"");
    fullCmdLine.append(ctx->targetExe);
    fullCmdLine.append(L"\" --no-pause ");
    fullCmdLine.append(expanded.c_str());

    if (!hasFilePlaceholder) {
        fullCmdLine.append(L" \"");
        if (ctx->files.size() > 1) {
            if (!CreateManifestFile(ctx->files, manifestPath, ARRAYSIZE(manifestPath))) {
                MessageBoxW(ctx->hwnd, L"Не удалось создать файл списка аргументов", L"bpdf", MB_ICONERROR);
                return;
            }
            fullCmdLine.append(L"@");
            fullCmdLine.append(manifestPath);
        } else {
            fullCmdLine.append(fullPath);
        }
        fullCmdLine.append(L"\"");
    }

    if (ctx->cmdLine) free(ctx->cmdLine);
    ctx->cmdLine = _wcsdup(fullCmdLine.c_str());
    lstrcpynW(ctx->workDir, dir, ARRAYSIZE(ctx->workDir));
    lstrcpynW(ctx->tempManifest, manifestPath, ARRAYSIZE(ctx->tempManifest));
    lstrcpynW(ctx->currentTitle, item.title, ARRAYSIZE(ctx->currentTitle));
    ctx->iconIndex = item.iconIndex;
    ctx->guiMode = item.guiMode;

    // Dynamically update window icon for current action
    if (ctx->targetExe[0] && item.iconIndex >= 0) {
        HICON hActionIcon = NULL;
        if (ExtractIconExW(ctx->targetExe, item.iconIndex, &hActionIcon, NULL, 1) > 0 && hActionIcon) {
            if (ctx->hIcon) DestroyIcon(ctx->hIcon);
            ctx->hIcon = hActionIcon;
            SendMessageW(ctx->hwnd, WM_SETICON, ICON_BIG, (LPARAM)ctx->hIcon);
            SendMessageW(ctx->hwnd, WM_SETICON, ICON_SMALL, (LPARAM)ctx->hIcon);
        }
    }

    // Hide Palette controls
    if (ctx->hTargetTitle) ShowWindow(ctx->hTargetTitle, SW_HIDE);
    if (ctx->hTargetSub) ShowWindow(ctx->hTargetSub, SW_HIDE);
    if (ctx->hBtnBrowse) ShowWindow(ctx->hBtnBrowse, SW_HIDE);
    if (ctx->hBtnBack) ShowWindow(ctx->hBtnBack, SW_HIDE);
    if (ctx->hBtnFolder) ShowWindow(ctx->hBtnFolder, SW_HIDE);
    for (size_t i = 0; i < ctx->actionCount; i++) {
        if (ctx->hActionBtns[i]) ShowWindow(ctx->hActionBtns[i], SW_HIDE);
    }

    // Switch state
    ctx->state = STATE_RUNNING;

    SetWindowTextW(ctx->hTitleStatic, item.title);

    wchar_t statusText[256];
    wnsprintfW(statusText, ARRAYSIZE(statusText), L"Выполняется: %s...", item.title);
    SetWindowTextW(ctx->hStatusStatic, statusText);

    wchar_t winTitle[256];
    wnsprintfW(winTitle, ARRAYSIZE(winTitle), L"bpdf — %s", item.title);
    SetWindowTextW(ctx->hwnd, winTitle);

    if (ctx->guiMode != GUI_INFO && ctx->hProgress) {
        ShowWindow(ctx->hProgress, SW_SHOW);
        SendMessageW(ctx->hProgress, PBM_SETMARQUEE, TRUE, 30);
    }

    ShowWindow(ctx->hTitleStatic, SW_SHOW);
    ShowWindow(ctx->hStatusStatic, SW_SHOW);
    ShowWindow(ctx->hEdit, SW_SHOW);
    ShowWindow(ctx->hBtnClose, SW_SHOW);
    SetWindowTextW(ctx->hBtnClose, L"Отмена");

    RECT curRc;
    GetWindowRect(ctx->hwnd, &curRc);
    int curW = curRc.right - curRc.left;
    int minProgressH = ctx->scale(440);
    if (curRc.bottom - curRc.top < minProgressH) {
        SetWindowPos(ctx->hwnd, NULL, 0, 0, curW, minProgressH, SWP_NOMOVE | SWP_NOZORDER);
    }

    LayoutControls(ctx->hwnd, ctx);

    ctx->hWorkerThread = CreateThread(NULL, 0, WorkerThreadProc, ctx, 0, NULL);
    if (!ctx->hWorkerThread) {
        MessageBoxW(ctx->hwnd, L"Не удалось запустить процесс bpdf.exe", L"bpdf", MB_ICONERROR);
        PostMessageW(ctx->hwnd, WM_BPDF_FINISHED, 1, 0);
    }
}

// -----------------------------------------------------------------------------
// Read Job File (Direct Command Mode)
// -----------------------------------------------------------------------------

static bool ReadJobFile(const wchar_t* path, GuiContext* ctx)
{
    return ReadJobFile(
        path,
        ctx->currentTitle, ARRAYSIZE(ctx->currentTitle),
        &ctx->iconIndex,
        &ctx->guiMode,
        ctx->workDir, ARRAYSIZE(ctx->workDir),
        ctx->tempManifest, ARRAYSIZE(ctx->tempManifest),
        ctx->targetExe, ARRAYSIZE(ctx->targetExe),
        &ctx->cmdLine,
        &ctx->files
    );
}

// -----------------------------------------------------------------------------
// Window Procedure
// -----------------------------------------------------------------------------

static LRESULT CALLBACK GuiWndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam)
{
    GuiContext* ctx = (GuiContext*)GetWindowLongPtrW(hwnd, GWLP_USERDATA);

    switch (msg) {
    case WM_CREATE: {
        CREATESTRUCTW* cs = (CREATESTRUCTW*)lParam;
        ctx = (GuiContext*)cs->lpCreateParams;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (LONG_PTR)ctx);
        ctx->hwnd = hwnd;

        if (ctx->hIcon) {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG, (LPARAM)ctx->hIcon);
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL, (LPARAM)ctx->hIcon);
        }

        HINSTANCE hInst = cs->hInstance;

        ctx->hFontTitle = CreateFontW(-ctx->scale(18), 0, 0, 0, FW_SEMIBOLD, FALSE, FALSE, FALSE,
            DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
        ctx->hFontSub = CreateFontW(-ctx->scale(12), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
            DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
        ctx->hFontButton = CreateFontW(-ctx->scale(13), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
            DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
        ctx->hFontUi = CreateFontW(-ctx->scale(13), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
            DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
        ctx->hFontEdit = CreateFontW(-ctx->scale(12), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
            DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, FIXED_PITCH, L"Consolas");
        ctx->hBrushEdit = CreateSolidBrush(RGB(24, 24, 24));

        // Header controls (Palette mode)
        ctx->hTargetTitle = CreateWindowExW(0, L"STATIC", L"", WS_CHILD | SS_LEFT | SS_ENDELLIPSIS,
            0, 0, 10, 10, hwnd, NULL, hInst, NULL);
        SendMessageW(ctx->hTargetTitle, WM_SETFONT, (WPARAM)ctx->hFontTitle, TRUE);

        ctx->hTargetSub = CreateWindowExW(0, L"STATIC", L"", WS_CHILD | SS_LEFT | SS_ENDELLIPSIS,
            0, 0, 10, 10, hwnd, NULL, hInst, NULL);
        SendMessageW(ctx->hTargetSub, WM_SETFONT, (WPARAM)ctx->hFontSub, TRUE);

        // Progress controls
        ctx->hTitleStatic = CreateWindowExW(0, L"STATIC", L"bpdf", WS_CHILD | SS_LEFT | SS_ENDELLIPSIS,
            0, 0, 10, 10, hwnd, NULL, hInst, NULL);
        SendMessageW(ctx->hTitleStatic, WM_SETFONT, (WPARAM)ctx->hFontTitle, TRUE);

        ctx->hStatusStatic = CreateWindowExW(0, L"STATIC", L"Подготовка...", WS_CHILD | SS_LEFT | SS_ENDELLIPSIS,
            0, 0, 10, 10, hwnd, NULL, hInst, NULL);
        SendMessageW(ctx->hStatusStatic, WM_SETFONT, (WPARAM)ctx->hFontUi, TRUE);

        ctx->hProgress = CreateWindowExW(0, PROGRESS_CLASSW, NULL, WS_CHILD | PBS_MARQUEE,
            0, 0, 10, 10, hwnd, NULL, hInst, NULL);

        ctx->hEdit = CreateWindowExW(WS_EX_CLIENTEDGE, L"EDIT", L"",
            WS_CHILD | WS_VSCROLL | ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY,
            0, 0, 10, 10, hwnd, NULL, hInst, NULL);
        SendMessageW(ctx->hEdit, WM_SETFONT, (WPARAM)ctx->hFontEdit, TRUE);

        ctx->hBtnClose = CreateWindowExW(0, L"BUTTON", L"Закрыть", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON,
            0, 0, 10, 10, hwnd, (HMENU)(INT_PTR)ID_BTN_CLOSE, hInst, NULL);
        SendMessageW(ctx->hBtnClose, WM_SETFONT, (WPARAM)ctx->hFontUi, TRUE);

        ctx->hBtnFolder = CreateWindowExW(0, L"BUTTON", L"Открыть папку", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON,
            0, 0, 10, 10, hwnd, (HMENU)(INT_PTR)ID_BTN_FOLDER, hInst, NULL);
        SendMessageW(ctx->hBtnFolder, WM_SETFONT, (WPARAM)ctx->hFontUi, TRUE);

        ctx->hBtnBrowse = CreateWindowExW(0, L"BUTTON", L"Выбрать файлы...", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON,
            0, 0, 10, 10, hwnd, (HMENU)(INT_PTR)ID_BTN_BROWSE, hInst, NULL);
        SendMessageW(ctx->hBtnBrowse, WM_SETFONT, (WPARAM)ctx->hFontUi, TRUE);

        ctx->hBtnBack = CreateWindowExW(0, L"BUTTON", L"← К действиям", WS_CHILD | WS_TABSTOP | BS_PUSHBUTTON,
            0, 0, 10, 10, hwnd, (HMENU)(INT_PTR)ID_BTN_BACK, hInst, NULL);
        SendMessageW(ctx->hBtnBack, WM_SETFONT, (WPARAM)ctx->hFontUi, TRUE);

        DragAcceptFiles(hwnd, TRUE);

        if (ctx->state == STATE_PALETTE) {
            ShowWindow(ctx->hTargetTitle, SW_SHOW);
            ShowWindow(ctx->hTargetSub, SW_SHOW);
            ShowWindow(ctx->hBtnClose, SW_SHOW);
            LoadPaletteForFiles(ctx);
        } else {
            // STATE_RUNNING (Direct job mode)
            if (ctx->currentTitle[0]) {
                SetWindowTextW(ctx->hTitleStatic, ctx->currentTitle);
            }
            ShowWindow(ctx->hTitleStatic, SW_SHOW);
            ShowWindow(ctx->hStatusStatic, SW_SHOW);
            ShowWindow(ctx->hEdit, SW_SHOW);
            ShowWindow(ctx->hBtnClose, SW_SHOW);
            SetWindowTextW(ctx->hBtnClose, L"Отмена");

            wchar_t statusText[256];
            wnsprintfW(statusText, ARRAYSIZE(statusText), L"Выполняется: %s...", ctx->currentTitle);
            SetWindowTextW(ctx->hStatusStatic, statusText);

            if (ctx->guiMode != GUI_INFO && ctx->hProgress) {
                ShowWindow(ctx->hProgress, SW_SHOW);
                SendMessageW(ctx->hProgress, PBM_SETMARQUEE, TRUE, 30);
            }

            ctx->hWorkerThread = CreateThread(NULL, 0, WorkerThreadProc, ctx, 0, NULL);
        }

        LayoutControls(hwnd, ctx);
        return 0;
    }

    case WM_DROPFILES: {
        HDROP hDrop = (HDROP)wParam;
        UINT fileCount = DragQueryFileW(hDrop, 0xFFFFFFFF, NULL, 0);
        if (fileCount > 0 && ctx && ctx->state == STATE_PALETTE) {
            ctx->files.clear();
            for (UINT i = 0; i < fileCount; i++) {
                wchar_t filePath[MAX_PATH * 2] = {0};
                if (DragQueryFileW(hDrop, i, filePath, ARRAYSIZE(filePath)) > 0) {
                    ctx->files.add(filePath);
                }
            }
            LoadPaletteForFiles(ctx);
        }
        DragFinish(hDrop);
        return 0;
    }

    case WM_SIZE: {
        LayoutControls(hwnd, ctx);
        return 0;
    }

    case WM_CTLCOLOREDIT:
    case WM_CTLCOLORSTATIC: {
        if (ctx && (HWND)lParam == ctx->hEdit) {
            HDC hdc = (HDC)wParam;
            SetTextColor(hdc, RGB(220, 220, 220));
            SetBkColor(hdc, RGB(24, 24, 24));
            return (LRESULT)ctx->hBrushEdit;
        }
        break;
    }

    case WM_BPDF_APPEND: {
        wchar_t* text = (wchar_t*)lParam;
        if (text) {
            if (ctx && ctx->hEdit) {
                int len = GetWindowTextLengthW(ctx->hEdit);
                SendMessageW(ctx->hEdit, EM_SETSEL, len, len);
                SendMessageW(ctx->hEdit, EM_REPLACESEL, FALSE, (LPARAM)text);
            }
            free(text);
        }
        return 0;
    }

    case WM_BPDF_FINISHED: {
        if (!ctx) return 0;
        InterlockedExchange(&ctx->isFinished, 1);
        ctx->state = STATE_FINISHED;

        DWORD exitCode = (DWORD)wParam;

        if (ctx->hProgress) {
            SendMessageW(ctx->hProgress, PBM_SETMARQUEE, FALSE, 0);
            ShowWindow(ctx->hProgress, SW_HIDE);
        }

        if (ctx->hBtnFolder) {
            ShowWindow(ctx->hBtnFolder, SW_SHOW);
        }

        if (ctx->hBtnBack) {
            ShowWindow(ctx->hBtnBack, SW_SHOW);
            SetWindowTextW(ctx->hBtnBack, ctx->isPaletteSession ? L"← К действиям" : L"Панель действий...");
        }

        if (ctx->hBtnClose) {
            SetWindowTextW(ctx->hBtnClose, L"Закрыть");
        }

        if (exitCode == 0) {
            SetWindowTextW(ctx->hStatusStatic, L"Готово! Операция успешно завершена.");
            if (ctx->guiMode == GUI_AUTO) {
                ctx->autoCloseSeconds = 5;
                SetTimer(hwnd, ID_TIMER_AUTOCLOSE, 1000, NULL);
                wchar_t timerMsg[64];
                if (ctx->isPaletteSession) {
                    wnsprintfW(timerMsg, ARRAYSIZE(timerMsg), L"← К действиям (%dс)", ctx->autoCloseSeconds);
                    if (ctx->hBtnBack) SetWindowTextW(ctx->hBtnBack, timerMsg);
                } else {
                    wnsprintfW(timerMsg, ARRAYSIZE(timerMsg), L"Закрыть (%dс)", ctx->autoCloseSeconds);
                    if (ctx->hBtnClose) SetWindowTextW(ctx->hBtnClose, timerMsg);
                }
            }
        } else {
            SetWindowTextW(ctx->hStatusStatic, L"Завершено с ошибками (проверьте журнал вывода)");
        }

        LayoutControls(hwnd, ctx);
        return 0;
    }

    case WM_TIMER: {
        if (wParam == ID_TIMER_AUTOCLOSE && ctx) {
            ctx->autoCloseSeconds--;
            if (ctx->autoCloseSeconds <= 0) {
                KillTimer(hwnd, ID_TIMER_AUTOCLOSE);
                if (ctx->isPaletteSession) {
                    ReturnToPalette(ctx);
                } else {
                    DestroyWindow(hwnd);
                }
            } else {
                wchar_t timerMsg[64];
                if (ctx->isPaletteSession) {
                    wnsprintfW(timerMsg, ARRAYSIZE(timerMsg), L"← К действиям (%dс)", ctx->autoCloseSeconds);
                    if (ctx->hBtnBack) SetWindowTextW(ctx->hBtnBack, timerMsg);
                } else {
                    wnsprintfW(timerMsg, ARRAYSIZE(timerMsg), L"Закрыть (%dс)", ctx->autoCloseSeconds);
                    if (ctx->hBtnClose) SetWindowTextW(ctx->hBtnClose, timerMsg);
                }
            }
            return 0;
        }
        break;
    }

    case WM_COMMAND: {
        int id = LOWORD(wParam);

        // Click on Action Button in Palette mode
        if (id >= ID_ACTION_BASE && id < ID_ACTION_BASE + 24 && ctx && ctx->state == STATE_PALETTE) {
            size_t idx = id - ID_ACTION_BASE;
            if (idx < ctx->actionCount) {
                StartActionExecution(ctx, ctx->actionDefs[idx]);
            }
            return 0;
        }

        if (id == ID_BTN_BACK && ctx) {
            ReturnToPalette(ctx);
            return 0;
        }

        if (id == ID_BTN_BROWSE && ctx && ctx->state == STATE_PALETTE) {
            wchar_t fileBuf[32768] = {0};
            OPENFILENAMEW ofn = { sizeof(ofn) };
            ofn.hwndOwner = hwnd;
            ofn.lpstrFilter = L"Все поддерживаемые файлы\0*.pdf;*.docx;*.xlsx;*.pptx;*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.tiff;*.tif;*.avif;*.heic;*.djvu;*.epub;*.cbz;*.fb2;*.txt;*.md;*.html\0PDF документы (*.pdf)\0*.pdf\0Изображения\0*.png;*.jpg;*.jpeg;*.bmp;*.tiff;*.webp;*.avif;*.heic\0Все файлы (*.*)\0*.*\0";
            ofn.lpstrFile = fileBuf;
            ofn.nMaxFile = ARRAYSIZE(fileBuf);
            ofn.Flags = OFN_ALLOWMULTISELECT | OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;

            if (GetOpenFileNameW(&ofn)) {
                ctx->files.clear();
                wchar_t* p = fileBuf;
                wchar_t* dir = p;
                p += wcslen(dir) + 1;
                if (*p == L'\0') {
                    ctx->files.add(dir);
                } else {
                    while (*p) {
                        wchar_t full[MAX_PATH * 2];
                        wnsprintfW(full, ARRAYSIZE(full), L"%s\\%s", dir, p);
                        ctx->files.add(full);
                        p += wcslen(p) + 1;
                    }
                }
                LoadPaletteForFiles(ctx);
            }
            return 0;
        }

        if (id == ID_BTN_FOLDER && ctx) {
            KillTimer(hwnd, ID_TIMER_AUTOCLOSE);
            if (ctx->isPaletteSession && ctx->hBtnBack) {
                SetWindowTextW(ctx->hBtnBack, L"← К действиям");
            } else if (ctx->hBtnClose) {
                SetWindowTextW(ctx->hBtnClose, L"Закрыть");
            }

            if (ctx->workDir[0]) {
                ShellExecuteW(NULL, L"open", ctx->workDir, NULL, NULL, SW_SHOWNORMAL);
            } else if (!ctx->files.empty()) {
                wchar_t dir[MAX_PATH * 2];
                lstrcpynW(dir, ctx->files[0], ARRAYSIZE(dir));
                PathRemoveFileSpecW(dir);
                ShellExecuteW(NULL, L"open", dir, NULL, NULL, SW_SHOWNORMAL);
            }
            return 0;
        }

        if (HIWORD(wParam) == EN_SETFOCUS && ctx && (HWND)lParam == ctx->hEdit) {
            KillTimer(hwnd, ID_TIMER_AUTOCLOSE);
            if (ctx->isPaletteSession && ctx->hBtnBack) {
                SetWindowTextW(ctx->hBtnBack, L"← К действиям");
            } else if (ctx->hBtnClose) {
                SetWindowTextW(ctx->hBtnClose, L"Закрыть");
            }
        }

        if (id == ID_BTN_CLOSE || id == IDCANCEL) {
            if (ctx && ctx->state == STATE_RUNNING && !ctx->isFinished) {
                InterlockedExchange(&ctx->isCancelled, 1);
                if (ctx->hProcess) TerminateProcess(ctx->hProcess, 1);
            }
            DestroyWindow(hwnd);
            return 0;
        }

        if (id == IDOK) {
            if (ctx && (ctx->isFinished || ctx->guiMode != GUI_AUTO)) {
                if (ctx->isPaletteSession) {
                    ReturnToPalette(ctx);
                } else {
                    DestroyWindow(hwnd);
                }
            }
            return 0;
        }
        break;
    }

    case WM_CLOSE: {
        if (ctx && ctx->state == STATE_RUNNING && !ctx->isFinished) {
            InterlockedExchange(&ctx->isCancelled, 1);
            if (ctx->hProcess) TerminateProcess(ctx->hProcess, 1);
        }
        DestroyWindow(hwnd);
        return 0;
    }

    case WM_DESTROY: {
        PostQuitMessage(0);
        return 0;
    }
    }

    return DefWindowProcW(hwnd, msg, wParam, lParam);
}

// -----------------------------------------------------------------------------
// Entry Point (wWinMain)
// -----------------------------------------------------------------------------

int WINAPI wWinMain(HINSTANCE hInst, HINSTANCE, LPWSTR, int)
{
    INITCOMMONCONTROLSEX icc = { sizeof(icc), ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES };
    InitCommonControlsEx(&icc);

    GuiContext ctx;

    if (!FindBpdfExe(ctx.targetExe, ARRAYSIZE(ctx.targetExe))) {
        // bpdf.exe could not be found yet, can still open palette and browse
    }

    int argc = 0;
    LPWSTR* argv = CommandLineToArgvW(GetCommandLineW(), &argc);

    bool directJob = false;

    if (argv && argc >= 2) {
        for (int i = 1; i < argc; i++) {
            if (StrCmpIW(argv[i], L"--job") == 0 && i + 1 < argc) {
                directJob = ReadJobFile(argv[i + 1], &ctx);
                break;
            } else if (StrCmpIW(argv[i], L"--palette") == 0 && i + 1 < argc) {
                StrW manifestText;
                if (ReadTextFile(argv[i + 1], manifestText)) {
                    wchar_t* p = manifestText.data;
                    while (p && *p) {
                        wchar_t* nl = wcschr(p, L'\n');
                        if (nl) { *nl = L'\0'; nl++; }
                        wchar_t* t = Trim(p);
                        if (*t) ctx.files.add(t);
                        p = nl;
                    }
                }
                DeleteFileW(argv[i + 1]);
                break;
            } else if (argv[i][0] != L'-') {
                ctx.files.add(argv[i]);
            }
        }
    }
    if (argv) LocalFree(argv);

    if (directJob) {
        ctx.state = STATE_RUNNING;
        ctx.isPaletteSession = false;
    } else {
        ctx.state = STATE_PALETTE;
        ctx.isPaletteSession = true;
    }

    WNDCLASSEXW wc = { sizeof(wc) };
    wc.style = CS_HREDRAW | CS_VREDRAW;
    wc.lpfnWndProc = GuiWndProc;
    wc.hInstance = hInst;
    wc.hCursor = LoadCursorW(NULL, IDC_ARROW);
    wc.hbrBackground = (HBRUSH)(COLOR_BTNFACE + 1);
    wc.lpszClassName = BPDF_GUI_CLASS;
    RegisterClassExW(&wc);

    if (ctx.targetExe[0]) {
        if (ctx.iconIndex >= 0) ExtractIconExW(ctx.targetExe, ctx.iconIndex, &ctx.hIcon, NULL, 1);
        if (!ctx.hIcon) ExtractIconExW(ctx.targetExe, 0, &ctx.hIcon, NULL, 1);
    }

    int dpi = 96;
    HDC hdc = GetDC(NULL);
    if (hdc) {
        dpi = GetDeviceCaps(hdc, LOGPIXELSY);
        ReleaseDC(NULL, hdc);
    }
    if (dpi <= 0) dpi = 96;
    ctx.dpi = dpi;

    int winW = MulDiv(580, dpi, 96);
    int winH = MulDiv(400, dpi, 96);
    int posX = (GetSystemMetrics(SM_CXSCREEN) - winW) / 2;
    int posY = (GetSystemMetrics(SM_CYSCREEN) - winH) / 2;

    wchar_t winTitle[256];
    if (ctx.state == STATE_RUNNING) {
        wnsprintfW(winTitle, ARRAYSIZE(winTitle), L"bpdf — %s", ctx.currentTitle);
    } else {
        lstrcpynW(winTitle, L"bpdf — Панель действий", ARRAYSIZE(winTitle));
    }

    HWND hwnd = CreateWindowExW(
        WS_EX_APPWINDOW,
        BPDF_GUI_CLASS,
        winTitle,
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX,
        posX, posY, winW, winH,
        NULL, NULL, hInst, &ctx
    );

    if (!hwnd) return 1;

    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);

    MSG msg;
    while (GetMessageW(&msg, NULL, 0, 0)) {
        if (!IsDialogMessage(hwnd, &msg)) {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    return 0;
}
