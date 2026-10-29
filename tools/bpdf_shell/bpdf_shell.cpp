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

// Paths of bpdf.exe (menu icons) and bpdf-gui.exe live as long as Explorer.
// A cached path costs one existence check per use; a missing or moved file is
// searched for again, so putting it in place needs no Explorer restart.
static SRWLOCK g_pathsLock = SRWLOCK_INIT;
static wchar_t g_exePath[MAX_PATH * 2] = {0};
static wchar_t g_guiPath[MAX_PATH * 2] = {0};

typedef bool (*FindPathFn)(wchar_t* outPath, size_t maxLen, HMODULE hMod);

static bool ResolvePath(wchar_t* cache, FindPathFn find, wchar_t* outPath, size_t maxLen)
{
    AcquireSRWLockExclusive(&g_pathsLock);
    if (!cache[0] || GetFileAttributesW(cache) == INVALID_FILE_ATTRIBUTES) {
        if (!find(cache, MAX_PATH * 2, g_hInst)) cache[0] = L'\0';
    }
    bool found = cache[0] != L'\0';
    if (found) lstrcpynW(outPath, cache, (int)maxLen);
    ReleaseSRWLockExclusive(&g_pathsLock);
    return found;
}

// -----------------------------------------------------------------------------
// Icon Caching & Bitmap Conversion (Lock-free thread-safe)
// -----------------------------------------------------------------------------

static HBITMAP g_iconCache[MAX_SHELL_ACTIONS] = {0};

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

    HDC hdcMem = CreateCompatibleDC(NULL);
    if (!hdcMem) return NULL;

    void* pBits = NULL;
    HBITMAP hBmp = CreateDIBSection(hdcMem, &bi, DIB_RGB_COLORS, &pBits, NULL, 0);

    if (hBmp && pBits) {
        HGDIOBJ hOld = SelectObject(hdcMem, hBmp);
        DrawIconEx(hdcMem, 0, 0, hIcon, cx, cy, 0, NULL, DI_NORMAL);

        DWORD* pPixels = (DWORD*)pBits;
        bool hasAlpha = false;
        int totalPixels = cx * cy;
        for (int i = 0; i < totalPixels; i++) {
            if ((pPixels[i] & 0xFF000000) != 0) {
                hasAlpha = true;
                break;
            }
        }
        if (!hasAlpha) {
            for (int i = 0; i < totalPixels; i++) {
                if ((pPixels[i] & 0x00FFFFFF) != 0) {
                    pPixels[i] |= 0xFF000000;
                }
            }
        }
        SelectObject(hdcMem, hOld);
    }

    DeleteDC(hdcMem);
    return hBmp;
}

// exePath is NULL while bpdf.exe is not found: the icon is not cached then
static HBITMAP GetCachedMenuIcon(int iconIndex, const wchar_t* exePath)
{
    if (iconIndex < 0 || iconIndex >= MAX_SHELL_ACTIONS) return NULL;
    if (g_iconCache[iconIndex]) return g_iconCache[iconIndex];
    if (!exePath) return NULL;

    HICON hSmall = NULL;
    HBITMAP hBmp = NULL;
    if (ExtractIconExW(exePath, iconIndex, NULL, &hSmall, 1) > 0 && hSmall) {
        hBmp = CreateMenuBitmapFromIcon(hSmall);
        DestroyIcon(hSmall);
    }

    if (hBmp) {
        HBITMAP prev = (HBITMAP)InterlockedCompareExchangePointer((PVOID*)&g_iconCache[iconIndex], hBmp, NULL);
        if (prev) {
            DeleteObject(hBmp);
            return prev;
        }
    }
    return hBmp;
}

static void ClearIconCache()
{
    for (int i = 0; i < MAX_SHELL_ACTIONS; i++) {
        HBITMAP h = (HBITMAP)InterlockedExchangePointer((PVOID*)&g_iconCache[i], NULL);
        if (h) DeleteObject(h);
    }
}

// -----------------------------------------------------------------------------
// Process Spawning (Async GUI Launcher)
// -----------------------------------------------------------------------------

