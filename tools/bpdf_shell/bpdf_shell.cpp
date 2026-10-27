#define WIN32_LEAN_AND_MEAN
#define UNICODE
#define _UNICODE

#include <windows.h>
#include <shellapi.h>
#include <shlobj.h>
#include <shlwapi.h>
#include <commctrl.h>
#include <stdlib.h>
#include <wchar.h>

#pragma comment(lib, "ole32.lib")
#pragma comment(lib, "shell32.lib")
#pragma comment(lib, "user32.lib")
#pragma comment(lib, "gdi32.lib")
#pragma comment(lib, "advapi32.lib")
#pragma comment(lib, "shlwapi.lib")
#pragma comment(lib, "comctl32.lib")

// Lightweight new/delete without libstdc++
void* operator new(size_t size) { return calloc(1, size); }
void operator delete(void* p) noexcept { if (p) free(p); }
void operator delete(void* p, size_t) noexcept { if (p) free(p); }

// CLSID: {B9DF0001-B000-4000-8000-000000000001}
static const GUID CLSID_BpdfShell = 
{ 0xb9df0001, 0xb000, 0x4000, { 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01 } };

static const wchar_t* CLSID_STRING = L"{B9DF0001-B000-4000-8000-000000000001}";
static const wchar_t* BPDF_WND_CLASS = L"BpdfGuiWindow";

static HINSTANCE g_hInst = NULL;
static LONG g_refCount = 0;

#define WM_BPDF_APPEND   (WM_APP + 1)
#define WM_BPDF_FINISHED (WM_APP + 2)

enum FileKind {
    KIND_PDF,
    KIND_IMAGE,
    KIND_OFFICE,
    KIND_HTML,
    KIND_EBOOK,
    KIND_TEXT,
    KIND_DIRECTORY,
    KIND_OTHER
};

struct CustomMenuItem {
    wchar_t title[128];
    wchar_t cmd[512];
    int iconIndex;
    bool isSeparator;
    bool autoClose;
    bool showProgress;
};

// Built-in fallback INI configuration
static const wchar_t* DEFAULT_INI_TEXT =
    L"[menu.single.pdf]\n"
    L"item1 = title: Оптимизировать PDF (сжатие) | cmd: optimize | icon: 12\n"
    L"item2 = ---\n"
    L"item3 = title: Распознать текст внутри PDF (Windows OCR) | cmd: ocr --engine windows -i | icon: 17\n"
    L"item4 = title: Распознать текст в MD (Groq OCR) | cmd: ocr | icon: 2\n"
    L"item5 = ---\n"
    L"item6 = title: Разбить PDF постранично | cmd: split | icon: 4\n"
    L"item7 = title: Извлечь текст в TXT | cmd: text -o \"{dir}\\{name}.txt\" | icon: 9\n"
    L"item8 = title: Удалить метаданные | cmd: strip -i | icon: 3\n"
    L"item9 = title: Повернуть страницы на 90° | cmd: rotate -a 90 -i | icon: 7\n"
    L"item10 = title: Подогнать под размер A4 | cmd: resize --size A4 -i | icon: 8\n"
    L"item11 = title: Конвертировать в JPEG | cmd: convert | icon: 14\n"
    L"item12 = ---\n"
    L"item13 = title: Информация о PDF (свойства, страницы) | cmd: info | icon: 6 | gui: info\n"
    L"[menu.single.image]\n"
    L"item1 = title: Конвертировать в PDF | cmd: merge --optimize --auto-rotate -s A4 | icon: 1\n"
    L"item2 = ---\n"
    L"item3 = title: Распознать текст (Windows OCR) | cmd: ocr --engine windows -i | icon: 17\n"
    L"item4 = title: Распознать текст в MD (Groq OCR) | cmd: ocr | icon: 2\n"
    L"item5 = ---\n"
    L"item6 = title: Удалить метаданные | cmd: strip -i | icon: 3\n"
    L"item7 = title: Повернуть на 90° | cmd: rotate -a 90 -i | icon: 7\n"
    L"item8 = title: Конвертировать в JPEG | cmd: convert | icon: 14\n"
    L"[menu.single.directory]\n"
    L"item1 = title: Объединить файлы папки в PDF | cmd: merge --optimize --auto-rotate -s A4 | icon: 1\n"
    L"item2 = ---\n"
    L"item3 = title: Распознать текст во всех файлах (Windows OCR) | cmd: ocr --engine windows -i | icon: 17\n"
    L"item4 = title: Удалить метаданные во всех файлах | cmd: strip -i | icon: 3\n"
    L"item5 = title: Конвертировать файлы в JPEG | cmd: convert | icon: 14\n"
    L"[menu.single.default]\n"
    L"item1 = title: Конвертировать в PDF | cmd: merge --optimize --auto-rotate -s A4 | icon: 1\n"
    L"item2 = ---\n"
    L"item3 = title: Распознать текст (Windows OCR) | cmd: ocr --engine windows -i | icon: 17\n"
    L"item4 = title: Конвертировать в JPEG | cmd: convert | icon: 14\n"
    L"[menu.multi]\n"
    L"item1 = title: Объединить в PDF | cmd: merge --optimize --auto-rotate -s A4 | icon: 1\n"
    L"item2 = ---\n"
    L"item3 = title: Распознать текст внутри PDF (Windows OCR) | cmd: ocr --engine windows -i | icon: 17\n"
    L"item4 = title: Распознать текст в MD (Groq OCR) | cmd: ocr | icon: 2\n"
    L"item5 = ---\n"
    L"item6 = title: Удалить метаданные | cmd: strip -i | icon: 3\n"
    L"item7 = title: Повернуть страницы на 90° | cmd: rotate -a 90 -i | icon: 7\n"
    L"item8 = title: Подогнать страницы под A4 | cmd: resize --size A4 -i | icon: 8\n"
    L"item9 = ---\n"
    L"item10 = title: Конвертировать в JPEG | cmd: convert | icon: 14\n";

// -----------------------------------------------------------------------------
// Lightweight Containers (Zero C++ Standard Library overhead)
// -----------------------------------------------------------------------------

struct StrW {
    wchar_t* data;
    size_t len;
    size_t cap;

    StrW() : data(NULL), len(0), cap(0) {}
    ~StrW() { if (data) free(data); }

    void reserve(size_t n) {
        if (n + 1 > cap) {
            cap = (n + 1 < 32) ? 32 : (n + 1) * 2;
            data = (wchar_t*)realloc(data, cap * sizeof(wchar_t));
        }
    }

    void assign(const wchar_t* s, size_t n) {
        reserve(n);
        wmemcpy(data, s, n);
        data[n] = L'\0';
        len = n;
    }

    void assign(const wchar_t* s) {
        if (s) assign(s, wcslen(s));
        else clear();
    }

    void append(const wchar_t* s, size_t n) {
        if (!s || n == 0) return;
        reserve(len + n);
        wmemcpy(data + len, s, n);
        len += n;
        data[len] = L'\0';
    }

    void append(const wchar_t* s) {
        if (s) append(s, wcslen(s));
    }

    void append(wchar_t c) {
        reserve(len + 1);
        data[len++] = c;
        data[len] = L'\0';
    }

    const wchar_t* c_str() const { return data ? data : L""; }
    bool empty() const { return len == 0; }

    void clear() {
        len = 0;
        if (data) data[0] = L'\0';
    }
};

