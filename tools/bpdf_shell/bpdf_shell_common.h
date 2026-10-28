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
// Lightweight Memory Management (Zero CRT Dependency)
// -----------------------------------------------------------------------------

#ifndef BPDF_CUSTOM_NEW_DELETE_DEFINED
#define BPDF_CUSTOM_NEW_DELETE_DEFINED
inline void* operator new(size_t size) noexcept { return calloc(1, size); }
inline void* operator new[](size_t size) noexcept { return calloc(1, size); }
inline void operator delete(void* p) noexcept { if (p) free(p); }
inline void operator delete(void* p, size_t) noexcept { if (p) free(p); }
inline void operator delete[](void* p) noexcept { if (p) free(p); }
inline void operator delete[](void* p, size_t) noexcept { if (p) free(p); }
#endif

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
    GUI_KEEP = 2,
    GUI_NONE = 3
};

#define MAX_SHELL_ACTIONS 32

struct MenuItemDef {
    wchar_t title[128];
    wchar_t cmd[512];
    int iconIndex; // -1 for separator
    GuiMode guiMode;

    MenuItemDef() : iconIndex(0), guiMode(GUI_AUTO) {
        title[0] = L'\0';
        cmd[0] = L'\0';
    }
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
        if (count + 1 > cap) {
            cap = (cap == 0) ? 8 : cap * 2;
            items = (wchar_t**)realloc(items, cap * sizeof(wchar_t*));
        }
        items[count++] = _wcsdup(str);
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

static inline FileKind ClassifyPath(const wchar_t* path)
{
    if (!path || !path[0]) return KIND_UNSUPPORTED;

    // Fast check: ends with slash -> directory
    size_t len = wcslen(path);
    if (path[len - 1] == L'\\' || path[len - 1] == L'/') {
        return KIND_DIRECTORY;
    }

    // Fast check by extension first (avoiding network/disk I/O in Explorer)
    const wchar_t* dot = PathFindExtensionW(path);
    if (dot && dot[0] && dot[1]) {
        const wchar_t* ext = dot + 1;
        if (StrCmpIW(ext, L"pdf") == 0) return KIND_PDF;
        if (MatchExt(ext, L".jpg.jpeg.png.webp.bmp.gif.tif.tiff.avif.heic.heif.cr2.nef.arw.dng.raf.")) return KIND_IMAGE;
        if (MatchExt(ext, L".docx.xlsx.pptx.rtf.odt.ods.odp.doc.xls.ppt.")) return KIND_OTHER_SUPPORTED;
        if (MatchExt(ext, L".html.htm.xhtml.svg.")) return KIND_OTHER_SUPPORTED;
        if (MatchExt(ext, L".epub.fb2.htmlz.djvu.xps.oxps.cbz.")) return KIND_OTHER_SUPPORTED;
        if (MatchExt(ext, L".txt.md.")) return KIND_OTHER_SUPPORTED;
    }

    // Only if extension is absent or unknown, check if path is a directory (e.g. C:\Folder or Report.2024)
    DWORD attrs = GetFileAttributesW(path);
    if (attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_DIRECTORY)) {
        return KIND_DIRECTORY;
    }

    return KIND_UNSUPPORTED;
}

static inline void GetSectionForFiles(const WStringList& files, wchar_t* outSection, size_t maxLen)
{
    if (files.empty()) {
        lstrcpynW(outSection, L"menu.single.default", (int)maxLen);
        return;
    }

    if (files.size() == 1) {
        FileKind kind = ClassifyPath(files[0]);
        if (kind == KIND_PDF) lstrcpynW(outSection, L"menu.single.pdf", (int)maxLen);
        else if (kind == KIND_IMAGE) lstrcpynW(outSection, L"menu.single.image", (int)maxLen);
        else if (kind == KIND_DIRECTORY) lstrcpynW(outSection, L"menu.single.directory", (int)maxLen);
        else lstrcpynW(outSection, L"menu.single.default", (int)maxLen);
        return;
    }

    lstrcpynW(outSection, L"menu.multi", (int)maxLen);
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

    bool found = false;
    if (GetEnvironmentVariableW(L"PATH", envPath, envLen) > 0) {
        wchar_t searchPath[MAX_PATH * 2] = {0};
        if (SearchPathW(envPath, fileName, NULL, ARRAYSIZE(searchPath), searchPath, NULL) > 0) {
            if (PathFileExistsW(searchPath)) {
                lstrcpynW(outPath, searchPath, (int)maxLen);
                found = true;
            }
        }
    }
    free(envPath);
    return found;
}

