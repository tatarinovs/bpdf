#include "bpdf_shell_common.h"
#include <shlobj.h>

// CLSID: {B9DF0001-B000-4000-8000-000000000001}
static const GUID CLSID_BpdfShell = 
{ 0xb9df0001, 0xb000, 0x4000, { 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01 } };

static const wchar_t* CLSID_STRING = L"{B9DF0001-B000-4000-8000-000000000001}";

static HINSTANCE g_hInst = NULL;
static LONG g_refCount = 0;
static CRITICAL_SECTION g_csIconCache;

static inline bool FindDllBpdfExe(wchar_t* outPath, size_t maxLen)
{
    return FindBpdfExe(outPath, maxLen, g_hInst);
}

static inline bool FindDllBpdfGui(wchar_t* outPath, size_t maxLen)
{
    return FindBpdfGui(outPath, maxLen, g_hInst);
}

static inline bool FindDllBpdfIni(wchar_t* outPath, size_t maxLen)
{
    return FindBpdfIni(outPath, maxLen, g_hInst);
}

static size_t LoadMenuConfig(const wchar_t* targetSection, MenuItemDef* outItems, size_t maxItems)
{
    wchar_t iniPath[MAX_PATH * 2] = {0};
    StrW iniContent;
    if (FindDllBpdfIni(iniPath, ARRAYSIZE(iniPath)) && ReadTextFile(iniPath, iniContent)) {
        return ParseIniSection(iniContent.c_str(), targetSection, outItems, maxItems);
    }
    return 0;
}

// -----------------------------------------------------------------------------
// Icon Caching & Bitmap Conversion
// -----------------------------------------------------------------------------

static HBITMAP g_iconCache[32] = {0};
static bool g_iconCacheInited[32] = {0};

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

static HBITMAP GetCachedMenuIcon(const wchar_t* exePath, int iconIndex)
{
    if (iconIndex < 0 || iconIndex >= 32 || !exePath || !exePath[0]) return NULL;

    EnterCriticalSection(&g_csIconCache);
    if (g_iconCacheInited[iconIndex]) {
        HBITMAP hBmp = g_iconCache[iconIndex];
        LeaveCriticalSection(&g_csIconCache);
        return hBmp;
    }

    HICON hSmall = NULL;
    HBITMAP hBmp = NULL;
    if (ExtractIconExW(exePath, iconIndex, NULL, &hSmall, 1) > 0 && hSmall) {
        hBmp = CreateMenuBitmapFromIcon(hSmall);
        DestroyIcon(hSmall);
    }

    g_iconCache[iconIndex] = hBmp;
    g_iconCacheInited[iconIndex] = true;
    LeaveCriticalSection(&g_csIconCache);
    return hBmp;
}

static void ClearIconCache()
{
    EnterCriticalSection(&g_csIconCache);
    for (int i = 0; i < 32; i++) {
        if (g_iconCache[i]) {
            DeleteObject(g_iconCache[i]);
            g_iconCache[i] = NULL;
        }
        g_iconCacheInited[i] = false;
    }
    LeaveCriticalSection(&g_csIconCache);
}

// -----------------------------------------------------------------------------
// Process Spawning (Async GUI Launcher)
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// Process Spawning (Async GUI Launcher)
// -----------------------------------------------------------------------------

static void ExecutePalette(const WStringList& files)
{
    if (files.empty()) return;

    wchar_t guiExe[MAX_PATH * 2] = {0};
    if (!FindDllBpdfGui(guiExe, ARRAYSIZE(guiExe))) {
        MessageBoxW(
            NULL,
            L"Исполняемый файл bpdf-gui.exe не найден.\n"
            L"Убедитесь, что bpdf-gui.exe находится рядом с bpdf_shell.dll или bpdf.exe.",
            L"bpdf",
            MB_ICONERROR
        );
        return;
    }

    wchar_t manifestPath[MAX_PATH * 2] = {0};
    if (!CreateManifestFile(files, manifestPath, ARRAYSIZE(manifestPath))) {
        MessageBoxW(NULL, L"Не удалось создать файл списка аргументов", L"bpdf", MB_ICONERROR);
        return;
    }

    StrW guiCmd;
    guiCmd.append(L"\"");
    guiCmd.append(guiExe);
    guiCmd.append(L"\" --palette \"");
    guiCmd.append(manifestPath);
    guiCmd.append(L"\"");

    wchar_t dir[MAX_PATH * 2] = {0};
    lstrcpynW(dir, files[0], ARRAYSIZE(dir));
    PathRemoveFileSpecW(dir);

    STARTUPINFOW si = { sizeof(si) };
    PROCESS_INFORMATION pi = { 0 };

    if (CreateProcessW(NULL, (LPWSTR)guiCmd.c_str(), NULL, NULL, FALSE, 0, NULL, dir[0] ? dir : NULL, &si, &pi)) {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    } else {
        DeleteFileW(manifestPath);
        MessageBoxW(NULL, L"Не удалось запустить bpdf-gui.exe", L"bpdf", MB_ICONERROR);
    }
}

