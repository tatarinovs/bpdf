#include "bpdf_shell_common.h"
#include <shlobj.h>
#include <olectl.h>

#ifndef SELFREG_E_CLASS
#define SELFREG_E_CLASS ((HRESULT)0x80040201L)
#endif

// CLSID: {B9DF0001-B000-4000-8000-000000000001}
static const GUID CLSID_BpdfShell = 
{ 0xb9df0001, 0xb000, 0x4000, { 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01 } };

static const wchar_t* CLSID_STRING = L"{B9DF0001-B000-4000-8000-000000000001}";

static HINSTANCE g_hInst = NULL;
static LONG g_refCount = 0;
static CRITICAL_SECTION g_csShell;

static wchar_t g_cachedExePath[MAX_PATH * 2] = {0};
static bool g_cachedExeValid = false;

static wchar_t g_cachedIniPath[MAX_PATH * 2] = {0};
static bool g_cachedIniFound = false;
static FILETIME g_cachedIniWriteTime = {0, 0};
static StrW g_cachedIniContent;

static bool GetCachedBpdfExe(wchar_t* outPath, size_t maxLen)
{
    EnterCriticalSection(&g_csShell);
    if (g_cachedExeValid && GetFileAttributesW(g_cachedExePath) != INVALID_FILE_ATTRIBUTES) {
        lstrcpynW(outPath, g_cachedExePath, (int)maxLen);
        LeaveCriticalSection(&g_csShell);
        return true;
    }
    LeaveCriticalSection(&g_csShell);

    wchar_t exe[MAX_PATH * 2] = {0};
    if (FindBpdfExe(exe, ARRAYSIZE(exe), g_hInst)) {
        EnterCriticalSection(&g_csShell);
        lstrcpynW(g_cachedExePath, exe, ARRAYSIZE(g_cachedExePath));
        g_cachedExeValid = true;
        lstrcpynW(outPath, exe, (int)maxLen);
        LeaveCriticalSection(&g_csShell);
        return true;
    }
    return false;
}