struct StrA {
    char* data;
    size_t len;
    size_t cap;

    StrA() : data(NULL), len(0), cap(0) {}
    ~StrA() { if (data) free(data); }

    void append(const char* s, size_t n) {
        if (!s || n == 0) return;
        if (len + n + 1 > cap) {
            cap = (len + n + 1 < 64) ? 64 : (len + n + 1) * 2;
            data = (char*)realloc(data, cap);
        }
        memcpy(data + len, s, n);
        len += n;
        data[len] = '\0';
    }

    void erase(size_t offset, size_t count) {
        if (offset >= len) return;
        if (offset + count >= len) {
            len = offset;
            data[len] = '\0';
        } else {
            memmove(data + offset, data + offset + count, len - (offset + count) + 1);
            len -= count;
        }
    }

    void clear() {
        len = 0;
        if (data) data[0] = '\0';
    }

    bool empty() const { return len == 0; }
};

struct WStringList {
    wchar_t** items;
    size_t count;
    size_t cap;

    WStringList() : items(NULL), count(0), cap(0) {}
    ~WStringList() { clear(); }

    void add(const wchar_t* str) {
        if (!str) return;
        if (count + 1 > cap) {
            cap = (cap == 0) ? 8 : cap * 2;
            items = (wchar_t**)realloc(items, cap * sizeof(wchar_t*));
        }
        items[count++] = _wcsdup(str);
    }

    void clear() {
        if (items) {
            for (size_t i = 0; i < count; i++) {
                if (items[i]) free(items[i]);
            }
            free(items);
            items = NULL;
        }
        count = 0;
        cap = 0;
    }

    const wchar_t* operator[](size_t i) const { return items[i]; }
    size_t size() const { return count; }
    bool empty() const { return count == 0; }
};

// -----------------------------------------------------------------------------
// Path & String Helpers (using shlwapi)
// -----------------------------------------------------------------------------

static void SafeStrCpy(wchar_t* dest, const wchar_t* src, size_t maxCount)
{
    if (!dest || maxCount == 0) return;
    if (!src) { dest[0] = L'\0'; return; }
    size_t i = 0;
    while (i + 1 < maxCount && src[i]) {
        dest[i] = src[i];
        i++;
    }
    dest[i] = L'\0';
}

static wchar_t* Trim(wchar_t* s)
{
    if (!s) return NULL;
    while (*s && (*s == L' ' || *s == L'\t' || *s == L'\r' || *s == L'\n' || *s == 0xFEFF)) s++;
    if (!*s) return s;
    wchar_t* end = s + wcslen(s) - 1;
    while (end >= s && (*end == L' ' || *end == L'\t' || *end == L'\r' || *end == L'\n')) {
        *end = L'\0';
        end--;
    }
    return s;
}

static bool MatchExt(const wchar_t* ext, const wchar_t* list)
{
    wchar_t needle[32];
    wnsprintfW(needle, ARRAYSIZE(needle), L".%s.", ext);
    return StrStrIW(list, needle) != NULL;
}

static FileKind ClassifyPath(const wchar_t* path)
{
    if (PathIsDirectoryW(path)) {
        return KIND_DIRECTORY;
    }

    const wchar_t* dot = PathFindExtensionW(path);
    if (!dot || !dot[0] || !dot[1]) {
        return KIND_OTHER;
    }
    const wchar_t* ext = dot + 1;

    if (StrCmpIW(ext, L"pdf") == 0) return KIND_PDF;
    if (MatchExt(ext, L".jpg.jpeg.png.webp.bmp.gif.tif.tiff.avif.heic.heif.cr2.nef.arw.dng.raf.")) return KIND_IMAGE;
    if (MatchExt(ext, L".docx.xlsx.pptx.rtf.odt.ods.odp.doc.xls.ppt.")) return KIND_OFFICE;
    if (MatchExt(ext, L".html.htm.xhtml.svg.")) return KIND_HTML;
    if (MatchExt(ext, L".epub.fb2.htmlz.")) return KIND_EBOOK;
    if (MatchExt(ext, L".txt.md.")) return KIND_TEXT;
    return KIND_OTHER;
}

static bool ResolveRelativeToDll(const wchar_t* const* relPaths, size_t count, wchar_t* outPath, size_t maxLen)
{
    wchar_t dllDir[MAX_PATH * 2] = {0};
    if (!GetModuleFileNameW(g_hInst, dllDir, ARRAYSIZE(dllDir))) return false;
    PathRemoveFileSpecW(dllDir);

    for (size_t i = 0; i < count; i++) {
        wchar_t combined[MAX_PATH * 2] = {0};
        wchar_t full[MAX_PATH * 2] = {0};
        wnsprintfW(combined, ARRAYSIZE(combined), L"%s\\%s", dllDir, relPaths[i]);
        if (GetFullPathNameW(combined, ARRAYSIZE(full), full, NULL) > 0 && PathFileExistsW(full)) {
            SafeStrCpy(outPath, full, maxLen);
            return true;
        }
    }
    return false;
}

static bool FindBpdfExe(wchar_t* outPath, size_t maxLen)
{
    // 1. Относительно самой DLL
    const wchar_t* relCandidates[] = {
        L"bpdf.exe",
        L"..\\bpdf.exe",
        L"..\\..\\dist\\bpdf.exe",
        L"..\\..\\target\\release\\bpdf.exe",
        L"..\\..\\bpdf.exe"
    };
    if (ResolveRelativeToDll(relCandidates, ARRAYSIZE(relCandidates), outPath, maxLen)) {
        return true;
    }

    // 2. В реестре HKCU\Software\bpdf -> ExePath
    wchar_t regPath[MAX_PATH * 2] = {0};
    DWORD bytes = sizeof(regPath);
    if (SHGetValueW(HKEY_CURRENT_USER, L"Software\\bpdf", L"ExePath", NULL, regPath, &bytes) == ERROR_SUCCESS) {
        if (PathFileExistsW(regPath)) {
            SafeStrCpy(outPath, regPath, maxLen);
            return true;
        }
    }

    // 3. В системном PATH
    wchar_t searchPath[MAX_PATH * 2] = {0};
    if (SearchPathW(NULL, L"bpdf.exe", NULL, ARRAYSIZE(searchPath), searchPath, NULL) > 0) {
        if (PathFileExistsW(searchPath)) {
            SafeStrCpy(outPath, searchPath, maxLen);
            return true;
        }
    }

    return false;
}

static bool FindBpdfIni(wchar_t* outPath, size_t maxLen)
{
    wchar_t dllDir[MAX_PATH * 2] = {0};
    if (!GetModuleFileNameW(g_hInst, dllDir, ARRAYSIZE(dllDir))) return false;
    PathRemoveFileSpecW(dllDir);

    wnsprintfW(outPath, (int)maxLen, L"%s\\bpdf_shell.ini", dllDir);
    return PathFileExistsW(outPath);
}

// -----------------------------------------------------------------------------
// INI Configuration Parser (UTF-8 + Built-in Fallback)
// -----------------------------------------------------------------------------