static void ExecuteCustomItem(const MenuItemDef& item, const WStringList& files)
{
    if (files.empty()) return;

    wchar_t exe[MAX_PATH * 2] = {0};
    if (!FindDllBpdfExe(exe, ARRAYSIZE(exe))) {
        MessageBoxW(
            NULL,
            L"Исполняемый файл bpdf.exe не найден.\n"
            L"Убедитесь, что bpdf.exe находится в одной папке с shell или добавлен в PATH.",
            L"bpdf",
            MB_ICONERROR
        );
        return;
    }

    wchar_t fullPath[MAX_PATH * 2] = {0};
    wchar_t dir[MAX_PATH * 2] = {0};
    wchar_t name[MAX_PATH * 2] = {0};
    wchar_t ext[64] = {0};

    lstrcpynW(fullPath, files[0], ARRAYSIZE(fullPath));
    lstrcpynW(dir, files[0], ARRAYSIZE(dir));
    PathRemoveFileSpecW(dir);

    SanitizePathForQuotes(fullPath);
    SanitizePathForQuotes(dir);

    lstrcpynW(name, PathFindFileNameW(files[0]), ARRAYSIZE(name));
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
    fullCmdLine.append(exe);
    fullCmdLine.append(L"\" --no-pause ");
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
            fullCmdLine.append(fullPath);
        }
        fullCmdLine.append(L"\"");
    }

    wchar_t guiExe[MAX_PATH * 2] = {0};
    if (FindDllBpdfGui(guiExe, ARRAYSIZE(guiExe))) {
        wchar_t jobPath[MAX_PATH * 2] = {0};
        if (CreateJobFile(
                item.title[0] ? item.title : L"Операция",
                item.iconIndex,
                (int)item.guiMode,
                dir,
                manifestPath,
                exe,
                fullCmdLine.c_str(),
                files,
                jobPath,
                ARRAYSIZE(jobPath)
            ))
        {
            StrW guiCmd;
            guiCmd.append(L"\"");
            guiCmd.append(guiExe);
            guiCmd.append(L"\" --job \"");
            guiCmd.append(jobPath);
            guiCmd.append(L"\"");

            STARTUPINFOW si = { sizeof(si) };
            PROCESS_INFORMATION pi = { 0 };
            if (CreateProcessW(NULL, (LPWSTR)guiCmd.c_str(), NULL, NULL, FALSE, 0, NULL, dir[0] ? dir : NULL, &si, &pi)) {
                CloseHandle(pi.hProcess);
                CloseHandle(pi.hThread);
                return;
            } else {
                DeleteFileW(jobPath);
            }
        }
    }

    // Fallback: direct console launch if bpdf-gui.exe is missing
    STARTUPINFOW si = { sizeof(si) };
    PROCESS_INFORMATION pi = { 0 };
    if (CreateProcessW(NULL, (LPWSTR)fullCmdLine.c_str(), NULL, NULL, FALSE, CREATE_NEW_CONSOLE, NULL, dir[0] ? dir : NULL, &si, &pi)) {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    } else {
        if (manifestPath[0]) DeleteFileW(manifestPath);
        MessageBoxW(NULL, L"Не удалось запустить команду bpdf", L"bpdf", MB_ICONERROR);
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
    MenuItemDef m_items[48];
    size_t m_itemCount;

public:
    CBpdfContextMenu() : m_refCount(1), m_itemCount(0)
    {
        InterlockedIncrement(&g_refCount);
    }

    ~CBpdfContextMenu()
    {
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

        FileKind firstKind = ClassifyPath(m_files[0]);
        bool anySupported = (firstKind != KIND_UNSUPPORTED);
        if (!anySupported) {
            for (size_t i = 1; i < m_files.size(); i++) {
                if (ClassifyPath(m_files[i]) != KIND_UNSUPPORTED) {
                    anySupported = true;
                    break;
                }
            }
        }
        if (!anySupported) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        wchar_t exePath[MAX_PATH * 2] = {0};
        FindDllBpdfExe(exePath, ARRAYSIZE(exePath));

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

        m_itemCount = LoadMenuConfig(targetSection, m_items, ARRAYSIZE(m_items));

        HMENU hSubMenu = CreatePopupMenu();
        if (!hSubMenu) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        // Submenu Item 0: Palette launcher
        MENUITEMINFOW palMii = { sizeof(palMii) };
        palMii.fMask = MIIM_STRING | MIIM_ID;
        palMii.wID = idCmdFirst + 0;
        palMii.dwTypeData = const_cast<LPWSTR>(L"Панель действий bpdf...");
        HBITMAP hLogoBmp = GetCachedMenuIcon(exePath, 0);
        if (hLogoBmp) {
            palMii.fMask |= MIIM_BITMAP;
            palMii.hbmpItem = hLogoBmp;
        }
        InsertMenuItemW(hSubMenu, 0, TRUE, &palMii);

        UINT totalCommands = 1;

        if (m_itemCount > 0) {
            // Submenu Item 1: Separator
            MENUITEMINFOW sepMii = { sizeof(sepMii) };
            sepMii.fMask = MIIM_FTYPE;
            sepMii.fType = MFT_SEPARATOR;
            InsertMenuItemW(hSubMenu, 1, TRUE, &sepMii);

            // Submenu Items 2..N: Custom actions
            for (size_t i = 0; i < m_itemCount; i++) {
                MENUITEMINFOW mii = { sizeof(mii) };
                if (m_items[i].iconIndex == -1) {
                    mii.fMask = MIIM_FTYPE;
                    mii.fType = MFT_SEPARATOR;
                } else {
                    mii.fMask = MIIM_STRING | MIIM_ID;
                    mii.wID = idCmdFirst + (UINT)(2 + i);
                    mii.dwTypeData = m_items[i].title;

                    HBITMAP hBmp = GetCachedMenuIcon(exePath, m_items[i].iconIndex);
                    if (hBmp) {
                        mii.fMask |= MIIM_BITMAP;
                        mii.hbmpItem = hBmp;
                    }
                }
                InsertMenuItemW(hSubMenu, (UINT)(2 + i), TRUE, &mii);
            }
            totalCommands = (UINT)(2 + m_itemCount);
        }

        // Parent cascaded menu item
        MENUITEMINFOW parentMii = { sizeof(parentMii) };
        parentMii.fMask = MIIM_STRING | MIIM_SUBMENU | MIIM_ID;
        parentMii.wID = idCmdFirst + totalCommands;
        parentMii.hSubMenu = hSubMenu;
        parentMii.dwTypeData = const_cast<LPWSTR>(L"bpdf");

        if (hLogoBmp) {
            parentMii.fMask |= MIIM_BITMAP;
            parentMii.hbmpItem = hLogoBmp;
        }

        if (!InsertMenuItemW(hmenu, indexMenu, TRUE, &parentMii)) {
            DestroyMenu(hSubMenu);
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        return MAKE_HRESULT(SEVERITY_SUCCESS, 0, (ULONG)(totalCommands + 1));
    }

    STDMETHODIMP InvokeCommand(LPCMINVOKECOMMANDINFO pici) override
    {
        if (!pici) return E_POINTER;
        if (!IS_INTRESOURCE(pici->lpVerb)) return E_FAIL;

        UINT verbOffset = LOWORD(pici->lpVerb);
        UINT parentVerb = (m_itemCount > 0) ? (UINT)(2 + m_itemCount) : 1;

        // Clicked Item 0 ("Панель действий bpdf...") or parent menu item
        if (verbOffset == 0 || verbOffset == parentVerb) {
            ExecutePalette(m_files);
            return S_OK;
        }

        // Clicked individual action
        if (m_itemCount > 0 && verbOffset >= 2 && verbOffset < 2 + m_itemCount) {
            size_t idx = verbOffset - 2;
            if (m_items[idx].iconIndex != -1) {
                ExecuteCustomItem(m_items[idx], m_files);
                return S_OK;
            }
        }

        return E_FAIL;
    }

    STDMETHODIMP GetCommandString(UINT_PTR, UINT, UINT*, LPSTR, UINT) override
    {
        return E_NOTIMPL;
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
    CBpdfClassFactory() : m_refCount(1)
    {
        InterlockedIncrement(&g_refCount);
    }

    ~CBpdfClassFactory()
    {
        InterlockedDecrement(&g_refCount);
    }

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
        CBpdfContextMenu* pExt = new CBpdfContextMenu();
        HRESULT hr = pExt->QueryInterface(riid, ppv);
        pExt->Release();
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
        InitializeCriticalSection(&g_csIconCache);
    } else if (reason == DLL_PROCESS_DETACH) {
        ClearIconCache();
        DeleteCriticalSection(&g_csIconCache);
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
