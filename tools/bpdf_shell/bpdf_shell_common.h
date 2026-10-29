#pragma once

#define WIN32_LEAN_AND_MEAN
#ifndef UNICODE
#define UNICODE
#endif
#ifndef _UNICODE
#define _UNICODE
#endif

#include <windows.h>
#include <shellapi.h>
#include <shlwapi.h>
#include <stdlib.h>
#include <wchar.h>

// -----------------------------------------------------------------------------
// Common Enums & Data Types
// -----------------------------------------------------------------------------

enum FileKind {
    KIND_UNSUPPORTED = 0,
    KIND_PDF,
    KIND_IMAGE,
    KIND_DIRECTORY,
    KIND_OTHER_SUPPORTED
};

enum GuiMode {
    GUI_AUTO = 0,
    GUI_INFO = 1,
    GUI_KEEP = 2
};

#define MAX_SHELL_ACTIONS 32
#define MAX_INI_ITEMS     64

struct MenuItemDef {
    wchar_t title[128];
    wchar_t cmd[512];
    int iconIndex; // -1 for separator
    GuiMode guiMode;
};

// -----------------------------------------------------------------------------
// Lightweight Dynamic Wide String
// -----------------------------------------------------------------------------

struct StrW {
    wchar_t* data;
    size_t len;
    size_t cap;

    StrW() : data(NULL), len(0), cap(0) {}
    ~StrW() { if (data) free(data); }
    StrW(const StrW&) = delete;
    StrW& operator=(const StrW&) = delete;

    void clear() {
        len = 0;
        if (data) data[0] = L'\0';
    }

    // Clears the whole buffer: for strings that held a password
    void wipe() {
        if (data) SecureZeroMemory(data, cap * sizeof(wchar_t));
        len = 0;
    }

    bool reserve(size_t n) {
        if (n + 1 > cap) {
            size_t newCap = (n + 1 < 32) ? 32 : (n + 1) * 2;
            wchar_t* newData = (wchar_t*)realloc(data, newCap * sizeof(wchar_t));
            if (!newData) return false;
            data = newData;
            cap = newCap;
        }
        return true;
    }

    void assign(const wchar_t* s, size_t n) {
        if (!reserve(n)) return;
        if (s && n > 0) wmemcpy(data, s, n);
        data[n] = L'\0';
        len = n;
    }

    void assign(const wchar_t* s) {
        if (s) assign(s, wcslen(s));
        else clear();
    }

    void append(const wchar_t* s, size_t n) {
        if (!s || n == 0) return;
        if (!reserve(len + n)) return;
        wmemcpy(data + len, s, n);
        len += n;
        data[len] = L'\0';
    }

    void append(const wchar_t* s) {
        if (s) append(s, wcslen(s));
    }

    void append(wchar_t c) {
        if (!reserve(len + 1)) return;
        data[len++] = c;
        data[len] = L'\0';
    }

    void truncate(size_t n) {
        if (n < len) {
            len = n;
            data[len] = L'\0';
        }
    }

    const wchar_t* c_str() const { return data ? data : L""; }
    size_t length() const { return len; }
    bool empty() const { return len == 0; }
};

// -----------------------------------------------------------------------------
// Lightweight Vector of Dynamic Strings
// -----------------------------------------------------------------------------

struct WStringList {
    wchar_t** items;
    size_t count;
    size_t cap;

    WStringList() : items(NULL), count(0), cap(0) {}
    ~WStringList() { clear(); }
    WStringList(const WStringList&) = delete;
    WStringList& operator=(const WStringList&) = delete;

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

    void add(const wchar_t* str) {
        if (!str) return;
        wchar_t* copy = _wcsdup(str);
        if (!copy) return;
        if (count + 1 > cap) {
            size_t newCap = (cap == 0) ? 8 : cap * 2;
            wchar_t** newItems = (wchar_t**)realloc(items, newCap * sizeof(wchar_t*));
            if (!newItems) {
                free(copy);
                return;
            }
            items = newItems;
            cap = newCap;
        }
        items[count++] = copy;
    }

    size_t size() const { return count; }
    bool empty() const { return count == 0; }
    const wchar_t* operator[](size_t idx) const { return items[idx]; }
};

// -----------------------------------------------------------------------------
// File Type Classification
// -----------------------------------------------------------------------------

static inline bool MatchExt(const wchar_t* ext, const wchar_t* list)
{
    wchar_t needle[32];
    wnsprintfW(needle, ARRAYSIZE(needle), L".%s.", ext);
    return StrStrIW(list, needle) != NULL;
}