static bool ReadTextFile(const wchar_t* path, StrW& outText)
{
    HANDLE hFile = CreateFileW(
        path,
        GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        NULL,
        OPEN_EXISTING,
        0,
        NULL
    );
    if (hFile == INVALID_HANDLE_VALUE) return false;

    DWORD size = GetFileSize(hFile, NULL);
    if (size == 0 || size == INVALID_FILE_SIZE) {
        CloseHandle(hFile);
        return false;
    }

    char* buf = (char*)malloc(size + 1);
    if (!buf) {
        CloseHandle(hFile);
        return false;
    }

    DWORD bytesRead = 0;
    bool ok = ReadFile(hFile, buf, size, &bytesRead, NULL) && bytesRead > 0;
    CloseHandle(hFile);
    if (!ok) {
        free(buf);
        return false;
    }
    buf[bytesRead] = '\0';

    const char* pText = buf;
    if (bytesRead >= 3 && (unsigned char)buf[0] == 0xEF && (unsigned char)buf[1] == 0xBB && (unsigned char)buf[2] == 0xBF) {
        pText += 3;
        bytesRead -= 3;
    }

    int wLen = MultiByteToWideChar(CP_UTF8, 0, pText, (int)bytesRead, NULL, 0);
    if (wLen > 0) {
        outText.reserve(wLen);
        MultiByteToWideChar(CP_UTF8, 0, pText, (int)bytesRead, outText.data, wLen);
        outText.data[wLen] = L'\0';
        outText.len = wLen;
    }

    free(buf);
    return (wLen > 0);
}

static void ParseItemFields(wchar_t* str, CustomMenuItem& item)
{
    item.iconIndex = 0;
    item.autoClose = true;
    item.showProgress = true;
    item.isSeparator = false;
    item.title[0] = L'\0';
    item.cmd[0] = L'\0';

    wchar_t* p = str;
    while (*p) {
        wchar_t* pipe = wcschr(p, L'|');
        if (pipe) *pipe = L'\0';

        wchar_t* part = Trim(p);
        wchar_t* colon = wcschr(part, L':');
        if (colon) {
            *colon = L'\0';
            wchar_t* key = Trim(part);
            wchar_t* val = Trim(colon + 1);

            if (StrCmpIW(key, L"title") == 0) {
                SafeStrCpy(item.title, val, ARRAYSIZE(item.title));
            } else if (StrCmpIW(key, L"cmd") == 0) {
                SafeStrCpy(item.cmd, val, ARRAYSIZE(item.cmd));
            } else if (StrCmpIW(key, L"icon") == 0) {
                item.iconIndex = StrToIntW(val);
            } else if (StrCmpIW(key, L"gui") == 0) {
                if (StrCmpIW(val, L"info") == 0) {
                    item.autoClose = false;
                    item.showProgress = false;
                } else if (StrCmpIW(val, L"keep") == 0) {
                    item.autoClose = false;
                    item.showProgress = true;
                }
            }
        }
        if (!pipe) break;
        p = pipe + 1;
    }
}

static bool ParseIniText(wchar_t* text, const wchar_t* targetSection, CustomMenuItem* outItems, size_t maxItems, size_t* outCount)
{
    size_t count = 0;
    bool inTargetSection = false;

    wchar_t* line = text;
    while (line && *line) {
        wchar_t* nextLine = wcschr(line, L'\n');
        if (nextLine) {
            *nextLine = L'\0';
            nextLine++;
        }

        wchar_t* trimmed = Trim(line);
        if (*trimmed == L';' || *trimmed == L'#' || *trimmed == L'\0') {
            line = nextLine;
            continue;
        }

        if (*trimmed == L'[') {
            wchar_t* endBracket = wcschr(trimmed, L']');
            if (endBracket) {
                *endBracket = L'\0';
                inTargetSection = (StrCmpIW(Trim(trimmed + 1), targetSection) == 0);
            } else {
                inTargetSection = false;
            }
            line = nextLine;
            continue;
        }

        if (inTargetSection && count < maxItems) {
            wchar_t* eq = wcschr(trimmed, L'=');
            if (eq) {
                wchar_t* val = Trim(eq + 1);
                if (wcscmp(val, L"---") == 0) {
                    outItems[count] = { L"", L"", -1, true, false, false };
                    count++;
                } else {
                    ParseItemFields(val, outItems[count]);
                    if (outItems[count].title[0] != L'\0') {
                        count++;
                    }
                }
            }
        }

        line = nextLine;
    }

    *outCount = count;
    return (count > 0);
}

static void LoadMenuConfig(const wchar_t* targetSection, CustomMenuItem* outItems, size_t maxItems, size_t* outCount)
{
    wchar_t iniPath[MAX_PATH * 2] = {0};
    StrW text;
    if (FindBpdfIni(iniPath, ARRAYSIZE(iniPath)) && ReadTextFile(iniPath, text) && !text.empty()) {
        if (ParseIniText(text.data, targetSection, outItems, maxItems, outCount)) {
            return;
        }
    }

    // Fallback to built-in configuration
    text.assign(DEFAULT_INI_TEXT);
    ParseIniText(text.data, targetSection, outItems, maxItems, outCount);
}

// -----------------------------------------------------------------------------
// Icon and Bitmap Rendering
// -----------------------------------------------------------------------------

static HBITMAP CreateMenuBitmapFromIcon(HICON hIcon)
{
    if (!hIcon) return NULL;

    int cx = GetSystemMetrics(SM_CXSMICON);
    int cy = GetSystemMetrics(SM_CYSMICON);
    if (cx <= 0) cx = 16;
    if (cy <= 0) cy = 16;

    BITMAPINFO bi = { 0 };
    bi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bi.bmiHeader.biWidth = cx;
    bi.bmiHeader.biHeight = cy;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB;

    HDC hdcScreen = GetDC(NULL);
    HDC hdcMem = CreateCompatibleDC(hdcScreen);
    void* pBits = NULL;
    HBITMAP hBmp = CreateDIBSection(hdcMem, &bi, DIB_RGB_COLORS, &pBits, NULL, 0);

    if (hBmp && pBits) {
        HGDIOBJ hOld = SelectObject(hdcMem, hBmp);
        memset(pBits, 0, cx * cy * 4);
        DrawIconEx(hdcMem, 0, 0, hIcon, cx, cy, 0, NULL, DI_NORMAL);

        DWORD* pPixels = (DWORD*)pBits;
        bool hasAlpha = false;
        for (int i = 0; i < cx * cy; i++) {
            if ((pPixels[i] & 0xFF000000) != 0) {
                hasAlpha = true;
                break;
            }
        }
        if (!hasAlpha) {
            for (int i = 0; i < cx * cy; i++) {
                if ((pPixels[i] & 0x00FFFFFF) != 0) {
                    pPixels[i] |= 0xFF000000;
                }
            }
        }

        SelectObject(hdcMem, hOld);
    }

    DeleteDC(hdcMem);
    ReleaseDC(NULL, hdcScreen);
    return hBmp;
}

static HBITMAP LoadMenuIcon(const wchar_t* exePath, int iconIndex)
{
    if (!exePath || !*exePath || iconIndex < 0) return NULL;

    HICON hSmall = NULL;
    if (ExtractIconExW(exePath, iconIndex, NULL, &hSmall, 1) > 0 && hSmall) {
        HBITMAP hBmp = CreateMenuBitmapFromIcon(hSmall);
        DestroyIcon(hSmall);
        return hBmp;
    }
    return NULL;
}