static void LaunchGui(const WStringList& files, int itemIndex, HWND hwndOwner)
{
    if (files.empty()) return;

    wchar_t guiExe[MAX_PATH * 2];
    if (!ResolvePath(g_guiPath, FindBpdfGui, guiExe, ARRAYSIZE(guiExe))) {
        MessageBoxW(
            hwndOwner,
            Tr(L"Исполняемый файл bpdf-gui.exe не найден.\n"
               L"Убедитесь, что bpdf-gui.exe находится рядом с bpdf_shell.dll или bpdf.exe.",
               L"bpdf-gui.exe was not found.\n"
               L"Put bpdf-gui.exe beside bpdf_shell.dll or bpdf.exe."),
            L"bpdf",
            MB_ICONERROR
        );
        return;
    }

    StrW guiCmd;
    AppendQuotedArg(guiCmd, guiExe);
    guiCmd.append(L' ');

    size_t totalLen = 0;
    for (size_t i = 0; i < files.size(); i++) {
        totalLen += wcslen(files[i]) + 3; // quotes + space
    }

    wchar_t tempManifestToDelete[MAX_PATH * 2] = {0};

    if (files.size() <= 16 && totalLen < 3000) {
        for (size_t i = 0; i < files.size(); i++) {
            AppendQuotedArg(guiCmd, files[i]);
            guiCmd.append(L" ");
        }
    } else {
        wchar_t manifestPath[MAX_PATH * 2] = {0};
        if (!CreateManifestFile(files, manifestPath, ARRAYSIZE(manifestPath))) {
            MessageBoxW(hwndOwner, Tr(L"Не удалось создать файл списка аргументов", L"Could not create the argument list file"), L"bpdf", MB_ICONERROR);
            return;
        }
        StrW atManifest;
        atManifest.append(L"@");
        atManifest.append(manifestPath);
        AppendQuotedArg(guiCmd, atManifest.c_str());
        guiCmd.append(L" --delete-manifest ");
        lstrcpynW(tempManifestToDelete, manifestPath, ARRAYSIZE(tempManifestToDelete));
    }

    if (itemIndex >= 0) {
        wchar_t numBuf[32];
        wnsprintfW(numBuf, ARRAYSIZE(numBuf), L"--item %d", itemIndex);
        guiCmd.append(numBuf);
    }

    STARTUPINFOW si = { sizeof(si) };
    PROCESS_INFORMATION pi = { 0 };

    if (CreateProcessW(NULL, (LPWSTR)guiCmd.c_str(), NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) {
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    } else {
        if (tempManifestToDelete[0]) DeleteFileW(tempManifestToDelete);
        MessageBoxW(hwndOwner, Tr(L"Не удалось запустить bpdf-gui.exe", L"Could not start bpdf-gui.exe"), L"bpdf", MB_ICONERROR);
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
    UINT m_actionCount;

public:
    CBpdfContextMenu() : m_refCount(1), m_actionCount(0)
    {
        InterlockedIncrement(&g_refCount);
    }

    ~CBpdfContextMenu()
    {
        InterlockedDecrement(&g_refCount);
    }

    // No C++ runtime: allocation through the CRT heap
    static void* operator new(size_t size) noexcept { return calloc(1, size); }
    static void operator delete(void* p) noexcept { free(p); }

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
        // Paths are sized on the heap: the extension also runs on the threads of
        // Open/Save dialogs in other programs, whose stacks may be small
        m_files.clear();
        if (pdtobj) {
            FORMATETC fmt = { CF_HDROP, NULL, DVASPECT_CONTENT, -1, TYMED_HGLOBAL };
            STGMEDIUM stg;
            if (SUCCEEDED(pdtobj->GetData(&fmt, &stg))) {
                HDROP hDrop = (HDROP)stg.hGlobal;
                UINT count = DragQueryFileW(hDrop, 0xFFFFFFFF, NULL, 0);
                StrW path;
                for (UINT i = 0; i < count; i++) {
                    UINT len = DragQueryFileW(hDrop, i, NULL, 0);
                    if (len > 0 && path.reserve(len) && DragQueryFileW(hDrop, i, path.data, len + 1) > 0) {
                        m_files.add(path.data);
                    }
                }
                ReleaseStgMedium(&stg);
            }
        } else if (pidlFolder) {
            PWSTR folderPath = NULL;
            if (SUCCEEDED(SHGetNameFromIDList(pidlFolder, SIGDN_FILESYSPATH, &folderPath))) {
                m_files.add(folderPath);
                CoTaskMemFree(folderPath);
            }
        }
        return S_OK;
    }

    // IContextMenu
    STDMETHODIMP QueryContextMenu(HMENU hmenu, UINT indexMenu, UINT idCmdFirst, UINT idCmdLast, UINT uFlags) override
    {
        m_actionCount = 0;
        if ((uFlags & CMF_DEFAULTONLY) != 0 || m_files.empty()) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        bool anySupported = false;
        size_t checkCount = m_files.size() < 32 ? m_files.size() : 32;
        for (size_t i = 0; i < checkCount; i++) {
            if (ClassifyPath(m_files[i]) != KIND_UNSUPPORTED) {
                anySupported = true;
                break;
            }
        }
        if (!anySupported) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        wchar_t targetSection[64] = {0};
        GetSectionForFiles(m_files, targetSection, ARRAYSIZE(targetSection));

        MenuItemDef* items = (MenuItemDef*)calloc(MAX_INI_ITEMS, sizeof(MenuItemDef));
        if (!items) {
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }
        size_t totalItems = LoadMenuItems(targetSection, items, MAX_INI_ITEMS, g_hInst);
        if (totalItems == 0) {
            free(items);
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        HMENU hSubMenu = CreatePopupMenu();
        if (!hSubMenu) {
            free(items);
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        // Once per menu: icons not cached yet are taken from this bpdf.exe
        wchar_t exeBuf[MAX_PATH * 2];
        const wchar_t* exePath = ResolvePath(g_exePath, FindBpdfExe, exeBuf, ARRAYSIZE(exeBuf)) ? exeBuf : NULL;

        UINT actionIndex = 0;
        UINT menuPos = 0;
        for (size_t i = 0; i < totalItems; i++) {
            MENUITEMINFOW mii = { sizeof(mii) };
            if (items[i].iconIndex == -1 || items[i].title[0] == L'\0') {
                mii.fMask = MIIM_FTYPE;
                mii.fType = MFT_SEPARATOR;
                InsertMenuItemW(hSubMenu, menuPos++, TRUE, &mii);
            } else if (actionIndex < MAX_SHELL_ACTIONS) {
                mii.fMask = MIIM_STRING | MIIM_ID;
                mii.wID = idCmdFirst + actionIndex;
                mii.dwTypeData = items[i].title;

                HBITMAP hBmp = GetCachedMenuIcon(items[i].iconIndex, exePath);
                if (hBmp) {
                    mii.fMask |= MIIM_BITMAP;
                    mii.hbmpItem = hBmp;
                }
                actionIndex++;
                InsertMenuItemW(hSubMenu, menuPos++, TRUE, &mii);
            }
        }

        free(items);

        if (actionIndex == 0 || idCmdFirst + actionIndex > idCmdLast + 1) {
            DestroyMenu(hSubMenu);
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }

        MENUITEMINFOW parentMii = { sizeof(parentMii) };
        parentMii.fMask = MIIM_STRING | MIIM_SUBMENU;
        parentMii.hSubMenu = hSubMenu;
        parentMii.dwTypeData = const_cast<LPWSTR>(L"bpdf");

        HBITMAP hLogoBmp = GetCachedMenuIcon(0, exePath);
        if (hLogoBmp) {
            parentMii.fMask |= MIIM_BITMAP;
            parentMii.hbmpItem = hLogoBmp;
        }

        if (!InsertMenuItemW(hmenu, indexMenu, TRUE, &parentMii)) {
            DestroyMenu(hSubMenu);
            return MAKE_HRESULT(SEVERITY_SUCCESS, 0, 0);
        }
        m_actionCount = actionIndex;

        return MAKE_HRESULT(SEVERITY_SUCCESS, 0, (ULONG)m_actionCount);
    }

    STDMETHODIMP InvokeCommand(LPCMINVOKECOMMANDINFO pici) override
    {
        if (!pici || !IS_INTRESOURCE(pici->lpVerb)) return E_FAIL;

        UINT verbOffset = LOWORD(pici->lpVerb);
        if (verbOffset < m_actionCount) {
            LaunchGui(m_files, (int)verbOffset, pici->hwnd);
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

    static void* operator new(size_t size) noexcept { return calloc(1, size); }
    static void operator delete(void* p) noexcept { free(p); }

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
        // The static MSVC CRT (/MT) needs the thread notifications
#if !(defined(_MSC_VER) && !defined(_DLL))
        DisableThreadLibraryCalls(hInst);
#endif
    } else if (reason == DLL_PROCESS_DETACH) {
        if (!lpReserved) {
            ClearIconCache();
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

    // Register in Approved Shell Extensions under HKCU (supports modern Windows policy checks without Admin rights)
    SetRegString(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Shell Extensions\\Approved", CLSID_STRING, L"bpdf Shell Extension");

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

    // Remove from Approved Shell Extensions
    HKEY hKeyApproved = NULL;
    if (RegOpenKeyExW(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Shell Extensions\\Approved", 0, KEY_SET_VALUE, &hKeyApproved) == ERROR_SUCCESS) {
        RegDeleteValueW(hKeyApproved, CLSID_STRING);
        RegCloseKey(hKeyApproved);
    }

    wchar_t clsidKey[128];
    wnsprintfW(clsidKey, ARRAYSIZE(clsidKey), L"Software\\Classes\\CLSID\\%s", CLSID_STRING);
    RegDeleteTreeW(HKEY_CURRENT_USER, clsidKey);

    SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, NULL, NULL);
    return S_OK;
}