// Known extensions are decided without disk access (network drives in Explorer);
// only the rest is checked for being a folder, e.g. "Scans.2024"
static inline FileKind ClassifyPath(const wchar_t* path)
{
    if (!path || !path[0]) return KIND_UNSUPPORTED;

    size_t len = wcslen(path);
    if (path[len - 1] == L'\\' || path[len - 1] == L'/') {
        return KIND_DIRECTORY;
    }

    const wchar_t* dot = PathFindExtensionW(path);
    if (dot[0] && dot[1]) {
        const wchar_t* ext = dot + 1;
        if (StrCmpIW(ext, L"pdf") == 0) return KIND_PDF;
        if (MatchExt(ext, L".jpg.jpeg.png.webp.bmp.gif.tif.tiff.avif.heic.heif.cr2.nef.arw.dng.raf.")) return KIND_IMAGE;
        if (MatchExt(ext, L".docx.xlsx.pptx.ppsx.rtf.odt.ods.odp.doc.xls.ppt.pps.html.htm.xhtml.svg.epub.fb2.htmlz.cbz.txt.md.")) return KIND_OTHER_SUPPORTED;
    }

    DWORD attrs = GetFileAttributesW(path);
    if (attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_DIRECTORY)) {
        return KIND_DIRECTORY;
    }
    return KIND_UNSUPPORTED;
}

static inline void GetSectionForFiles(const WStringList& files, wchar_t* outSection, size_t maxLen)
{
    const wchar_t* section = L"menu.multi";
    if (files.size() <= 1) {
        FileKind kind = files.empty() ? KIND_UNSUPPORTED : ClassifyPath(files[0]);
        if (kind == KIND_PDF) section = L"menu.single.pdf";
        else if (kind == KIND_IMAGE) section = L"menu.single.image";
        else if (kind == KIND_DIRECTORY) section = L"menu.single.directory";
        else section = L"menu.single.default";
    }
    lstrcpynW(outSection, section, (int)maxLen);
}

// -----------------------------------------------------------------------------
// Exe, GUI & INI Path Discovery
// -----------------------------------------------------------------------------

static inline bool SearchInPathEnv(const wchar_t* fileName, wchar_t* outPath, size_t maxLen)
{
    DWORD envLen = GetEnvironmentVariableW(L"PATH", NULL, 0);
    if (envLen == 0) return false;

    wchar_t* envPath = (wchar_t*)malloc(envLen * sizeof(wchar_t));
    if (!envPath) return false;

    // Only the PATH directories: SearchPathW with NULL would also look in the
    // current directory of the host process
    DWORD n = 0;
    if (GetEnvironmentVariableW(L"PATH", envPath, envLen) > 0) {
        n = SearchPathW(envPath, fileName, NULL, (DWORD)maxLen, outPath, NULL);
    }
    free(envPath);
    return n > 0 && n < maxLen;
}

// Looks for fileName in dir, dir\shell and the parent of dir
static inline bool FindNear(const wchar_t* dir, const wchar_t* fileName, wchar_t* outPath, size_t maxLen)
{
    static const wchar_t* const kSubdirs[] = { L"", L"shell\\", L"..\\" };
    for (size_t i = 0; i < ARRAYSIZE(kSubdirs); i++) {
        wchar_t cand[MAX_PATH * 2];
        wnsprintfW(cand, ARRAYSIZE(cand), L"%s\\%s%s", dir, kSubdirs[i], fileName);
        DWORD n = GetFullPathNameW(cand, (DWORD)maxLen, outPath, NULL);
        if (n > 0 && n < maxLen && GetFileAttributesW(outPath) != INVALID_FILE_ATTRIBUTES) {
            return true;
        }
    }
    return false;
}

static inline bool FindNearModule(const wchar_t* fileName, wchar_t* outPath, size_t maxLen, HMODULE hMod)
{
    wchar_t baseDir[MAX_PATH * 2] = {0};
    if (!GetModuleFileNameW(hMod, baseDir, ARRAYSIZE(baseDir))) return false;
    PathRemoveFileSpecW(baseDir);
    return FindNear(baseDir, fileName, outPath, maxLen);
}

static inline bool FindBpdfExe(wchar_t* outPath, size_t maxLen, HMODULE hMod = NULL)
{
    return FindNearModule(L"bpdf.exe", outPath, maxLen, hMod) ||
           SearchInPathEnv(L"bpdf.exe", outPath, maxLen);
}