static bool CreateManifestFile(const WStringList& files, wchar_t* outPath, size_t maxLen)
{
    wchar_t tempDir[MAX_PATH] = {0};
    GetTempPathW(ARRAYSIZE(tempDir), tempDir);

    wchar_t tempFile[MAX_PATH] = {0};
    if (!GetTempFileNameW(tempDir, L"bpdf", 0, tempFile)) return false;

    HANDLE hFile = CreateFileW(tempFile, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (hFile == INVALID_HANDLE_VALUE) return false;

    const unsigned char bom[3] = { 0xEF, 0xBB, 0xBF };
    DWORD written = 0;
    WriteFile(hFile, bom, sizeof(bom), &written, NULL);

    for (size_t i = 0; i < files.size(); i++) {
        int utf8Len = WideCharToMultiByte(CP_UTF8, 0, files[i], -1, NULL, 0, NULL, NULL);
        if (utf8Len > 1) {
            char* utf8Buf = (char*)malloc(utf8Len);
            if (utf8Buf) {
                WideCharToMultiByte(CP_UTF8, 0, files[i], -1, utf8Buf, utf8Len, NULL, NULL);
                WriteFile(hFile, utf8Buf, utf8Len - 1, &written, NULL);
                WriteFile(hFile, "\r\n", 2, &written, NULL);
                free(utf8Buf);
            }
        }
    }

    CloseHandle(hFile);
    SafeStrCpy(outPath, tempFile, maxLen);
    return true;
}

// -----------------------------------------------------------------------------
// GUI Execution Window (No console popup!)
// -----------------------------------------------------------------------------

struct GuiProcessContext {
    HWND hwnd;
    HWND hTitleStatic;
    HWND hStatusStatic;
    HWND hProgress;
    HWND hEdit;
    HWND hBtnClose;
    HWND hBtnFolder;

    HFONT hFontUi;
    HFONT hFontTitle;
    HFONT hFontEdit;
    HBRUSH hBrushEdit;
    HICON hIcon;

    wchar_t title[128];
    wchar_t exePath[MAX_PATH * 2];
    wchar_t* cmdLine;
    wchar_t workDir[MAX_PATH * 2];
    wchar_t tempManifest[MAX_PATH * 2];

    int iconIndex;
    bool autoCloseOnSuccess;
    bool showProgress;

    HANDLE hProcess;
    HANDLE hWorkerThread;
    volatile bool isFinished;
    volatile bool userCancelled;
    DWORD exitCode;

    ~GuiProcessContext() {
        if (hWorkerThread) {
            WaitForSingleObject(hWorkerThread, INFINITE);
            CloseHandle(hWorkerThread);
        }
        if (hProcess) CloseHandle(hProcess);
        if (hFontUi) DeleteObject(hFontUi);
        if (hFontTitle) DeleteObject(hFontTitle);
        if (hFontEdit) DeleteObject(hFontEdit);
        if (hBrushEdit) DeleteObject(hBrushEdit);
        if (hIcon) DestroyIcon(hIcon);
        if (cmdLine) free(cmdLine);
        if (tempManifest[0] != L'\0') DeleteFileW(tempManifest);
    }
};

static size_t GetValidUtf8Length(const char* str, size_t len)
{
    if (!str || len == 0) return 0;
    for (size_t i = 0; i < 4 && i < len; i++) {
        unsigned char c = (unsigned char)str[len - 1 - i];
        if ((c & 0x80) == 0) return len;
        if ((c & 0xC0) == 0xC0) {
            size_t needed = 0;
            if ((c & 0xE0) == 0xC0) needed = 2;
            else if ((c & 0xF0) == 0xE0) needed = 3;
            else if ((c & 0xF8) == 0xF0) needed = 4;
            else return len;

            size_t available = i + 1;
            return (available < needed) ? (len - available) : len;
        }
    }
    return len;
}

static void FlushUtf8ToWindow(HWND hwnd, const char* data, size_t len)
{
    if (len == 0) return;
    int wLen = MultiByteToWideChar(CP_UTF8, 0, data, (int)len, NULL, 0);
    if (wLen > 0) {
        wchar_t* wBuf = (wchar_t*)malloc((wLen + 1) * sizeof(wchar_t));
        if (wBuf) {
            MultiByteToWideChar(CP_UTF8, 0, data, (int)len, wBuf, wLen);
            wBuf[wLen] = L'\0';
            if (!PostMessageW(hwnd, WM_BPDF_APPEND, 0, (LPARAM)wBuf)) {
                free(wBuf);
            }
        }
    }
}

static DWORD WINAPI ProcessWorkerThread(LPVOID param)
{
    GuiProcessContext* ctx = (GuiProcessContext*)param;

    HANDLE hReadPipe = NULL, hWritePipe = NULL;
    SECURITY_ATTRIBUTES sa = { sizeof(sa), NULL, TRUE };
    if (!CreatePipe(&hReadPipe, &hWritePipe, &sa, 0)) {
        ctx->isFinished = true;
        PostMessageW(ctx->hwnd, WM_BPDF_FINISHED, 0, (LPARAM)1);
        return 1;
    }
    SetHandleInformation(hReadPipe, HANDLE_FLAG_INHERIT, 0);

    HANDLE hNullIn = CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0, NULL);

    STARTUPINFOW si = { sizeof(si) };
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdOutput = hWritePipe;
    si.hStdError = hWritePipe;
    si.hStdInput = (hNullIn != INVALID_HANDLE_VALUE) ? hNullIn : NULL;

    PROCESS_INFORMATION pi = { 0 };
    const wchar_t* pDir = (ctx->workDir[0] != L'\0') ? ctx->workDir : NULL;

    BOOL ok = CreateProcessW(
        ctx->exePath,
        ctx->cmdLine,
        NULL,
        NULL,
        TRUE,
        CREATE_NO_WINDOW,
        NULL,
        pDir,
        &si,
        &pi
    );

    CloseHandle(hWritePipe);
    if (hNullIn != INVALID_HANDLE_VALUE) CloseHandle(hNullIn);

    if (!ok) {
        CloseHandle(hReadPipe);
        ctx->isFinished = true;
        PostMessageW(ctx->hwnd, WM_BPDF_FINISHED, 0, (LPARAM)1);
        return 1;
    }

    CloseHandle(pi.hThread);
    ctx->hProcess = pi.hProcess;

    char rawBuf[2048];
    DWORD bytesRead = 0;
    StrA utf8Accum;

    while (ReadFile(hReadPipe, rawBuf, sizeof(rawBuf), &bytesRead, NULL) && bytesRead > 0) {
        utf8Accum.append(rawBuf, bytesRead);
        size_t validLen = GetValidUtf8Length(utf8Accum.data, utf8Accum.len);
        if (validLen > 0) {
            FlushUtf8ToWindow(ctx->hwnd, utf8Accum.data, validLen);
            utf8Accum.erase(0, validLen);
        }
    }

    if (!utf8Accum.empty()) {
        FlushUtf8ToWindow(ctx->hwnd, utf8Accum.data, utf8Accum.len);
        utf8Accum.clear();
    }

    CloseHandle(hReadPipe);

    WaitForSingleObject(ctx->hProcess, INFINITE);
    DWORD exitCode = 0;
    GetExitCodeProcess(ctx->hProcess, &exitCode);
    ctx->exitCode = exitCode;
    ctx->isFinished = true;

    PostMessageW(ctx->hwnd, WM_BPDF_FINISHED, 0, (LPARAM)exitCode);
    return 0;
}