static inline bool FindBpdfExe(wchar_t* outPath, size_t maxLen, HMODULE hMod = NULL)
{
    wchar_t baseDir[MAX_PATH * 2] = {0};
    if (GetModuleFileNameW(hMod, baseDir, ARRAYSIZE(baseDir))) {
        PathRemoveFileSpecW(baseDir);

        // 1. Рядом с модулем (DLL или GUI)
        wchar_t cand[MAX_PATH * 2];
        wnsprintfW(cand, ARRAYSIZE(cand), L"%s\\bpdf.exe", baseDir);
        if (PathFileExistsW(cand)) {
            lstrcpynW(outPath, cand, (int)maxLen);
            return true;
        }

        // 2. В родительской папке (например dist\bpdf.exe при dist\shell\bpdf_shell.dll)
        wnsprintfW(cand, ARRAYSIZE(cand), L"%s\\..\\bpdf.exe", baseDir);
        wchar_t full[MAX_PATH * 2];
        if (GetFullPathNameW(cand, ARRAYSIZE(full), full, NULL) > 0 && PathFileExistsW(full)) {
            lstrcpynW(outPath, full, (int)maxLen);
            return true;
        }
    }

    // 3. В системном PATH (безопасный поиск только по переменной PATH, исключая текущую папку процесса)
    return SearchInPathEnv(L"bpdf.exe", outPath, maxLen);
}

static inline bool FindBpdfGui(wchar_t* outPath, size_t maxLen, HMODULE hMod = NULL)
{
    wchar_t baseDir[MAX_PATH * 2] = {0};
    if (GetModuleFileNameW(hMod, baseDir, ARRAYSIZE(baseDir))) {
        PathRemoveFileSpecW(baseDir);

        // 1. Рядом с модулем
        wchar_t cand[MAX_PATH * 2];
        wnsprintfW(cand, ARRAYSIZE(cand), L"%s\\bpdf-gui.exe", baseDir);
        if (PathFileExistsW(cand)) {
            lstrcpynW(outPath, cand, (int)maxLen);
            return true;
        }

        // 2. В родительской папке
        wnsprintfW(cand, ARRAYSIZE(cand), L"%s\\..\\bpdf-gui.exe", baseDir);
        wchar_t full[MAX_PATH * 2];
        if (GetFullPathNameW(cand, ARRAYSIZE(full), full, NULL) > 0 && PathFileExistsW(full)) {
            lstrcpynW(outPath, full, (int)maxLen);
            return true;
        }
    }

    // 3. Рядом с bpdf.exe
    wchar_t bpdfExe[MAX_PATH * 2] = {0};
    if (FindBpdfExe(bpdfExe, ARRAYSIZE(bpdfExe), hMod)) {
        PathRemoveFileSpecW(bpdfExe);
        wchar_t cand[MAX_PATH * 2];
        wnsprintfW(cand, ARRAYSIZE(cand), L"%s\\bpdf-gui.exe", bpdfExe);
        if (PathFileExistsW(cand)) {
            lstrcpynW(outPath, cand, (int)maxLen);
            return true;
        }
        wnsprintfW(cand, ARRAYSIZE(cand), L"%s\\shell\\bpdf-gui.exe", bpdfExe);
        if (PathFileExistsW(cand)) {
            lstrcpynW(outPath, cand, (int)maxLen);
            return true;
        }
    }

    // 4. В системном PATH (безопасный поиск только по переменной PATH)
    return SearchInPathEnv(L"bpdf-gui.exe", outPath, maxLen);
}