static inline bool FindBpdfGui(wchar_t* outPath, size_t maxLen, HMODULE hMod = NULL)
{
    if (FindNearModule(L"bpdf-gui.exe", outPath, maxLen, hMod)) return true;

    wchar_t exeDir[MAX_PATH * 2] = {0};
    if (FindBpdfExe(exeDir, ARRAYSIZE(exeDir), hMod)) {
        PathRemoveFileSpecW(exeDir);
        if (FindNear(exeDir, L"bpdf-gui.exe", outPath, maxLen)) return true;
    }
    return SearchInPathEnv(L"bpdf-gui.exe", outPath, maxLen);
}

static inline bool FindBpdfIni(wchar_t* outPath, size_t maxLen, HMODULE hMod = NULL)
{
    return FindNearModule(L"bpdf_shell.ini", outPath, maxLen, hMod);
}

// -----------------------------------------------------------------------------
// UTF-8 INI Reader & Parser
// -----------------------------------------------------------------------------

static inline bool ReadTextFile(const wchar_t* path, StrW& outText)
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

    char* buf = (char*)malloc(size + 2);
    if (!buf) { CloseHandle(hFile); return false; }

    DWORD bytesRead = 0;
    bool ok = ReadFile(hFile, buf, size, &bytesRead, NULL) && bytesRead > 0;
    CloseHandle(hFile);
    if (!ok) { free(buf); return false; }
    buf[bytesRead] = '\0';
    buf[bytesRead + 1] = '\0';

    if (bytesRead >= 2 && (unsigned char)buf[0] == 0xFF && (unsigned char)buf[1] == 0xFE) {
        outText.assign((const wchar_t*)(buf + 2), (bytesRead - 2) / sizeof(wchar_t));
        free(buf);
        return true;
    }

    int offset = 0;
    if (bytesRead >= 3 && (unsigned char)buf[0] == 0xEF && (unsigned char)buf[1] == 0xBB && (unsigned char)buf[2] == 0xBF) {
        offset = 3;
    }

    UINT codePage = CP_UTF8;
    DWORD flags = MB_ERR_INVALID_CHARS;
    int wLen = MultiByteToWideChar(codePage, flags, buf + offset, (int)bytesRead - offset, NULL, 0);
    if (wLen <= 0) {
        codePage = CP_ACP;
        flags = 0;
        wLen = MultiByteToWideChar(codePage, flags, buf + offset, (int)bytesRead - offset, NULL, 0);
    }
    if (wLen <= 0 || !outText.reserve(wLen)) { free(buf); return false; }
    MultiByteToWideChar(codePage, flags, buf + offset, (int)bytesRead - offset, outText.data, wLen);
    outText.data[wLen] = L'\0';
    outText.len = wLen;
    free(buf);
    return true;
}

static inline wchar_t* Trim(wchar_t* s)
{
    if (!s) return s;
    while (*s == L' ' || *s == L'\t' || *s == L'\r' || *s == L'\n') s++;
    size_t len = wcslen(s);
    while (len > 0 && (s[len - 1] == L' ' || s[len - 1] == L'\t' || s[len - 1] == L'\r' || s[len - 1] == L'\n')) {
        s[--len] = L'\0';
    }
    return s;
}

// Fields of "title: ... | cmd: ... | icon: N | gui: mode"; modifies line
static inline void ParseItemFields(wchar_t* line, MenuItemDef& item)
{
    item.title[0] = L'\0';
    item.cmd[0] = L'\0';
    item.iconIndex = 0;
    item.guiMode = GUI_AUTO;

    wchar_t* token = line;
    while (token && *token) {
        wchar_t* nextBar = wcschr(token, L'|');
        if (nextBar) *nextBar++ = L'\0';

        wchar_t* colon = wcschr(token, L':');
        if (colon) {
            *colon = L'\0';
            wchar_t* key = Trim(token);
            wchar_t* val = Trim(colon + 1);

            if (StrCmpIW(key, L"title") == 0) lstrcpynW(item.title, val, ARRAYSIZE(item.title));
            else if (StrCmpIW(key, L"cmd") == 0) lstrcpynW(item.cmd, val, ARRAYSIZE(item.cmd));
            else if (StrCmpIW(key, L"icon") == 0) {
                int idx = StrToIntW(val);
                item.iconIndex = (idx < 0) ? 0 : idx;
            }
            else if (StrCmpIW(key, L"gui") == 0) {
                if (StrCmpIW(val, L"info") == 0) item.guiMode = GUI_INFO;
                else if (StrCmpIW(val, L"keep") == 0) item.guiMode = GUI_KEEP;
            }
        }
        token = nextBar;
    }
}