static size_t LoadMenuConfig(const wchar_t* targetSection, MenuItemDef* outItems, size_t maxItems)
{
    EnterCriticalSection(&g_csShell);
    if (!g_cachedIniFound || GetFileAttributesW(g_cachedIniPath) == INVALID_FILE_ATTRIBUTES) {
        g_cachedIniFound = FindBpdfIni(g_cachedIniPath, ARRAYSIZE(g_cachedIniPath), g_hInst);
        g_cachedIniWriteTime.dwLowDateTime = 0;
        g_cachedIniWriteTime.dwHighDateTime = 0;
        g_cachedIniContent.clear();
    }

    if (g_cachedIniFound) {
        WIN32_FILE_ATTRIBUTE_DATA fad = {0};
        if (GetFileAttributesExW(g_cachedIniPath, GetFileExInfoStandard, &fad)) {
            if (fad.ftLastWriteTime.dwLowDateTime != g_cachedIniWriteTime.dwLowDateTime ||
                fad.ftLastWriteTime.dwHighDateTime != g_cachedIniWriteTime.dwHighDateTime ||
                g_cachedIniContent.length() == 0)
            {
                if (ReadTextFile(g_cachedIniPath, g_cachedIniContent)) {
                    g_cachedIniWriteTime = fad.ftLastWriteTime;
                }
            }
        }
    }

    size_t count = 0;
    if (g_cachedIniContent.length() > 0) {
        count = ParseIniSection(g_cachedIniContent.c_str(), targetSection, outItems, maxItems);
    }
    LeaveCriticalSection(&g_csShell);
    return count;
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

    EnterCriticalSection(&g_csShell);
    if (g_iconCacheInited[iconIndex]) {
        HBITMAP hBmp = g_iconCache[iconIndex];
        LeaveCriticalSection(&g_csShell);
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
    LeaveCriticalSection(&g_csShell);
    return hBmp;
}

static void ClearIconCache()
{
    EnterCriticalSection(&g_csShell);
    for (int i = 0; i < 32; i++) {
        if (g_iconCache[i]) {
            DeleteObject(g_iconCache[i]);
            g_iconCache[i] = NULL;
        }
        g_iconCacheInited[i] = false;
    }
    LeaveCriticalSection(&g_csShell);
}

// -----------------------------------------------------------------------------
// Process Spawning (Async GUI Launcher)
// -----------------------------------------------------------------------------

static void LaunchGui(const WStringList& files, int itemIndex, HWND hwndOwner)
{
    if (files.empty()) return;

    wchar_t guiExe[MAX_PATH * 2] = {0};
    if (!FindBpdfGui(guiExe, ARRAYSIZE(guiExe), g_hInst)) {
        MessageBoxW(
            hwndOwner,
            L"Исполняемый файл bpdf-gui.exe не найден.\n"
            L"Убедитесь, что bpdf-gui.exe находится рядом с bpdf_shell.dll или bpdf.exe.",
            L"bpdf",
            MB_ICONERROR
        );
        return;
    }

    wchar_t manifestPath[MAX_PATH * 2] = {0};
    if (!CreateManifestFile(files, manifestPath, ARRAYSIZE(manifestPath))) {
        MessageBoxW(hwndOwner, L"Не удалось создать файл списка аргументов", L"bpdf", MB_ICONERROR);
        return;
    }

    StrW guiCmd;
    guiCmd.append(L"\"");
    guiCmd.append(guiExe);
    guiCmd.append(L"\" --manifest \"");
    guiCmd.append(manifestPath);
    guiCmd.append(L"\"");

    if (itemIndex >= 0) {
        wchar_t numBuf[32];
        wnsprintfW(numBuf, ARRAYSIZE(numBuf), L" --item %d", itemIndex);
        guiCmd.append(numBuf);
    }

    StrW fullPath, dir, name, ext;
    SplitPathParts(files[0], fullPath, dir, name, ext);

    STARTUPINFOW si = { sizeof(si) };
    PROCESS_INFORMATION pi = { 0 };

    if (CreateProcessW(NULL, (LPWSTR)guiCmd.c_str(), NULL, NULL, FALSE, 0, NULL, dir.length() ? dir.c_str() : NULL, &si, &pi)) {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    } else {
        DeleteFileW(manifestPath);
        MessageBoxW(hwndOwner, L"Не удалось запустить bpdf-gui.exe", L"bpdf", MB_ICONERROR);
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
                    UINT len = DragQueryFileW(hDrop, i, NULL, 0);
                    if (len > 0) {
                        wchar_t* path = (wchar_t*)malloc((len + 1) * sizeof(wchar_t));
                        if (path) {
                            if (DragQueryFileW(hDrop, i, path, len + 1) > 0) {
                                m_files.add(path);
                            }
                            free(path);
                        }
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
    STDMETHODIMP QueryContextMenu(HMENU hmenu, UINT indexMenu, UINT idCmdFirst, UINT idCmdLast, UINT uFlags) override
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
        GetCachedBpdfExe(exePath, ARRAYSIZE(exePath));

        wchar_t targetSection[64] = {0};
        GetSectionForFiles(m_files, targetSection, ARRAYSIZE(targetSection));

        m_itemCount = LoadMenuConfig(targetSection, m_items, ARRAYSIZE(m_items));

        UINT totalCommands = (UINT)m_itemCount;
        if (idCmdFirst + totalCommands + 1 > idCmdLast) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        HMENU hSubMenu = CreatePopupMenu();
        if (!hSubMenu) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        HBITMAP hLogoBmp = GetCachedMenuIcon(exePath, 0);

        if (m_itemCount > 0) {
            for (size_t i = 0; i < m_itemCount; i++) {
                MENUITEMINFOW mii = { sizeof(mii) };
                if (m_items[i].iconIndex == -1) {
                    mii.fMask = MIIM_FTYPE;
                    mii.fType = MFT_SEPARATOR;
                } else {
                    mii.fMask = MIIM_STRING | MIIM_ID;
                    mii.wID = idCmdFirst + (UINT)(i);
                    mii.dwTypeData = m_items[i].title;

                    HBITMAP hBmp = GetCachedMenuIcon(exePath, m_items[i].iconIndex);
                    if (hBmp) {
                        mii.fMask |= MIIM_BITMAP;
                        mii.hbmpItem = hBmp;
                    }
                }
                InsertMenuItemW(hSubMenu, (UINT)(i), TRUE, &mii);
            }
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
        UINT parentVerb = (m_itemCount > 0) ? (UINT)(m_itemCount) : 0;
        HWND hwndOwner = pici->hwnd;

        // Clicked individual action
        if (m_itemCount > 0 && verbOffset < m_itemCount) {
            size_t idx = verbOffset;
            if (m_items[idx].iconIndex != -1) {
                LaunchGui(m_files, (int)idx, hwndOwner);
                return S_OK;
            }
        } else if (verbOffset == parentVerb) {
            LaunchGui(m_files, -1, hwndOwner);
            return S_OK;
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
        if (!pExt) return E_OUTOFMEMORY;
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

BOOL WINAPI DllMain(HINSTANCE hInst, DWORD reason, LPVOID lpReserved)
{
    if (reason == DLL_PROCESS_ATTACH) {
        g_hInst = hInst;
        DisableThreadLibraryCalls(hInst);
        InitializeCriticalSection(&g_csShell);
    } else if (reason == DLL_PROCESS_DETACH) {
        if (!lpReserved) {
            ClearIconCache();
            DeleteCriticalSection(&g_csShell);
        }
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
    if (!pFactory) return E_OUTOFMEMORY;
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

    bool ok = true;
    ok &= SetRegString(HKEY_CURRENT_USER, clsidKey, NULL, L"bpdf Shell Extension");
    ok &= SetRegString(HKEY_CURRENT_USER, inprocKey, NULL, dllPath);
    ok &= SetRegString(HKEY_CURRENT_USER, inprocKey, L"ThreadingModel", L"Apartment");

    ok &= SetRegString(HKEY_CURRENT_USER, L"Software\\Classes\\*\\shellex\\ContextMenuHandlers\\bpdf", NULL, CLSID_STRING);
    ok &= SetRegString(HKEY_CURRENT_USER, L"Software\\Classes\\Folder\\shellex\\ContextMenuHandlers\\bpdf", NULL, CLSID_STRING);
    ok &= SetRegString(HKEY_CURRENT_USER, L"Software\\Classes\\Directory\\Background\\shellex\\ContextMenuHandlers\\bpdf", NULL, CLSID_STRING);

    if (!ok) {
        return SELFREG_E_CLASS;
    }

    SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, NULL, NULL);
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

    SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, NULL, NULL);
    return S_OK;
}