static void CancelRunningProcess(GuiProcessContext* ctx)
{
    if (ctx && !ctx->isFinished && ctx->hProcess) {
        ctx->userCancelled = true;
        TerminateProcess(ctx->hProcess, 1);
    }
}

static LRESULT CALLBACK GuiWndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam)
{
    GuiProcessContext* ctx = (GuiProcessContext*)GetWindowLongPtrW(hwnd, GWLP_USERDATA);

    switch (msg) {
    case WM_CREATE: {
        CREATESTRUCTW* cs = (CREATESTRUCTW*)lParam;
        ctx = (GuiProcessContext*)cs->lpCreateParams;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (LONG_PTR)ctx);
        ctx->hwnd = hwnd;

        if (ctx->hIcon) {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG, (LPARAM)ctx->hIcon);
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL, (LPARAM)ctx->hIcon);
        }

        ctx->hFontUi = CreateFontW(-12, 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH | FF_SWISS, L"Segoe UI");
        ctx->hFontTitle = CreateFontW(-16, 0, 0, 0, FW_BOLD, FALSE, FALSE, FALSE, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, DEFAULT_PITCH | FF_SWISS, L"Segoe UI");
        ctx->hFontEdit = CreateFontW(-13, 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, FIXED_PITCH | FF_MODERN, L"Consolas");

        ctx->hTitleStatic = CreateWindowExW(0, L"STATIC", ctx->title, WS_CHILD | WS_VISIBLE | SS_LEFT, 15, 12, 595, 22, hwnd, NULL, g_hInst, NULL);
        
        const wchar_t* initStatus = ctx->showProgress ? L"Выполняется..." : L"Свойства документа:";
        ctx->hStatusStatic = CreateWindowExW(0, L"STATIC", initStatus, WS_CHILD | WS_VISIBLE | SS_LEFT, 15, 36, 595, 18, hwnd, NULL, g_hInst, NULL);

        int editY = 58;
        int editH = 331;
        if (ctx->showProgress) {
            ctx->hProgress = CreateWindowExW(0, PROGRESS_CLASSW, NULL, WS_CHILD | WS_VISIBLE | PBS_MARQUEE, 15, 58, 595, 14, hwnd, NULL, g_hInst, NULL);
            SendMessageW(ctx->hProgress, PBM_SETMARQUEE, TRUE, 30);
            editY = 80;
            editH = 309;
        }

        ctx->hEdit = CreateWindowExW(
            WS_EX_CLIENTEDGE, L"EDIT", L"",
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_HSCROLL | ES_MULTILINE | ES_AUTOVSCROLL | ES_AUTOHSCROLL | ES_READONLY,
            15, editY, 595, editH, hwnd, (HMENU)101, g_hInst, NULL
        );
        SendMessageW(ctx->hEdit, EM_SETLIMITTEXT, 0, 0);

        ctx->hBtnFolder = CreateWindowExW(0, L"BUTTON", L"Открыть папку", WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON, 15, 400, 130, 28, hwnd, (HMENU)102, g_hInst, NULL);
        const wchar_t* closeText = ctx->autoCloseOnSuccess ? L"Отмена" : L"Закрыть";
        ctx->hBtnClose = CreateWindowExW(0, L"BUTTON", closeText, WS_CHILD | WS_VISIBLE | BS_DEFPUSHBUTTON, 500, 400, 110, 28, hwnd, (HMENU)103, g_hInst, NULL);

        SendMessageW(ctx->hTitleStatic, WM_SETFONT, (WPARAM)ctx->hFontTitle, TRUE);
        SendMessageW(ctx->hStatusStatic, WM_SETFONT, (WPARAM)ctx->hFontUi, TRUE);
        SendMessageW(ctx->hEdit, WM_SETFONT, (WPARAM)ctx->hFontEdit, TRUE);
        SendMessageW(ctx->hBtnFolder, WM_SETFONT, (WPARAM)ctx->hFontUi, TRUE);
        SendMessageW(ctx->hBtnClose, WM_SETFONT, (WPARAM)ctx->hFontUi, TRUE);

        EnableWindow(ctx->hBtnFolder, (ctx->workDir[0] != L'\0'));
        if (!ctx->autoCloseOnSuccess) {
            SetFocus(ctx->hBtnClose);
        }

        ctx->hWorkerThread = CreateThread(NULL, 0, ProcessWorkerThread, ctx, 0, NULL);
        return 0;
    }

    case WM_GETMINMAXINFO: {
        MINMAXINFO* mmi = (MINMAXINFO*)lParam;
        mmi->ptMinTrackSize.x = 450;
        mmi->ptMinTrackSize.y = 300;
        return 0;
    }

    case WM_SIZE: {
        int w = LOWORD(lParam);
        int h = HIWORD(lParam);
        if (ctx && ctx->hEdit) {
            SetWindowPos(ctx->hTitleStatic, NULL, 15, 12, w - 30, 22, SWP_NOZORDER);
            SetWindowPos(ctx->hStatusStatic, NULL, 15, 36, w - 30, 18, SWP_NOZORDER);
            if (ctx->showProgress && ctx->hProgress) {
                SetWindowPos(ctx->hProgress, NULL, 15, 58, w - 30, 14, SWP_NOZORDER);
                SetWindowPos(ctx->hEdit, NULL, 15, 80, w - 30, h - 124, SWP_NOZORDER);
            } else {
                SetWindowPos(ctx->hEdit, NULL, 15, 58, w - 30, h - 102, SWP_NOZORDER);
            }
            SetWindowPos(ctx->hBtnFolder, NULL, 15, h - 38, 130, 28, SWP_NOZORDER);
            SetWindowPos(ctx->hBtnClose, NULL, w - 125, h - 38, 110, 28, SWP_NOZORDER);
        }
        return 0;
    }

    case WM_CTLCOLOREDIT:
    case WM_CTLCOLORSTATIC: {
        if (ctx && (HWND)lParam == ctx->hEdit) {
            HDC hdc = (HDC)wParam;
            SetTextColor(hdc, RGB(225, 230, 235));
            SetBkColor(hdc, RGB(26, 27, 30));
            if (!ctx->hBrushEdit) {
                ctx->hBrushEdit = CreateSolidBrush(RGB(26, 27, 30));
            }
            return (LRESULT)ctx->hBrushEdit;
        }
        break;
    }

    case WM_BPDF_APPEND: {
        wchar_t* text = (wchar_t*)lParam;
        if (text) {
            if (ctx && ctx->hEdit) {
                const wchar_t* toRemove = L"Press Enter to exit...";
                wchar_t* pos = wcsstr(text, toRemove);
                if (pos) {
                    size_t remLen = wcslen(toRemove);
                    wmemmove(pos, pos + remLen, wcslen(pos + remLen) + 1);
                }

                StrW crlfText;
                for (size_t i = 0; text[i] != L'\0'; i++) {
                    if (text[i] == L'\n' && (i == 0 || text[i - 1] != L'\r')) {
                        crlfText.append(L'\r');
                    }
                    crlfText.append(text[i]);
                }
                int len = GetWindowTextLengthW(ctx->hEdit);
                SendMessageW(ctx->hEdit, EM_SETSEL, len, len);
                SendMessageW(ctx->hEdit, EM_REPLACESEL, FALSE, (LPARAM)crlfText.c_str());
                SendMessageW(ctx->hEdit, EM_SCROLLCARET, 0, 0);
            }
            free(text);
        }
        return 0;
    }

    case WM_BPDF_FINISHED: {
        DWORD exitCode = (DWORD)lParam;
        if (ctx) {
            if (ctx->hProgress) {
                SendMessageW(ctx->hProgress, PBM_SETMARQUEE, FALSE, 0);
            }
            if (exitCode == 0) {
                if (ctx->hProgress) {
                    SendMessageW(ctx->hProgress, PBM_SETRANGE32, 0, 100);
                    SendMessageW(ctx->hProgress, PBM_SETPOS, 100, 0);
                }
                SetWindowTextW(ctx->hStatusStatic, L"Завершено успешно");

                if (ctx->autoCloseOnSuccess && !ctx->userCancelled) {
                    SetTimer(hwnd, 1001, 350, NULL);
                    return 0;
                }
            } else if (ctx->userCancelled) {
                SetWindowTextW(ctx->hStatusStatic, L"Операция отменена пользователем");
            } else {
                wchar_t errText[64];
                wnsprintfW(errText, ARRAYSIZE(errText), L"Завершено с ошибкой (код %u)", exitCode);
                SetWindowTextW(ctx->hStatusStatic, errText);
            }
            SetWindowTextW(ctx->hBtnClose, L"Закрыть");
            SetFocus(ctx->hBtnClose);
        }
        return 0;
    }

    case WM_TIMER: {
        if (wParam == 1001) {
            KillTimer(hwnd, 1001);
            DestroyWindow(hwnd);
            return 0;
        }
        break;
    }

    case WM_COMMAND: {
        WORD id = LOWORD(wParam);
        if (id == 102 && ctx && ctx->workDir[0] != L'\0') {
            ShellExecuteW(NULL, L"open", ctx->workDir, NULL, NULL, SW_SHOWNORMAL);
        } else if (id == 103 && ctx) {
            if (!ctx->isFinished) {
                CancelRunningProcess(ctx);
            } else {
                DestroyWindow(hwnd);
            }
        }
        return 0;
    }

    case WM_KEYDOWN: {
        if (wParam == VK_ESCAPE) {
            CancelRunningProcess(ctx);
            DestroyWindow(hwnd);
            return 0;
        } else if (wParam == VK_RETURN) {
            if (ctx && (!ctx->autoCloseOnSuccess || ctx->isFinished)) {
                DestroyWindow(hwnd);
                return 0;
            }
        }
        break;
    }

    case WM_CLOSE: {
        CancelRunningProcess(ctx);
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

static void EnsureWindowClassRegistered()
{
    static bool s_registered = false;
    if (s_registered) return;

    INITCOMMONCONTROLSEX icc = { sizeof(icc), ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES };
    InitCommonControlsEx(&icc);

    WNDCLASSEXW wc = { sizeof(wc) };
    wc.style = CS_HREDRAW | CS_VREDRAW;
    wc.lpfnWndProc = GuiWndProc;
    wc.hInstance = g_hInst;
    wc.hCursor = LoadCursorW(NULL, IDC_ARROW);
    wc.hbrBackground = (HBRUSH)(COLOR_BTNFACE + 1);
    wc.lpszClassName = BPDF_WND_CLASS;
    RegisterClassExW(&wc);
    s_registered = true;
}

static DWORD WINAPI GuiThreadProc(LPVOID p)
{
    InterlockedIncrement(&g_refCount);
    GuiProcessContext* ctx = (GuiProcessContext*)p;

    EnsureWindowClassRegistered();

    if (ctx->iconIndex >= 0) {
        ExtractIconExW(ctx->exePath, ctx->iconIndex, &ctx->hIcon, NULL, 1);
    }
    if (!ctx->hIcon) {
        ExtractIconExW(ctx->exePath, 0, &ctx->hIcon, NULL, 1);
    }

    int winW = 640;
    int winH = 480;
    int posX = (GetSystemMetrics(SM_CXSCREEN) - winW) / 2;
    int posY = (GetSystemMetrics(SM_CYSCREEN) - winH) / 2;

    wchar_t winTitle[256];
    wnsprintfW(winTitle, ARRAYSIZE(winTitle), L"bpdf — %s", ctx->title);

    HWND hwnd = CreateWindowExW(
        WS_EX_APPWINDOW,
        BPDF_WND_CLASS,
        winTitle,
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX,
        posX, posY, winW, winH,
        NULL, NULL, g_hInst, ctx
    );

    if (hwnd) {
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);

        MSG msg;
        while (GetMessageW(&msg, NULL, 0, 0)) {
            if (msg.message == WM_KEYDOWN && (msg.wParam == VK_ESCAPE || (msg.wParam == VK_RETURN && (!ctx->autoCloseOnSuccess || ctx->isFinished)))) {
                SendMessageW(hwnd, WM_KEYDOWN, msg.wParam, msg.lParam);
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    delete ctx;
    InterlockedDecrement(&g_refCount);
    return 0;
}

static void ReplaceString(StrW& target, const wchar_t* placeholder, const wchar_t* replacement)
{
    if (!replacement || !target.data) return;
    size_t phLen = wcslen(placeholder);
    size_t repLen = wcslen(replacement);
    size_t searchFrom = 0;

    while (searchFrom < target.len) {
        wchar_t* pos = wcsstr(target.data + searchFrom, placeholder);
        if (!pos) break;
        size_t offset = (size_t)(pos - target.data);

        StrW next;
        next.append(target.data, offset);
        next.append(replacement, repLen);
        next.append(pos + phLen);
        target.assign(next.c_str());

        searchFrom = offset + repLen;
    }
}

static void ExecuteCustomItem(const CustomMenuItem& item, const WStringList& files)
{
    if (files.empty()) return;

    wchar_t exe[MAX_PATH * 2] = {0};
    if (!FindBpdfExe(exe, ARRAYSIZE(exe))) {
        MessageBoxW(
            NULL,
            L"Исполняемый файл bpdf.exe не найден!\n\n"
            L"Убедитесь, что bpdf.exe находится в одной папке с shell или запустите shell\\register.bat из папки с программой.",
            L"bpdf",
            MB_ICONERROR
        );
        return;
    }

    wchar_t fullPath[MAX_PATH * 2] = {0};
    wchar_t dir[MAX_PATH * 2] = {0};
    wchar_t name[MAX_PATH * 2] = {0};
    wchar_t ext[64] = {0};

    SafeStrCpy(fullPath, files[0], ARRAYSIZE(fullPath));
    SafeStrCpy(dir, files[0], ARRAYSIZE(dir));
    PathRemoveFileSpecW(dir);

    SafeStrCpy(name, PathFindFileNameW(files[0]), ARRAYSIZE(name));
    const wchar_t* dot = PathFindExtensionW(name);
    if (dot && dot[0] == L'.') {
        SafeStrCpy(ext, dot + 1, ARRAYSIZE(ext));
        PathRemoveExtensionW(name);
    }

    StrW expanded;
    expanded.assign(item.cmd);

    bool hasFilePlaceholder = (wcsstr(item.cmd, L"{file}") != NULL);
    ReplaceString(expanded, L"{file}", fullPath);
    ReplaceString(expanded, L"{dir}", dir);
    ReplaceString(expanded, L"{name}", name);
    ReplaceString(expanded, L"{ext}", ext);

    wchar_t manifestPath[MAX_PATH * 2] = {0};
    StrW fullCmdLine;
    fullCmdLine.append(L"\"");
    fullCmdLine.append(exe);
    fullCmdLine.append(L"\" ");
    fullCmdLine.append(expanded.c_str());

    if (!hasFilePlaceholder) {
        fullCmdLine.append(L" \"");
        if (files.size() > 1) {
            if (!CreateManifestFile(files, manifestPath, ARRAYSIZE(manifestPath))) {
                MessageBoxW(NULL, L"Не удалось создать файл списка аргументов", L"bpdf", MB_ICONERROR);
                return;
            }
            fullCmdLine.append(L"@");
            fullCmdLine.append(manifestPath);
        } else {
            fullCmdLine.append(files[0]);
        }
        fullCmdLine.append(L"\"");
    }

    GuiProcessContext* ctx = new GuiProcessContext();
    SafeStrCpy(ctx->title, item.title[0] ? item.title : L"Операция", ARRAYSIZE(ctx->title));
    SafeStrCpy(ctx->exePath, exe, ARRAYSIZE(ctx->exePath));
    ctx->cmdLine = _wcsdup(fullCmdLine.c_str());
    SafeStrCpy(ctx->workDir, dir, ARRAYSIZE(ctx->workDir));
    SafeStrCpy(ctx->tempManifest, manifestPath, ARRAYSIZE(ctx->tempManifest));
    ctx->iconIndex = item.iconIndex;
    ctx->autoCloseOnSuccess = item.autoClose;
    ctx->showProgress = item.showProgress;

    HANDLE hThread = CreateThread(NULL, 0, GuiThreadProc, ctx, 0, NULL);
    if (hThread) {
        CloseHandle(hThread);
    } else {
        GuiThreadProc(ctx);
    }
}

// -----------------------------------------------------------------------------
// COM Shell Extension Context Menu Implementation
// -----------------------------------------------------------------------------

class CBpdfContextMenu final : public IShellExtInit, public IContextMenu
{
private:
    LONG m_refCount;
    WStringList m_files;
    CustomMenuItem m_actionMap[48];
    size_t m_actionCount;
    HBITMAP m_bitmaps[48];
    size_t m_bitmapCount;

public:
    CBpdfContextMenu() : m_refCount(1), m_actionCount(0), m_bitmapCount(0)
    {
        InterlockedIncrement(&g_refCount);
    }

    ~CBpdfContextMenu()
    {
        for (size_t i = 0; i < m_bitmapCount; i++) {
            if (m_bitmaps[i]) DeleteObject(m_bitmaps[i]);
        }
        InterlockedDecrement(&g_refCount);
    }

    // IUnknown
    STDMETHODIMP QueryInterface(REFIID riid, void** ppv) override
    {
        if (!ppv) return E_POINTER;
        *ppv = NULL;

        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_IShellExtInit)) {
            *ppv = static_cast<IShellExtInit*>(this);
        } else if (IsEqualIID(riid, IID_IContextMenu)) {
            *ppv = static_cast<IContextMenu*>(this);
        } else {
            return E_NOINTERFACE;
        }

        AddRef();
        return S_OK;
    }

    STDMETHODIMP_(ULONG) AddRef() override { return InterlockedIncrement(&m_refCount); }

    STDMETHODIMP_(ULONG) Release() override
    {
        LONG count = InterlockedDecrement(&m_refCount);
        if (count == 0) delete this;
        return count;
    }

    // IShellExtInit
    STDMETHODIMP Initialize(PCIDLIST_ABSOLUTE pidlFolder, IDataObject* pdtobj, HKEY /*hkeyProgID*/) override
    {
        m_files.clear();
        if (pdtobj) {
            FORMATETC fmt = { CF_HDROP, NULL, DVASPECT_CONTENT, -1, TYMED_HGLOBAL };
            STGMEDIUM stg;
            if (SUCCEEDED(pdtobj->GetData(&fmt, &stg))) {
                HDROP hDrop = (HDROP)stg.hGlobal;
                UINT count = DragQueryFileW(hDrop, 0xFFFFFFFF, NULL, 0);
                for (UINT i = 0; i < count; i++) {
                    wchar_t path[MAX_PATH * 4] = {0};
                    if (DragQueryFileW(hDrop, i, path, ARRAYSIZE(path)) > 0) {
                        m_files.add(path);
                    }
                }
                ReleaseStgMedium(&stg);
            }
        }
        if (m_files.empty() && pidlFolder) {
            wchar_t folderPath[MAX_PATH * 4] = {0};
            if (SHGetPathFromIDListW(pidlFolder, folderPath)) {
                m_files.add(folderPath);
            }
        }
        return S_OK;
    }

    // IContextMenu
    STDMETHODIMP QueryContextMenu(HMENU hmenu, UINT indexMenu, UINT idCmdFirst, UINT /*idCmdLast*/, UINT uFlags) override
    {
        if ((uFlags & CMF_DEFAULTONLY) != 0 || m_files.empty()) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        bool anySupported = false;
        FileKind firstKind = KIND_OTHER;
        for (size_t i = 0; i < m_files.size(); i++) {
            FileKind k = ClassifyPath(m_files[i]);
            if (i == 0) firstKind = k;
            if (k != KIND_OTHER) anySupported = true;
        }
        if (!anySupported) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        wchar_t exePath[MAX_PATH * 2] = {0};
        FindBpdfExe(exePath, ARRAYSIZE(exePath));

        const wchar_t* targetSection = L"menu.single.default";
        if (m_files.size() > 1) {
            targetSection = L"menu.multi";
        } else {
            switch (firstKind) {
            case KIND_PDF:       targetSection = L"menu.single.pdf"; break;
            case KIND_IMAGE:     targetSection = L"menu.single.image"; break;
            case KIND_DIRECTORY: targetSection = L"menu.single.directory"; break;
            default:             targetSection = L"menu.single.default"; break;
            }
        }

        CustomMenuItem items[48];
        size_t itemCount = 0;
        LoadMenuConfig(targetSection, items, ARRAYSIZE(items), &itemCount);
        if (itemCount == 0) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        HMENU hSubMenu = CreatePopupMenu();
        if (!hSubMenu) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        m_actionCount = 0;
        UINT itemIndex = 0;
        for (size_t i = 0; i < itemCount; i++) {
            MENUITEMINFOW mii = { sizeof(mii) };
            if (items[i].isSeparator) {
                mii.fMask = MIIM_FTYPE;
                mii.fType = MFT_SEPARATOR;
                InsertMenuItemW(hSubMenu, itemIndex++, TRUE, &mii);
            } else if (m_actionCount < ARRAYSIZE(m_actionMap)) {
                mii.fMask = MIIM_STRING | MIIM_ID;
                mii.wID = idCmdFirst + (UINT)m_actionCount;
                mii.dwTypeData = items[i].title;

                HBITMAP hBmp = LoadMenuIcon(exePath, items[i].iconIndex);
                if (hBmp && m_bitmapCount < ARRAYSIZE(m_bitmaps)) {
                    mii.fMask |= MIIM_BITMAP;
                    mii.hbmpItem = hBmp;
                    m_bitmaps[m_bitmapCount++] = hBmp;
                }

                InsertMenuItemW(hSubMenu, itemIndex++, TRUE, &mii);
                m_actionMap[m_actionCount++] = items[i];
            }
        }

        MENUITEMINFOW parentMii = { sizeof(parentMii) };
        parentMii.fMask = MIIM_STRING | MIIM_SUBMENU | MIIM_ID;
        parentMii.wID = idCmdFirst + (UINT)m_actionCount;
        parentMii.hSubMenu = hSubMenu;
        parentMii.dwTypeData = const_cast<LPWSTR>(L"bpdf");

        HBITMAP hMainBmp = LoadMenuIcon(exePath, 0);
        if (hMainBmp && m_bitmapCount < ARRAYSIZE(m_bitmaps)) {
            parentMii.fMask |= MIIM_BITMAP;
            parentMii.hbmpItem = hMainBmp;
            m_bitmaps[m_bitmapCount++] = hMainBmp;
        }

        if (!InsertMenuItemW(hmenu, indexMenu, TRUE, &parentMii)) {
            DestroyMenu(hSubMenu);
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        return MAKE_HRESULT(SEVERITY_SUCCESS, 0, (ULONG)m_actionCount + 1);
    }

    STDMETHODIMP InvokeCommand(LPCMINVOKECOMMANDINFO pici) override
    {
        if (!pici) return E_POINTER;
        if (HIWORD(pici->lpVerb) != 0) return E_FAIL;

        UINT verbOffset = LOWORD(pici->lpVerb);
        if (verbOffset >= m_actionCount) return E_FAIL;

        ExecuteCustomItem(m_actionMap[verbOffset], m_files);
        return S_OK;
    }

    STDMETHODIMP GetCommandString(UINT_PTR, UINT, UINT*, LPSTR, UINT) override
    {
        return S_OK;
    }
};

// -----------------------------------------------------------------------------
// Class Factory
// -----------------------------------------------------------------------------

class CBpdfClassFactory final : public IClassFactory
{
private:
    LONG m_refCount;

public:
    CBpdfClassFactory() : m_refCount(1) {}

    STDMETHODIMP QueryInterface(REFIID riid, void** ppv) override
    {
        if (!ppv) return E_POINTER;
        *ppv = NULL;

        if (IsEqualIID(riid, IID_IUnknown) || IsEqualIID(riid, IID_IClassFactory)) {
            *ppv = static_cast<IClassFactory*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef() override { return InterlockedIncrement(&m_refCount); }

    STDMETHODIMP_(ULONG) Release() override
    {
        LONG count = InterlockedDecrement(&m_refCount);
        if (count == 0) delete this;
        return count;
    }

    STDMETHODIMP CreateInstance(IUnknown* pUnkOuter, REFIID riid, void** ppv) override
    {
        if (pUnkOuter) return CLASS_E_NOAGGREGATION;
        CBpdfContextMenu* pMenu = new CBpdfContextMenu();
        HRESULT hr = pMenu->QueryInterface(riid, ppv);
        pMenu->Release();
        return hr;
    }

    STDMETHODIMP LockServer(BOOL fLock) override
    {
        if (fLock) InterlockedIncrement(&g_refCount);
        else InterlockedDecrement(&g_refCount);
        return S_OK;
    }
};

// -----------------------------------------------------------------------------
// DLL Exports & Self-Registration (HKCU - no admin rights required!)
// -----------------------------------------------------------------------------

BOOL WINAPI DllMain(HINSTANCE hInst, DWORD reason, LPVOID)
{
    if (reason == DLL_PROCESS_ATTACH) {
        g_hInst = hInst;
        DisableThreadLibraryCalls(hInst);
    }
    return TRUE;
}

STDAPI DllGetClassObject(REFCLSID rclsid, REFIID riid, void** ppv)
{
    if (!ppv) return E_POINTER;
    *ppv = NULL;

    if (!IsEqualCLSID(rclsid, CLSID_BpdfShell)) {
        return CLASS_E_CLASSNOTAVAILABLE;
    }

    CBpdfClassFactory* pFactory = new CBpdfClassFactory();
    HRESULT hr = pFactory->QueryInterface(riid, ppv);
    pFactory->Release();
    return hr;
}

STDAPI DllCanUnloadNow(void)
{
    return (g_refCount == 0) ? S_OK : S_FALSE;
}

static bool SetRegString(HKEY root, const wchar_t* subkey, const wchar_t* name, const wchar_t* value)
{
    DWORD bytes = (DWORD)((wcslen(value) + 1) * sizeof(wchar_t));
    return SHSetValueW(root, subkey, name, REG_SZ, value, bytes) == ERROR_SUCCESS;
}

STDAPI DllRegisterServer(void)
{
    wchar_t dllPath[MAX_PATH * 2] = {0};
    if (GetModuleFileNameW(g_hInst, dllPath, ARRAYSIZE(dllPath)) == 0) {
        return E_FAIL;
    }

    wchar_t clsidKey[128];
    wnsprintfW(clsidKey, ARRAYSIZE(clsidKey), L"Software\\Classes\\CLSID\\%s", CLSID_STRING);
    wchar_t inprocKey[160];
    wnsprintfW(inprocKey, ARRAYSIZE(inprocKey), L"%s\\InprocServer32", clsidKey);

    SetRegString(HKEY_CURRENT_USER, clsidKey, NULL, L"bpdf Shell Extension");
    SetRegString(HKEY_CURRENT_USER, inprocKey, NULL, dllPath);
    SetRegString(HKEY_CURRENT_USER, inprocKey, L"ThreadingModel", L"Apartment");

    wchar_t exePath[MAX_PATH * 2] = {0};
    if (FindBpdfExe(exePath, ARRAYSIZE(exePath))) {
        SetRegString(HKEY_CURRENT_USER, L"Software\\bpdf", L"ExePath", exePath);
    }

    SetRegString(HKEY_CURRENT_USER, L"Software\\Classes\\*\\shellex\\ContextMenuHandlers\\bpdf", NULL, CLSID_STRING);
    SetRegString(HKEY_CURRENT_USER, L"Software\\Classes\\Folder\\shellex\\ContextMenuHandlers\\bpdf", NULL, CLSID_STRING);
    SetRegString(HKEY_CURRENT_USER, L"Software\\Classes\\Directory\\Background\\shellex\\ContextMenuHandlers\\bpdf", NULL, CLSID_STRING);

    return S_OK;
}

STDAPI DllUnregisterServer(void)
{
    RegDeleteTreeW(HKEY_CURRENT_USER, L"Software\\Classes\\*\\shellex\\ContextMenuHandlers\\bpdf");
    RegDeleteTreeW(HKEY_CURRENT_USER, L"Software\\Classes\\Folder\\shellex\\ContextMenuHandlers\\bpdf");
    RegDeleteTreeW(HKEY_CURRENT_USER, L"Software\\Classes\\Directory\\Background\\shellex\\ContextMenuHandlers\\bpdf");

    wchar_t clsidKey[128];
    wnsprintfW(clsidKey, ARRAYSIZE(clsidKey), L"Software\\Classes\\CLSID\\%s", CLSID_STRING);
    RegDeleteTreeW(HKEY_CURRENT_USER, clsidKey);
    RegDeleteTreeW(HKEY_CURRENT_USER, L"Software\\bpdf");

    return S_OK;
}