static inline size_t ParseIniSection(const wchar_t* iniContent, const wchar_t* targetSection, MenuItemDef* outItems, size_t maxItems)
{
    if (!iniContent || !targetSection || maxItems == 0) return 0;

    wchar_t* buf = _wcsdup(iniContent);
    if (!buf) return 0;

    size_t count = 0;
    bool inTargetSection = false;
    wchar_t* line = buf;

    while (line && *line && count < maxItems) {
        wchar_t* nextLine = wcschr(line, L'\n');
        if (nextLine) *nextLine++ = L'\0';

        wchar_t* trimmed = Trim(line);
        line = nextLine;
        if (*trimmed == L';' || *trimmed == L'#' || *trimmed == L'\0') continue;

        if (*trimmed == L'[') {
            wchar_t* endBracket = wcschr(trimmed, L']');
            if (endBracket) *endBracket = L'\0';
            inTargetSection = endBracket && StrCmpIW(Trim(trimmed + 1), targetSection) == 0;
            continue;
        }

        wchar_t* eq = inTargetSection ? wcschr(trimmed, L'=') : NULL;
        if (!eq) continue;

        wchar_t* val = Trim(eq + 1);
        MenuItemDef& item = outItems[count];
        if (wcscmp(val, L"---") == 0) {
            item.title[0] = L'\0';
            item.cmd[0] = L'\0';
            item.iconIndex = -1;
            item.guiMode = GUI_AUTO;
            count++;
        } else {
            ParseItemFields(val, item);
            if (item.title[0]) count++;
        }
    }

    free(buf);
    return count;
}

static inline size_t LoadMenuItems(const wchar_t* section, MenuItemDef* outItems, size_t maxItems, HMODULE hMod = NULL)
{
    wchar_t iniPath[MAX_PATH * 2] = {0};
    StrW iniContent;
    if (!FindBpdfIni(iniPath, ARRAYSIZE(iniPath), hMod) || !ReadTextFile(iniPath, iniContent)) return 0;
    return ParseIniSection(iniContent.c_str(), section, outItems, maxItems);
}

// -----------------------------------------------------------------------------
// Command Line Building
// -----------------------------------------------------------------------------

// Text for inside "..." by the CommandLineToArgvW rules: '"' is escaped and
// the backslashes in front of it doubled, trailing ones too when quoteFollows
static inline void AppendEscaped(StrW& out, const wchar_t* s, bool quoteFollows)
{
    size_t bsCount = 0;
    for (; *s; ++s) {
        if (*s == L'\\') {
            bsCount++;
        } else {
            if (*s == L'"') {
                for (size_t i = 0; i <= bsCount; i++) out.append(L'\\');
            }
            bsCount = 0;
        }
        out.append(*s);
    }
    if (quoteFollows) {
        for (size_t i = 0; i < bsCount; i++) out.append(L'\\');
    }
}

static inline void AppendQuotedArg(StrW& out, const wchar_t* s)
{
    out.append(L'"');
    AppendEscaped(out, s, true);
    out.append(L'"');
}

// Quotes only when the argument would not survive as is
static inline void AppendArg(StrW& out, const wchar_t* s)
{
    if (!s[0] || wcspbrk(s, L" \t\"")) AppendQuotedArg(out, s);
    else out.append(s);
}

// UTF-8 list of files, one per line, in %TEMP%
static inline bool CreateManifestFile(const WStringList& files, wchar_t* outPath, size_t maxLen)
{
    wchar_t tempDir[MAX_PATH * 2] = {0};
    GetTempPathW(ARRAYSIZE(tempDir), tempDir);

    wchar_t tempFile[MAX_PATH * 2] = {0};
    if (!GetTempFileNameW(tempDir, L"bpdf", 0, tempFile)) return false;

    HANDLE hFile = CreateFileW(tempFile, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (hFile == INVALID_HANDLE_VALUE) {
        DeleteFileW(tempFile);
        return false;
    }

    bool writeOk = true;
    DWORD written = 0;
    for (size_t i = 0; i < files.size() && writeOk; i++) {
        int utf8Len = WideCharToMultiByte(CP_UTF8, 0, files[i], -1, NULL, 0, NULL, NULL);
        if (utf8Len <= 1) continue;
        char* utf8Buf = (char*)malloc(utf8Len + 1);
        if (!utf8Buf) {
            writeOk = false;
            break;
        }
        WideCharToMultiByte(CP_UTF8, 0, files[i], -1, utf8Buf, utf8Len, NULL, NULL);
        utf8Buf[utf8Len - 1] = '\n';
        writeOk = WriteFile(hFile, utf8Buf, utf8Len, &written, NULL) && written == (DWORD)utf8Len;
        free(utf8Buf);
    }

    CloseHandle(hFile);
    if (!writeOk) {
        DeleteFileW(tempFile);
        return false;
    }
    lstrcpynW(outPath, tempFile, (int)maxLen);
    return true;
}