static inline bool FindBpdfIni(wchar_t* outPath, size_t maxLen, HMODULE hMod = NULL)
{
    wchar_t baseDir[MAX_PATH * 2] = {0};
    if (!GetModuleFileNameW(hMod, baseDir, ARRAYSIZE(baseDir))) return false;
    PathRemoveFileSpecW(baseDir);

    // 1. Рядом с модулем
    wnsprintfW(outPath, (int)maxLen, L"%s\\bpdf_shell.ini", baseDir);
    if (PathFileExistsW(outPath)) return true;

    // 2. В подпапке shell
    wnsprintfW(outPath, (int)maxLen, L"%s\\shell\\bpdf_shell.ini", baseDir);
    if (PathFileExistsW(outPath)) return true;

    // 3. В родительской папке
    wchar_t cand[MAX_PATH * 2];
    wnsprintfW(cand, ARRAYSIZE(cand), L"%s\\..\\bpdf_shell.ini", baseDir);
    wchar_t full[MAX_PATH * 2];
    if (GetFullPathNameW(cand, ARRAYSIZE(full), full, NULL) > 0 && PathFileExistsW(full)) {
        lstrcpynW(outPath, full, (int)maxLen);
        return true;
    }

    return false;
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

    // Check UTF-16 LE BOM (FF FE)
    if (bytesRead >= 2 && (unsigned char)buf[0] == 0xFF && (unsigned char)buf[1] == 0xFE) {
        size_t charCount = (bytesRead - 2) / sizeof(wchar_t);
        outText.reserve(charCount);
        wmemcpy(outText.data, (const wchar_t*)(buf + 2), charCount);
        outText.data[charCount] = L'\0';
        outText.len = charCount;
        free(buf);
        return true;
    }

    int offset = 0;
    if (bytesRead >= 3 && (unsigned char)buf[0] == 0xEF && (unsigned char)buf[1] == 0xBB && (unsigned char)buf[2] == 0xBF) {
        offset = 3;
    }

    int wLen = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, buf + offset, (int)bytesRead - offset, NULL, 0);
    if (wLen <= 0) {
        wLen = MultiByteToWideChar(CP_ACP, 0, buf + offset, (int)bytesRead - offset, NULL, 0);
        if (wLen <= 0) { free(buf); return false; }
        outText.reserve(wLen);
        MultiByteToWideChar(CP_ACP, 0, buf + offset, (int)bytesRead - offset, outText.data, wLen);
    } else {
        outText.reserve(wLen);
        MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, buf + offset, (int)bytesRead - offset, outText.data, wLen);
    }
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

static inline void ParseItemFields(const wchar_t* line, MenuItemDef& item)
{
    item.title[0] = L'\0';
    item.cmd[0] = L'\0';
    item.iconIndex = 0;
    item.guiMode = GUI_AUTO;

    if (!line || !line[0]) return;
    wchar_t* buf = _wcsdup(line);
    if (!buf) return;

    wchar_t* token = buf;
    while (token && *token) {
        wchar_t* nextBar = wcschr(token, L'|');
        if (nextBar) {
            *nextBar = L'\0';
            nextBar++;
        }

        wchar_t* part = Trim(token);
        wchar_t* colon = wcschr(part, L':');
        if (colon) {
            *colon = L'\0';
            wchar_t* key = Trim(part);
            wchar_t* val = Trim(colon + 1);

            if (StrCmpIW(key, L"title") == 0) lstrcpynW(item.title, val, ARRAYSIZE(item.title));
            else if (StrCmpIW(key, L"cmd") == 0) lstrcpynW(item.cmd, val, ARRAYSIZE(item.cmd));
            else if (StrCmpIW(key, L"icon") == 0) item.iconIndex = StrToIntW(val);
            else if (StrCmpIW(key, L"gui") == 0) {
                if (StrCmpIW(val, L"info") == 0) item.guiMode = GUI_INFO;
                else if (StrCmpIW(val, L"keep") == 0) item.guiMode = GUI_KEEP;
                else if (StrCmpIW(val, L"none") == 0 || StrCmpIW(val, L"hide") == 0 ||
                         StrCmpIW(val, L"off") == 0 || StrCmpIW(val, L"false") == 0 ||
                         StrCmpIW(val, L"no") == 0) item.guiMode = GUI_NONE;
                else item.guiMode = GUI_AUTO;
            }
        }
        token = nextBar;
    }
    free(buf);
}

static inline size_t ParseIniSection(const wchar_t* iniContent, const wchar_t* targetSection, MenuItemDef* outItems, size_t maxItems)
{
    if (!iniContent || !targetSection || maxItems == 0) return 0;

    wchar_t* buf = _wcsdup(iniContent);
    if (!buf) return 0;

    size_t count = 0;
    bool inTargetSection = false;
    wchar_t* line = buf;

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
                    MenuItemDef sepItem;
                    sepItem.title[0] = L'\0';
                    sepItem.cmd[0] = L'\0';
                    sepItem.iconIndex = -1;
                    sepItem.guiMode = GUI_AUTO;
                    outItems[count++] = sepItem;
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

    free(buf);
    return count;
}

