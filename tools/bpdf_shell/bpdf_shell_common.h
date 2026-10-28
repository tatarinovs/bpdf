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
inline void* operator new(size_t size) { return calloc(1, size); }
inline void operator delete(void* p) noexcept { if (p) free(p); }
inline void operator delete(void* p, size_t) noexcept { if (p) free(p); }
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

    DWORD attrs = GetFileAttributesW(path);
    if (attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_DIRECTORY)) {
        return KIND_DIRECTORY;
    }

    const wchar_t* dot = PathFindExtensionW(path);
    if (!dot || !dot[0] || !dot[1]) {
        return KIND_UNSUPPORTED;
    }
    const wchar_t* ext = dot + 1;

    if (StrCmpIW(ext, L"pdf") == 0) return KIND_PDF;
    if (MatchExt(ext, L".jpg.jpeg.png.webp.bmp.gif.tif.tiff.avif.heic.heif.cr2.nef.arw.dng.raf.")) return KIND_IMAGE;
    if (MatchExt(ext, L".docx.xlsx.pptx.rtf.odt.ods.odp.doc.xls.ppt.")) return KIND_OTHER_SUPPORTED;
    if (MatchExt(ext, L".html.htm.xhtml.svg.")) return KIND_OTHER_SUPPORTED;
    if (MatchExt(ext, L".epub.fb2.htmlz.djvu.xps.oxps.cbz.")) return KIND_OTHER_SUPPORTED;
    if (MatchExt(ext, L".txt.md.")) return KIND_OTHER_SUPPORTED;

    return KIND_UNSUPPORTED;
}

static inline FileKind GetFileKind(const wchar_t* path)
{
    return ClassifyPath(path);
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

    // 3. В системном PATH
    wchar_t searchPath[MAX_PATH * 2] = {0};
    if (SearchPathW(NULL, L"bpdf.exe", NULL, ARRAYSIZE(searchPath), searchPath, NULL) > 0) {
        if (PathFileExistsW(searchPath)) {
            lstrcpynW(outPath, searchPath, (int)maxLen);
            return true;
        }
    }

    return false;
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

    // 4. В системном PATH
    wchar_t searchPath[MAX_PATH * 2] = {0};
    if (SearchPathW(NULL, L"bpdf-gui.exe", NULL, ARRAYSIZE(searchPath), searchPath, NULL) > 0) {
        if (PathFileExistsW(searchPath)) {
            lstrcpynW(outPath, searchPath, (int)maxLen);
            return true;
        }
    }

    return false;
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

    char* buf = (char*)malloc(size + 1);
    if (!buf) { CloseHandle(hFile); return false; }

    DWORD bytesRead = 0;
    bool ok = ReadFile(hFile, buf, size, &bytesRead, NULL) && bytesRead > 0;
    CloseHandle(hFile);
    if (!ok) { free(buf); return false; }
    buf[bytesRead] = '\0';

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
        MultiByteToWideChar(CP_UTF8, 0, buf + offset, (int)bytesRead - offset, outText.data, wLen);
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

    wchar_t buf[1024];
    lstrcpynW(buf, line, ARRAYSIZE(buf));

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

static inline void SplitPathParts(const wchar_t* path, StrW& outFullPath, StrW& outDir, StrW& outName, StrW& outExt)
{
    outFullPath.assign(path);
    SanitizePathForQuotes(outFullPath);

    outDir.assign(path);
    if (outDir.data) {
        wchar_t* lastSlash = wcsrchr(outDir.data, L'\\');
        wchar_t* lastFSlash = wcsrchr(outDir.data, L'/');
        if (lastFSlash > lastSlash) lastSlash = lastFSlash;
        if (lastSlash) {
            if (lastSlash == outDir.data + 2 && outDir.data[1] == L':') {
                *(lastSlash + 1) = L'\0';
            } else {
                *lastSlash = L'\0';
            }
            outDir.len = wcslen(outDir.data);
        }
    }
    SanitizePathForQuotes(outDir);

    const wchar_t* p = path ? path : L"";
    const wchar_t* lastSlash = wcsrchr(p, L'\\');
    const wchar_t* lastFSlash = wcsrchr(p, L'/');
    if (lastFSlash > lastSlash) lastSlash = lastFSlash;
    const wchar_t* fileName = lastSlash ? (lastSlash + 1) : p;

    outName.assign(fileName);
    outExt.clear();

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

    const wchar_t* p = pattern;
    while (*p) {
        if (*p == L'{') {
            if (wcsncmp(p, L"{file}", 6) == 0) {
                outCmd.append(fullPath);
                outHasFile = true;
                p += 6;
                continue;
            } else if (wcsncmp(p, L"{dir}", 5) == 0) {
                outCmd.append(dir);
                p += 5;
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
    if (hFile == INVALID_HANDLE_VALUE) return false;

    DWORD written = 0;
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