// -----------------------------------------------------------------------------
// Command Expansion & Formatting
// -----------------------------------------------------------------------------

static inline void SanitizePathForQuotes(wchar_t* path)
{
    if (!path) return;
    size_t len = wcslen(path);
    while (len > 0 && path[len - 1] == L'\\') {
        path[--len] = L'\0';
    }
}

static inline void SanitizePathForQuotes(StrW& str)
{
    if (!str.data) return;
    while (str.len > 0 && str.data[str.len - 1] == L'\\') {
        str.data[--str.len] = L'\0';
    }
}

// Аргумент в кавычках: хвостовые '\' перед закрывающей кавычкой удваиваются (канон Windows CommandLineToArgvW)
static inline void AppendQuotedArg(StrW& out, const wchar_t* s)
{
    if (!s) return;
    size_t n = wcslen(s), tail = 0;
    while (tail < n && s[n - 1 - tail] == L'\\') tail++;
    out.append(L'"');
    out.append(s, n);
    for (size_t i = 0; i < tail; i++) out.append(L'\\');
    out.append(L'"');
}

static inline void SplitPathParts(const wchar_t* path, StrW& outFullPath, StrW& outDir, StrW& outName, StrW& outExt)
{
    outFullPath.assign(path);
    outExt.clear();

    if (!path || !path[0]) {
        outDir.clear();
        outName.clear();
        return;
    }

    wchar_t absBuf[MAX_PATH * 2] = {0};
    if (GetFullPathNameW(path, ARRAYSIZE(absBuf), absBuf, NULL) > 0) {
        path = absBuf;
        outFullPath.assign(absBuf);
    }

    // Check if path is a drive root like "D:\" or "D:/"
    if (path[0] && path[1] == L':' && (path[2] == L'\\' || path[2] == L'/') && path[3] == L'\0') {
        outDir.assign(path);
        wchar_t driveLetter[3] = { path[0], L':', L'\0' };
        outName.assign(driveLetter);
        return;
    }

    // Check if path is a directory
    bool isDirectory = (ClassifyPath(path) == KIND_DIRECTORY);

    if (isDirectory) {
        outDir.assign(path);
        SanitizePathForQuotes(outDir);

        const wchar_t* p = outDir.c_str();
        const wchar_t* lastSlash = wcsrchr(p, L'\\');
        const wchar_t* lastFSlash = wcsrchr(p, L'/');
        if (lastFSlash > lastSlash) lastSlash = lastFSlash;
        const wchar_t* dirName = lastSlash ? (lastSlash + 1) : p;
        outName.assign(dirName);
        return;
    }

    // Regular file
    outDir.assign(path);
    if (outDir.data) {
        wchar_t* lastSlash = wcsrchr(outDir.data, L'\\');
        wchar_t* lastFSlash = wcsrchr(outDir.data, L'/');
        if (lastFSlash > lastSlash) lastSlash = lastFSlash;
        if (lastSlash) {
            // If file is in root like "D:\file.pdf", preserve "D:\"
            if (lastSlash == outDir.data + 2 && outDir.data[1] == L':') {
                *(lastSlash + 1) = L'\0';
            } else {
                *lastSlash = L'\0';
            }
            outDir.len = wcslen(outDir.data);
        }
    }
    if (outDir.len > 3) {
        SanitizePathForQuotes(outDir);
    }

    const wchar_t* p = path;
    const wchar_t* lastSlash = wcsrchr(p, L'\\');
    const wchar_t* lastFSlash = wcsrchr(p, L'/');
    if (lastFSlash > lastSlash) lastSlash = lastFSlash;
    const wchar_t* fileName = lastSlash ? (lastSlash + 1) : p;

    outName.assign(fileName);

    const wchar_t* dot = wcsrchr(outName.c_str(), L'.');
    if (dot && dot > outName.c_str()) {
        outExt.assign(dot + 1);
        outName.data[dot - outName.c_str()] = L'\0';
        outName.len = dot - outName.c_str();
    }
}

static inline void ExpandCommand(const wchar_t* pattern, const wchar_t* fullPath, const wchar_t* dir, const wchar_t* name, const wchar_t* ext, StrW& outCmd, bool& outHasFile, const wchar_t* exeDir = NULL)
{
    outCmd.clear();
    outHasFile = false;
    if (!pattern) return;

    bool inQuotes = false;
    const wchar_t* p = pattern;
    while (*p) {
        if (*p == L'"') {
            inQuotes = !inQuotes;
            outCmd.append(*p);
            p++;
            continue;
        }

        if (*p == L'{') {
            if (wcsncmp(p, L"{file}", 6) == 0) {
                outHasFile = true;
                p += 6;
                if (inQuotes) {
                    outCmd.append(fullPath);
                } else {
                    AppendQuotedArg(outCmd, fullPath);
                }
                continue;
            } else if (wcsncmp(p, L"{dir}", 5) == 0) {
                outCmd.append(dir);
                p += 5;
                if (*p == L'\\' && dir && dir[0] && dir[wcslen(dir) - 1] == L'\\') {
                    p++;
                }
                continue;
            } else if (wcsncmp(p, L"{name}", 6) == 0) {
                outCmd.append(name);
                p += 6;
                continue;
            } else if (wcsncmp(p, L"{ext}", 5) == 0) {
                outCmd.append(ext);
                p += 5;
                continue;
            } else if (wcsncmp(p, L"{exe_dir}", 9) == 0) {
                if (exeDir && exeDir[0]) outCmd.append(exeDir);
                p += 9;
                continue;
            } else if (wcsncmp(p, L"{tool_dir}", 10) == 0) {
                if (exeDir && exeDir[0]) outCmd.append(exeDir);
                p += 10;
                continue;
            }
        }
        outCmd.append(*p);
        p++;
    }
}

static inline bool CreateManifestFile(const WStringList& files, wchar_t* outPath, size_t maxLen)
{
    wchar_t tempDir[MAX_PATH] = {0};
    GetTempPathW(ARRAYSIZE(tempDir), tempDir);

    wchar_t tempFile[MAX_PATH] = {0};
    if (!GetTempFileNameW(tempDir, L"bpdf", 0, tempFile)) return false;

    HANDLE hFile = CreateFileW(tempFile, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (hFile == INVALID_HANDLE_VALUE) {
        DeleteFileW(tempFile);
        return false;
    }

    bool writeOk = true;
    DWORD written = 0;
    for (size_t i = 0; i < files.size(); i++) {
        int utf8Len = WideCharToMultiByte(CP_UTF8, 0, files[i], -1, NULL, 0, NULL, NULL);
        if (utf8Len > 1) {
            char* utf8Buf = (char*)malloc(utf8Len);
            if (utf8Buf) {
                WideCharToMultiByte(CP_UTF8, 0, files[i], -1, utf8Buf, utf8Len, NULL, NULL);
                if (!WriteFile(hFile, utf8Buf, utf8Len - 1, &written, NULL) ||
                    !WriteFile(hFile, "\r\n", 2, &written, NULL))
                {
                    writeOk = false;
                }
                free(utf8Buf);
                if (!writeOk) break;
            } else {
                writeOk = false;
                break;
            }
        }
    }

    CloseHandle(hFile);
    if (!writeOk) {
        DeleteFileW(tempFile);
        return false;
    }
    lstrcpynW(outPath, tempFile, (int)maxLen);
    return true;
}

// -----------------------------------------------------------------------------
// Text Normalization (LF -> CRLF for multiline controls)
// -----------------------------------------------------------------------------

static inline wchar_t* NormalizeLineEndings(const wchar_t* src, int len, int& outLen)
{
    if (!src || len <= 0) { outLen = 0; return NULL; }
    int extra = 0;
    for (int i = 0; i < len; i++) {
        if (src[i] == L'\n' && (i == 0 || src[i - 1] != L'\r')) {
            extra++;
        }
    }
    if (extra == 0) {
        outLen = len;
        wchar_t* copy = (wchar_t*)malloc((len + 1) * sizeof(wchar_t));
        if (copy) { wmemcpy(copy, src, len); copy[len] = L'\0'; }
        return copy;
    }
    outLen = len + extra;
    wchar_t* dst = (wchar_t*)malloc((outLen + 1) * sizeof(wchar_t));
    if (!dst) return NULL;
    int j = 0;
    for (int i = 0; i < len; i++) {
        if (src[i] == L'\n' && (i == 0 || src[i - 1] != L'\r')) {
            dst[j++] = L'\r';
        }
        dst[j++] = src[i];
    }
    dst[j] = L'\0';
    return dst;
}
