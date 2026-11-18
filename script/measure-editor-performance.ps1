<#
.SYNOPSIS
Measures memory, CPU and startup time of Solo, Zed and VS Code on Windows.

.DESCRIPTION
Runs each editor through the scenarios in docs/performance.md, with its own
data folders and its own copy of a project, and prints a table of medians
(and writes CSV and JSON next to it).

Every editor runs with a fresh data folder under -WorkRoot, so nothing it
does reaches the data of an editor the user runs. Solo and Zed also run with
ZED_STATELESS=1, which keeps them from handing off to an instance that's
already running. The script drives the editors only through their own
windows: PrintWindow to look at them, PostMessage to type into them, and
custom key bindings on F13-F24 (keys a keyboard rarely has) for the actions
it needs. It never uses global input, never activates a window and never
touches the clipboard. It only stops processes it started.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File script\measure-editor-performance.ps1

.EXAMPLE
# A quick pass: one run, Solo only, short idle windows.
powershell -ExecutionPolicy Bypass -File script\measure-editor-performance.ps1 -Editors solo -Runs 1 -IdleCpuSeconds 30
#>
[CmdletBinding()]
param(
    # Which editors to measure: solo, zed, vscode.
    [string[]]$Editors = @('solo', 'zed', 'vscode'),
    # Which scenarios: startup, a, b, c, d, e, f, g (see docs/performance.md).
    [string[]]$Scenarios = @('startup', 'a', 'b', 'c', 'd', 'e', 'f', 'g'),
    # Language servers off, on, or both (each reported separately).
    [ValidateSet('off', 'on', 'both')]
    [string]$LanguageServers = 'both',
    [int]$Runs = 3,
    # The number of the first run (to continue an interrupted series).
    [int]$FirstRun = 1,
    # Data folders, project copies, logs and results go here.
    [string]$WorkRoot = (Join-Path $env:TEMP 'solo-perf'),
    # The project to copy for scenarios b-g: by default this checkout.
    [string]$SourceRepo,
    # Executables; found automatically when left out.
    [string]$SoloExe,
    [string]$ZedExe,
    [string]$CodeExe,
    # Idle CPU is averaged over this long in scenarios a, b and f (idle terminals).
    [int]$IdleCpuSeconds = 300,
    # ...and over this long in the other scenarios and with language servers on.
    [int]$ShortCpuSeconds = 60,
    # How long terminals in scenario f print before they are measured.
    [int]$BusySeconds = 60,
    # The longest wait for an editor (and its language servers) to go quiet.
    [int]$SettleMaxSeconds = 120,
    [int]$LsSettleMaxSeconds = 900,
    [int]$Terminals = 10,
    # Folder with a data folder in which the editors have already downloaded
    # their language servers (languages\ and node\); see docs/performance.md.
    [string]$LsSeedRoot,
    # Also sample Solo's threads during the idle-terminals window.
    [switch]$ThreadProfile,
    # Also run rust-analyzer when language servers are on. Off by default: on
    # this project it takes 7-10 GB while it indexes.
    [switch]$WithRustAnalyzer,
    # Only summarize earlier results: folders with results-raw.json (for
    # editors measured in separate invocations).
    [string[]]$SummarizeFrom,
    # Only fill -LsSeedRoot: let Solo and Zed download their language
    # servers once (needs access to GitHub and npm).
    [switch]$PrepareLsSeed
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2

# ---------------------------------------------------------------------------
# Native helpers
# ---------------------------------------------------------------------------

Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;

public static class PerfNative
{
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr hWnd, uint cmd);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam, uint flags, uint timeout, out IntPtr result);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint mapType);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);

    // The visible, unowned top-level windows of these processes, largest first.
    public static IntPtr[] MainWindows(int[] pids)
    {
        var wanted = new HashSet<uint>();
        foreach (int p in pids) wanted.Add((uint)p);
        var found = new List<KeyValuePair<IntPtr, int>>();
        EnumWindows(delegate (IntPtr h, IntPtr l)
        {
            uint pid;
            GetWindowThreadProcessId(h, out pid);
            if (!wanted.Contains(pid) || !IsWindowVisible(h) || IsIconic(h)) return true;
            if (GetWindow(h, 4) != IntPtr.Zero) return true; // GW_OWNER: a popup
            RECT r;
            GetWindowRect(h, out r);
            int w = r.Right - r.Left, ht = r.Bottom - r.Top;
            if (w < 400 || ht < 300) return true;
            found.Add(new KeyValuePair<IntPtr, int>(h, w * ht));
            return true;
        }, IntPtr.Zero);
        found.Sort(delegate (KeyValuePair<IntPtr, int> a, KeyValuePair<IntPtr, int> b) { return b.Value.CompareTo(a.Value); });
        var result = new IntPtr[found.Count];
        for (int i = 0; i < found.Count; i++) result[i] = found[i].Key;
        return result;
    }

    public static string Title(IntPtr h)
    {
        var sb = new StringBuilder(512);
        GetWindowText(h, sb, sb.Capacity);
        return sb.ToString();
    }

    public static string ClassName(IntPtr h)
    {
        var sb = new StringBuilder(256);
        GetClassName(h, sb, sb.Capacity);
        return sb.ToString();
    }

    // Whether the window's thread handles a message within the timeout.
    public static bool Responds(IntPtr h, uint timeoutMs)
    {
        IntPtr result;
        return SendMessageTimeout(h, 0 /* WM_NULL */, IntPtr.Zero, IntPtr.Zero, 2 /* SMTO_ABORTIFHUNG */, timeoutMs, out result) != IntPtr.Zero;
    }

    // Captures the window's client area with PrintWindow. Returns a hash of
    // the pixels (0 on failure) and the number of distinct colors among a
    // sample of them; saves a PNG when savePath isn't null.
    public static long Capture(IntPtr h, string savePath, out int distinctColors)
    {
        distinctColors = 0;
        RECT r;
        if (!GetClientRect(h, out r)) return 0;
        int w = r.Right - r.Left, ht = r.Bottom - r.Top;
        if (w <= 0 || ht <= 0) return 0;
        using (var bmp = new Bitmap(w, ht, PixelFormat.Format32bppArgb))
        {
            using (var g = Graphics.FromImage(bmp))
            {
                IntPtr hdc = g.GetHdc();
                bool ok = PrintWindow(h, hdc, 1 | 2); // PW_CLIENTONLY | PW_RENDERFULLCONTENT
                g.ReleaseHdc(hdc);
                if (!ok) return 0;
            }
            if (!string.IsNullOrEmpty(savePath)) bmp.Save(savePath, ImageFormat.Png);
            var data = bmp.LockBits(new Rectangle(0, 0, w, ht), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
            try
            {
                int stride = data.Stride;
                var row = new byte[stride];
                ulong hash = 14695981039346656037UL;
                var colors = new HashSet<int>();
                var blocks = new int[BlocksX * BlocksY];
                for (int y = 0; y < ht; y++)
                {
                    Marshal.Copy(new IntPtr(data.Scan0.ToInt64() + (long)y * stride), row, 0, stride);
                    int by = y * BlocksY / ht;
                    for (int x = 0; x < w * 4; x++)
                    {
                        hash = (hash ^ row[x]) * 1099511628211UL;
                        blocks[by * BlocksX + (x / 4) * BlocksX / w] += row[x];
                    }
                    if (y % 7 == 0)
                    {
                        for (int x = 0; x < w; x += 5)
                        {
                            if (colors.Count < 4096) colors.Add(BitConverter.ToInt32(row, x * 4));
                        }
                    }
                }
                distinctColors = colors.Count;
                int perBlock = Math.Max(1, (w / BlocksX) * (ht / BlocksY) * 4);
                for (int i = 0; i < blocks.Length; i++) blocks[i] /= perBlock;
                LastBlocks = blocks;
                return (long)(hash & 0x7FFFFFFFFFFFFFFFUL) | 1;
            }
            finally { bmp.UnlockBits(data); }
        }
    }

    // The picture of the last Capture, as the mean byte value of each block
    // of a 48 x 32 grid.
    public const int BlocksX = 48, BlocksY = 32;
    public static int[] LastBlocks;

    // How many blocks differ noticeably between two pictures (a blinking
    // cursor changes one or two).
    public static int ChangedBlocks(int[] a, int[] b)
    {
        if (a == null || b == null || a.Length != b.Length) return int.MaxValue;
        int n = 0;
        for (int i = 0; i < a.Length; i++) if (Math.Abs(a[i] - b[i]) > 2) n++;
        return n;
    }

    static bool IsExtended(int vk)
    {
        return (vk >= 0x21 && vk <= 0x28) || vk == 0x2D || vk == 0x2E;
    }

    // A key press and release, posted to the window (no global input).
    public static void Key(IntPtr h, int vk)
    {
        uint sc = MapVirtualKey((uint)vk, 0);
        long lp = 1 | ((long)sc << 16) | (IsExtended(vk) ? (1L << 24) : 0);
        PostMessage(h, 0x0100, new IntPtr(vk), new IntPtr(lp));
        PostMessage(h, 0x0101, new IntPtr(vk), new IntPtr(lp | (1L << 30) | (1L << 31)));
    }

    // Tells the window it is active and has the keyboard focus, by posted
    // messages only: the system's foreground window and focus don't change.
    // Chromium (VS Code) drops key messages while it thinks it is inactive.
    // Asks the window to close, as its close button would.
    public static void Close(IntPtr h)
    {
        PostMessage(h, 0x0010 /* WM_CLOSE */, IntPtr.Zero, IntPtr.Zero);
    }

    public static void SayActive(IntPtr h)
    {
        PostMessage(h, 0x0006 /* WM_ACTIVATE */, new IntPtr(1) /* WA_ACTIVE */, IntPtr.Zero);
        PostMessage(h, 0x0007 /* WM_SETFOCUS */, IntPtr.Zero, IntPtr.Zero);
    }

    // Text, posted to the window as WM_CHAR messages.
    public static void Chars(IntPtr h, string text)
    {
        foreach (char c in text)
        {
            uint sc = MapVirtualKey((uint)char.ToUpperInvariant(c), 0);
            PostMessage(h, 0x0102, new IntPtr(c), new IntPtr(1 | ((long)sc << 16)));
        }
    }

    // --- processes ---

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct PROCESSENTRY32W
    {
        public uint dwSize, cntUsage, th32ProcessID;
        public IntPtr th32DefaultHeapID;
        public uint th32ModuleID, cntThreads, th32ParentProcessID;
        public int pcPriClassBase;
        public uint dwFlags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string szExeFile;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct THREADENTRY32
    {
        public uint dwSize, cntUsage, th32ThreadID, th32OwnerProcessID;
        public int tpBasePri, tpDeltaPri;
        public uint dwFlags;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct PROCESS_MEMORY_COUNTERS_EX
    {
        public uint cb, PageFaultCount;
        public UIntPtr PeakWorkingSetSize, WorkingSetSize, QuotaPeakPagedPoolUsage, QuotaPagedPoolUsage,
            QuotaPeakNonPagedPoolUsage, QuotaNonPagedPoolUsage, PagefileUsage, PeakPagefileUsage, PrivateUsage;
    }

    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool Process32FirstW(IntPtr snap, ref PROCESSENTRY32W e);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool Process32NextW(IntPtr snap, ref PROCESSENTRY32W e);
    [DllImport("kernel32.dll")] static extern bool Thread32First(IntPtr snap, ref THREADENTRY32 e);
    [DllImport("kernel32.dll")] static extern bool Thread32Next(IntPtr snap, ref THREADENTRY32 e);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern IntPtr OpenThread(uint access, bool inherit, uint tid);
    [DllImport("kernel32.dll")] static extern bool GetProcessTimes(IntPtr h, out long creation, out long exit, out long kernel, out long user);
    [DllImport("kernel32.dll")] static extern bool GetThreadTimes(IntPtr h, out long creation, out long exit, out long kernel, out long user);
    [DllImport("kernel32.dll")] static extern int GetThreadDescription(IntPtr h, out IntPtr description);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);
    [DllImport("psapi.dll")] static extern bool GetProcessMemoryInfo(IntPtr h, out PROCESS_MEMORY_COUNTERS_EX c, uint cb);
    [DllImport("ntdll.dll")] static extern int NtQueryInformationThread(IntPtr h, int cls, out IntPtr info, int len, IntPtr retLen);
    [DllImport("psapi.dll", CharSet = CharSet.Unicode)] static extern bool EnumProcessModulesEx(IntPtr h, [Out] IntPtr[] mods, int cb, out int needed, uint filter);
    [DllImport("psapi.dll", CharSet = CharSet.Unicode)] static extern uint GetModuleBaseName(IntPtr h, IntPtr mod, StringBuilder name, int size);
    [StructLayout(LayoutKind.Sequential)] struct MODULEINFO { public IntPtr lpBaseOfDll; public uint SizeOfImage; public IntPtr EntryPoint; }
    [DllImport("psapi.dll")] static extern bool GetModuleInformation(IntPtr h, IntPtr mod, out MODULEINFO info, uint cb);

    public class Proc
    {
        public int Pid, ParentPid;
        public string Name;
    }

    public static Proc[] Processes()
    {
        var list = new List<Proc>();
        IntPtr snap = CreateToolhelp32Snapshot(2 /* TH32CS_SNAPPROCESS */, 0);
        if (snap == new IntPtr(-1)) return list.ToArray();
        try
        {
            var e = new PROCESSENTRY32W();
            e.dwSize = (uint)Marshal.SizeOf(typeof(PROCESSENTRY32W));
            if (Process32FirstW(snap, ref e))
            {
                do
                {
                    list.Add(new Proc { Pid = (int)e.th32ProcessID, ParentPid = (int)e.th32ParentProcessID, Name = e.szExeFile });
                } while (Process32NextW(snap, ref e));
            }
        }
        finally { CloseHandle(snap); }
        return list.ToArray();
    }

    public class Stats
    {
        public bool Ok;
        public long Created, Cpu, PrivateBytes, WorkingSet;
    }

    // CPU time (100 ns units), private bytes and working set of a process.
    public static Stats ProcessStats(int pid)
    {
        var s = new Stats();
        IntPtr h = OpenProcess(0x1000 /* PROCESS_QUERY_LIMITED_INFORMATION */, false, (uint)pid);
        if (h == IntPtr.Zero) return s;
        try
        {
            long c, x, k, u;
            if (!GetProcessTimes(h, out c, out x, out k, out u)) return s;
            if (x != 0) return s; // exited
            var m = new PROCESS_MEMORY_COUNTERS_EX();
            m.cb = (uint)Marshal.SizeOf(typeof(PROCESS_MEMORY_COUNTERS_EX));
            if (!GetProcessMemoryInfo(h, out m, m.cb)) return s;
            s.Created = c;
            s.Cpu = k + u;
            s.PrivateBytes = (long)m.PrivateUsage.ToUInt64();
            s.WorkingSet = (long)m.WorkingSetSize.ToUInt64();
            s.Ok = true;
        }
        finally { CloseHandle(h); }
        return s;
    }

    public class ThreadInfo
    {
        public int Tid;
        public long Cpu;
        public string Name, Start;
    }

    // The threads of a process with their CPU time (100 ns units), name
    // (SetThreadDescription) and start address as module+offset.
    public static ThreadInfo[] Threads(int pid)
    {
        var modules = new List<KeyValuePair<string, MODULEINFO>>();
        IntPtr ph = OpenProcess(0x0400 | 0x0010 /* QUERY_INFORMATION | VM_READ */, false, (uint)pid);
        if (ph != IntPtr.Zero)
        {
            try
            {
                var mods = new IntPtr[2048];
                int needed;
                if (EnumProcessModulesEx(ph, mods, IntPtr.Size * mods.Length, out needed, 3))
                {
                    int count = Math.Min(needed / IntPtr.Size, mods.Length);
                    for (int i = 0; i < count; i++)
                    {
                        var name = new StringBuilder(260);
                        GetModuleBaseName(ph, mods[i], name, name.Capacity);
                        MODULEINFO mi;
                        if (GetModuleInformation(ph, mods[i], out mi, (uint)Marshal.SizeOf(typeof(MODULEINFO))))
                            modules.Add(new KeyValuePair<string, MODULEINFO>(name.ToString(), mi));
                    }
                }
            }
            finally { CloseHandle(ph); }
        }
        var list = new List<ThreadInfo>();
        IntPtr snap = CreateToolhelp32Snapshot(4 /* TH32CS_SNAPTHREAD */, 0);
        if (snap == new IntPtr(-1)) return list.ToArray();
        try
        {
            var e = new THREADENTRY32();
            e.dwSize = (uint)Marshal.SizeOf(typeof(THREADENTRY32));
            if (!Thread32First(snap, ref e)) return list.ToArray();
            do
            {
                if (e.th32OwnerProcessID != (uint)pid) continue;
                IntPtr th = OpenThread(0x0040 | 0x0800 /* QUERY_INFORMATION | QUERY_LIMITED_INFORMATION */, false, e.th32ThreadID);
                if (th == IntPtr.Zero) continue;
                try
                {
                    var t = new ThreadInfo { Tid = (int)e.th32ThreadID, Name = "", Start = "" };
                    long c, x, k, u;
                    if (GetThreadTimes(th, out c, out x, out k, out u)) t.Cpu = k + u;
                    IntPtr desc;
                    if (GetThreadDescription(th, out desc) >= 0 && desc != IntPtr.Zero)
                    {
                        t.Name = Marshal.PtrToStringUni(desc) ?? "";
                        LocalFree(desc);
                    }
                    IntPtr start;
                    if (NtQueryInformationThread(th, 9 /* ThreadQuerySetWin32StartAddress */, out start, IntPtr.Size, IntPtr.Zero) == 0)
                    {
                        long a = start.ToInt64();
                        t.Start = "0x" + a.ToString("x");
                        foreach (var m in modules)
                        {
                            long b = m.Value.lpBaseOfDll.ToInt64();
                            if (a >= b && a < b + m.Value.SizeOfImage)
                            {
                                t.Start = m.Key + "+0x" + (a - b).ToString("x");
                                break;
                            }
                        }
                    }
                    list.Add(t);
                }
                finally { CloseHandle(th); }
            } while (Thread32Next(snap, ref e));
        }
        finally { CloseHandle(snap); }
        return list.ToArray();
    }

    [DllImport("kernel32.dll")] static extern bool GetSystemTimes(out long idle, out long kernel, out long user);

    // Busy CPU time of the whole machine, summed over its processors (100 ns units).
    public static long SystemBusy()
    {
        long idle, kernel, user;
        if (!GetSystemTimes(out idle, out kernel, out user)) return 0;
        return kernel + user - idle; // kernel time includes idle time
    }

    // --- starting a process without activating its window ---

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct STARTUPINFO
    {
        public int cb;
        public string lpReserved, lpDesktop, lpTitle;
        public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
        public short wShowWindow, cbReserved2;
        public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool CreateProcess(string app, StringBuilder cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags, string env, string dir, ref STARTUPINFO si, out PROCESS_INFORMATION pi);

    // Starts a program with SW_SHOWNOACTIVATE, so its first window doesn't
    // take the keyboard from the user's window. env: "NAME=value\0..." + "\0".
    public static int Start(string exe, string commandLine, string env, string dir)
    {
        var si = new STARTUPINFO();
        si.cb = Marshal.SizeOf(typeof(STARTUPINFO));
        si.dwFlags = 1; // STARTF_USESHOWWINDOW
        si.wShowWindow = 4; // SW_SHOWNOACTIVATE
        PROCESS_INFORMATION pi;
        var cmd = new StringBuilder(commandLine, 32768);
        if (!CreateProcess(exe, cmd, IntPtr.Zero, IntPtr.Zero, false, 0x400 /* CREATE_UNICODE_ENVIRONMENT */, env, dir, ref si, out pi))
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
        return pi.dwProcessId;
    }
}
'@

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

# "-Editors solo,zed" through powershell -File arrives as one string.
$Editors = @($Editors | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
$Scenarios = @($Scenarios | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
if (-not $SourceRepo) { $SourceRepo = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path) }
$Cores = [Environment]::ProcessorCount
$RunId = Get-Date -Format 'yyyyMMdd-HHmmss'
$ResultDir = Join-Path $WorkRoot "results\$RunId"
$ProjectDir = Join-Path $WorkRoot 'project'
$LogFile = Join-Path $WorkRoot 'data\perf-200k.log'
$AgentScript = Join-Path $WorkRoot 'data\agent-sim.ps1'
$ModeFile = Join-Path $WorkRoot 'data\agent-mode.txt'
New-Item -ItemType Directory -Force $ResultDir, (Join-Path $WorkRoot 'data'), (Join-Path $WorkRoot 'runs') | Out-Null
$TraceFile = Join-Path $ResultDir 'trace.log'

function Write-Trace([string]$Message) {
    $line = '{0:HH:mm:ss} {1}' -f (Get-Date), $Message
    Write-Host $line
    # Another program (an indexer, a virus scanner) may hold the file a moment.
    for ($i = 0; $i -lt 5; $i++) {
        try { [IO.File]::AppendAllText($TraceFile, $line + "`r`n"); return } catch { Start-Sleep -Milliseconds 200 }
    }
}

# Tells the terminals' scripts to print or to stop (they read the file
# every second, which can block a write for a moment).
function Set-Mode([string]$Mode) {
    for ($i = 0; $i -lt 20; $i++) {
        try { [IO.File]::WriteAllText($ModeFile, $Mode); return } catch { Start-Sleep -Milliseconds 100 }
    }
    throw "couldn't write $ModeFile"
}

# Writes a results file, retrying while another program holds it.
function Save-File([string]$Path, [string]$Text) {
    for ($i = 0; $i -lt 10; $i++) {
        try { Write-TextFile $Path $Text; return } catch { Start-Sleep -Milliseconds 300 }
    }
    Write-Trace "  couldn't write $Path"
}

# The 20 files of scenario c, relative to the project.
$EditFiles = @(
    'crates/editor/src/editor.rs', 'crates/workspace/src/workspace.rs', 'crates/project/src/project.rs',
    'crates/gpui/src/window.rs', 'crates/terminal/src/terminal.rs', 'crates/language/src/buffer.rs',
    'crates/solo/src/main.rs', 'crates/paths/src/paths.rs', 'crates/search/src/project_search.rs',
    'crates/fs/src/fs.rs', 'crates/sum_tree/src/cursor.rs', 'crates/gpui/src/styled.rs',
    'crates/tab_switcher/src/tab_switcher.rs', 'crates/zlog/src/filter.rs', 'crates/path/src/path.rs',
    'crates/theme_settings/src/settings.rs', 'crates/util/src/paths.rs', 'Cargo.toml',
    'assets/settings/default.json', 'crates/prettier/src/prettier_server.js'
)
$SearchQueries = @('fn new(', 'impl Render for', 'cx.notify()')
$LogQuery = 'status=503'

# Function keys the custom key bindings use.
$VK = @{
    F13 = 0x7C; F14 = 0x7D; F15 = 0x7E; F16 = 0x7F; F17 = 0x80; F18 = 0x81
    F19 = 0x82; F20 = 0x83; F21 = 0x84; F22 = 0x85; F23 = 0x86; F24 = 0x87
    Enter = 0x0D; Escape = 0x1B; PageDown = 0x22; PageUp = 0x21; Home = 0x24
}

function Find-SoloExe {
    $package = Get-AppxPackage -Name 'lucas9801.SoloCode' -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $package) { return $null }
    $manifest = [xml](Get-Content -Raw (Join-Path $package.InstallLocation 'AppxManifest.xml'))
    $exe = $manifest.Package.Applications.Application | Select-Object -First 1 -ExpandProperty Executable
    Join-Path $package.InstallLocation $exe
}

if (-not $SoloExe) { $SoloExe = Find-SoloExe }
if (-not $ZedExe) { $ZedExe = Join-Path $env:LOCALAPPDATA 'Programs\Zed\Zed.exe' }
if (-not $CodeExe) { $CodeExe = Join-Path $WorkRoot 'apps\vscode\Code.exe' }

# Refuse to run Solo from anywhere a user's own Solo lives.
foreach ($forbidden in @('G:\Application\Solo', (Join-Path $env:LOCALAPPDATA 'Programs\Solo'))) {
    if ($SoloExe -and $SoloExe.StartsWith($forbidden, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to run the Solo at ${SoloExe}: that is a user's own installation."
    }
}

function Get-FileVersionText([string]$Path) {
    if (-not (Test-Path $Path)) { return 'missing' }
    $info = (Get-Item $Path).VersionInfo
    if ($info.ProductVersion) { $info.ProductVersion } else { $info.FileVersion }
}

# ---------------------------------------------------------------------------
# Test data
# ---------------------------------------------------------------------------

function New-TestData {
    if (-not (Test-Path $LogFile)) {
        Write-Trace "writing $LogFile"
        $random = New-Object System.Random 42
        $levels = @('INFO', 'INFO', 'INFO', 'DEBUG', 'WARN', 'ERROR')
        $statuses = @(200, 200, 200, 200, 201, 204, 304, 400, 404, 500, 503)
        $writer = New-Object System.IO.StreamWriter($LogFile, $false, (New-Object System.Text.UTF8Encoding $false))
        $time = [datetime]'2026-01-01T00:00:00'
        for ($i = 0; $i -lt 200000; $i++) {
            $time = $time.AddMilliseconds($random.Next(1, 400))
            $writer.WriteLine(('{0:yyyy-MM-ddTHH:mm:ss.fff}Z {1,-5} worker-{2:D2} request_id={3} path=/api/v1/items/{4} status={5} latency_ms={6} msg="handled request {3} for item {4}"' -f
                    $time, $levels[$random.Next($levels.Count)], $random.Next(16), (100000 + $i), $random.Next(100000),
                    $statuses[$random.Next($statuses.Count)], $random.Next(1, 2000)))
        }
        $writer.Close()
    }

    # What the terminals of scenario f run: output like a coding agent's
    # (a spinner redrawn in place, colored log lines, now and then a diff),
    # while the mode file says "active"; then it returns to the prompt.
    Set-Content -Path $AgentScript -Encoding ASCII -Value ((@'
$ModeFile = 'MODE_FILE'
function Get-Mode { try { [IO.File]::ReadAllText($ModeFile).Trim() } catch { $null } }
if ((Get-Mode) -ne 'active') { return }
$e = [char]27
$spin = '|/-\'
$random = New-Object System.Random $PID
$words = 'reading','editing','running','tests','crates','src','editor','workspace','terminal','buffer','project','search'
$out = [Console]::Out
$watch = [Diagnostics.Stopwatch]::StartNew()
$nextCheck = 1000; $nextDiff = 5000; $tick = 0
while ($true) {
    $tick++
    $line = ($words | Get-Random -Count 6 -SetSeed ($random.Next())) -join ' '
    # Clear the spinner line, print a log line, then redraw the spinner.
    $out.Write("`r$e[2K$e[32m*$e[0m $e[1m$line$e[0m $e[90m$($random.Next(100000))$e[0m`r`n")
    if ($watch.ElapsedMilliseconds -ge $nextDiff) {
        $nextDiff += 5000
        for ($i = 0; $i -lt 40; $i++) {
            if ($i % 2) { $out.Write("$e[31m- old line $i of the diff $($random.Next())$e[0m`r`n") }
            else { $out.Write("$e[32m+ new line $i of the diff $($random.Next())$e[0m`r`n") }
        }
    }
    $out.Write("$e[36m$($spin[$tick % 4])$e[0m Working... ($tick)")
    $out.Flush()
    if ($watch.ElapsedMilliseconds -ge $nextCheck) {
        $nextCheck += 1000
        $mode = Get-Mode
        if ($null -ne $mode -and $mode -ne 'active') { break }
    }
    Start-Sleep -Milliseconds 100
}
$out.Write("`r$e[2K$e[0mdone`r`n")
'@) -replace 'MODE_FILE', $ModeFile)
    Set-Mode 'idle'
}

# A clean copy of the project, as a git repository so the editors show its
# git status. Reset to its first commit before each session.
function Reset-Project {
    if (-not (Test-Path (Join-Path $ProjectDir '.git'))) {
        Write-Trace "copying $SourceRepo to $ProjectDir"
        if (Test-Path $ProjectDir) { Remove-Item -Recurse -Force $ProjectDir }
        # A shallow clone of the checkout's commit: one commit of history,
        # and symbolic links become plain files without needing a privilege.
        $url = 'file:///' + ((Resolve-Path $SourceRepo).Path -replace '\\', '/')
        & git -c core.symlinks=false clone -q --depth 1 --no-hardlinks $url $ProjectDir 2>$null
        if ($LASTEXITCODE) { throw 'git clone failed' }
    }
    & git -C $ProjectDir checkout -q -- . 2>$null
    & git -C $ProjectDir clean -q -fdx 2>$null
}

# ---------------------------------------------------------------------------
# Editor profiles
# ---------------------------------------------------------------------------

function ConvertTo-JsonText($Value) { ConvertTo-Json -Depth 20 -InputObject $Value }

# UTF-8 without a byte order mark, which Solo's and Zed's JSON parser rejects.
function Write-TextFile([string]$Path, [string]$Text) {
    [IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding $false))
}

# What scenario f types into each terminal (the editors' default shell,
# Windows PowerShell here).
function Get-AgentCommand { "& $AgentScript" }

# Settings and key bindings for Solo and Zed (the same format).
function Write-ZedConfig([string]$DataDir, [bool]$Ls) {
    $config = Join-Path $DataDir 'config'
    New-Item -ItemType Directory -Force $config | Out-Null
    $settings = [ordered]@{
        auto_update             = $false
        telemetry               = @{ diagnostics = $false; metrics = $false }
        autosave                = @{ after_delay = @{ milliseconds = 300 } }
        cursor_blink            = $false
        # No "Restricted Mode" prompt for the project copy.
        session                 = @{ trust_all_worktrees = $true }
        enable_language_server  = $Ls
        terminal                = @{ blinking = 'off' }
    }
    if ($WithRustAnalyzer) {
        # Keeps rust-analyzer from building the project (build scripts, proc
        # macros, cargo check), which would fill the disk and the CPU.
        $settings['lsp'] = @{
            'rust-analyzer' = @{
                initialization_options = @{
                    cargo        = @{ buildScripts = @{ enable = $false } }
                    procMacro    = @{ enable = $false }
                    checkOnSave  = $false
                }
            }
        }
    }
    else {
        $settings['languages'] = @{ Rust = @{ language_servers = @('!rust-analyzer', '...') } }
    }
    Write-TextFile (Join-Path $config 'settings.json') (ConvertTo-JsonText $settings)
    $bindings = [ordered]@{
        'f13' = 'file_finder::Toggle'
        'f14' = 'workspace::Save'
        'f15' = @('pane::DeploySearch', @{ query = $SearchQueries[0] })
        'f16' = @('pane::DeploySearch', @{ query = $SearchQueries[1] })
        'f17' = @('pane::DeploySearch', @{ query = $SearchQueries[2] })
        'f18' = 'buffer_search::Deploy'
        'f19' = 'workspace::NewTerminal'
        'f20' = 'workspace::CloseAllItemsAndPanes'
        'f21' = @('pane::CloseAllItems', @{ close_pinned = $true })
        'f22' = 'terminal_panel::Toggle'
        'f23' = 'workspace::ActivatePaneDown'
        'f24' = 'workspace::ActivatePaneUp'
    }
    $keymap = @(@{ bindings = $bindings })
    Write-TextFile (Join-Path $config 'keymap.json') (ConvertTo-JsonText $keymap)
}

function Write-CodeConfig([string]$DataDir) {
    $user = Join-Path $DataDir 'user-data\User'
    New-Item -ItemType Directory -Force $user, (Join-Path $DataDir 'extensions') | Out-Null
    $settings = [ordered]@{
        'telemetry.telemetryLevel'                      = 'off'
        'update.mode'                                   = 'none'
        'update.showReleaseNotes'                       = $false
        'extensions.autoUpdate'                         = $false
        'extensions.autoCheckUpdates'                   = $false
        'workbench.enableExperiments'                   = $false
        'workbench.startupEditor'                       = 'none'
        'workbench.tips.enabled'                        = $false
        'security.workspace.trust.enabled'              = $false
        'files.autoSave'                                = 'afterDelay'
        'files.autoSaveDelay'                           = 300
        'editor.cursorBlinking'                         = 'solid'
        'terminal.integrated.cursorBlinking'            = $false
        'terminal.integrated.confirmOnKill'             = 'never'
        'terminal.integrated.enablePersistentSessions'  = $false
    }
    Write-TextFile (Join-Path $user 'settings.json') (ConvertTo-JsonText $settings)
    $find = { param($q) [ordered]@{ query = $q; triggerSearch = $true; isRegex = $false } }
    $bindings = @(
        @{ key = 'f13'; command = 'workbench.action.quickOpen' },
        @{ key = 'f14'; command = 'workbench.action.files.save' },
        @{ key = 'f15'; command = 'workbench.action.findInFiles'; args = (& $find $SearchQueries[0]) },
        @{ key = 'f16'; command = 'workbench.action.findInFiles'; args = (& $find $SearchQueries[1]) },
        @{ key = 'f17'; command = 'workbench.action.findInFiles'; args = (& $find $SearchQueries[2]) },
        @{ key = 'f18'; command = 'actions.find' },
        @{ key = 'f19'; command = 'workbench.action.terminal.new' },
        @{ key = 'f20'; command = 'workbench.action.closeAllEditors' },
        @{ key = 'f21'; command = 'workbench.action.terminal.killAll' },
        @{ key = 'f22'; command = 'workbench.action.togglePanel' }
    )
    Write-TextFile (Join-Path $user 'keybindings.json') (ConvertTo-JsonText $bindings)
}

# Built-in VS Code extensions that run language servers; disabled when
# language servers are off.
$CodeLsExtensions = @(
    'vscode.typescript-language-features', 'vscode.json-language-features', 'vscode.css-language-features',
    'vscode.html-language-features', 'vscode.markdown-language-features', 'vscode.php-language-features'
)

function Get-EditorLaunch([string]$Editor, [string]$DataDir, [bool]$Ls, [string]$OpenPath) {
    $vars = @{}
    switch ($Editor) {
        { $_ -in 'solo', 'zed' } {
            $exe = if ($Editor -eq 'solo') { $SoloExe } else { $ZedExe }
            $arguments = @('--user-data-dir', $DataDir)
            $vars['ZED_STATELESS'] = '1'
        }
        'vscode' {
            $exe = $CodeExe
            # Portable mode in a folder of its own: user data, extensions and
            # argv.json (which otherwise is in the user's ~\.vscode) go there.
            $vars['VSCODE_PORTABLE'] = $DataDir
            $arguments = @('--disable-workspace-trust', '--skip-release-notes', '--skip-welcome', '--disable-telemetry', '--new-window')
            if (-not $Ls) { foreach ($id in $CodeLsExtensions) { $arguments += @('--disable-extension', $id) } }
        }
    }
    if ($OpenPath) { $arguments += $OpenPath }
    @{ Exe = $exe; Args = $arguments; Env = $vars }
}

function Initialize-DataDir([string]$Editor, [string]$DataDir, [bool]$Ls) {
    if (Test-Path $DataDir) { Remove-Item -Recurse -Force $DataDir }
    New-Item -ItemType Directory -Force $DataDir | Out-Null
    if ($Editor -eq 'vscode') { Write-CodeConfig $DataDir }
    else {
        Write-ZedConfig $DataDir $Ls
        # Language servers the editor downloaded earlier, so each run doesn't
        # download them again.
        if ($Ls -and $LsSeedRoot) {
            $seed = Join-Path $LsSeedRoot $Editor
            foreach ($sub in 'languages', 'node') {
                $from = Join-Path $seed $sub
                if (Test-Path $from) { Copy-Item -Recurse -Force $from (Join-Path $DataDir $sub) }
            }
        }
    }
}

# ---------------------------------------------------------------------------
# Processes
# ---------------------------------------------------------------------------

function Format-CommandLine([string[]]$Parts) {
    ($Parts | ForEach-Object {
            if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ }
        }) -join ' '
}

function Start-Editor($Launch) {
    $vars = [Environment]::GetEnvironmentVariables()
    foreach ($key in @($vars.Keys)) {
        # Nothing of the shell this runs in that steers an editor.
        if ($key -like 'ZED_*' -or $key -like 'SOLO_*' -or $key -like 'VSCODE_*' -or $key -like 'ELECTRON_*') { $vars.Remove($key) }
    }
    foreach ($key in $Launch.Env.Keys) { $vars[$key] = $Launch.Env[$key] }
    $block = (($vars.Keys | Sort-Object | ForEach-Object { "$_=$($vars[$_])" }) -join "`0") + "`0`0"
    $commandLine = Format-CommandLine (@($Launch.Exe) + $Launch.Args)
    $id = [PerfNative]::Start($Launch.Exe, $commandLine, $block, (Split-Path -Parent $Launch.Exe))
    $stats = [PerfNative]::ProcessStats($id)
    $tree = @{ Root = $id; Created = $stats.Created; Pids = @{ $id = $stats.Created }; Started = (Get-Date); Exe = $Launch.Exe; Cmd = @{} }
    $tree
}

# Adds new descendants of the tree's processes (started after their parent).
function Update-Tree($Tree) {
    $all = [PerfNative]::Processes()
    $changed = $true
    while ($changed) {
        $changed = $false
        foreach ($p in $all) {
            if ($Tree.Pids.ContainsKey($p.Pid) -or -not $Tree.Pids.ContainsKey($p.ParentPid)) { continue }
            $s = [PerfNative]::ProcessStats($p.Pid)
            if (-not $s.Ok -or $s.Created -lt $Tree.Pids[$p.ParentPid]) { continue }
            $Tree.Pids[$p.Pid] = $s.Created
            $changed = $true
        }
    }
    $alive = @{}
    foreach ($p in $all) { $alive[$p.Pid] = $p }
    foreach ($id in @($Tree.Pids.Keys)) {
        if (-not $alive.ContainsKey($id)) { $Tree.Pids.Remove($id); continue }
        $Tree.Pids[$id] | Out-Null
    }
    $Tree.Procs = $alive
}

function Get-CommandLine($Tree, [int]$ProcessId) {
    if (-not $Tree.Cmd.ContainsKey($ProcessId)) {
        $p = Get-CimInstance Win32_Process -Filter "ProcessId=$ProcessId" -ErrorAction SilentlyContinue
        $Tree.Cmd[$ProcessId] = if ($p -and $p.CommandLine) { $p.CommandLine } else { '' }
    }
    $Tree.Cmd[$ProcessId]
}

$TerminalNames = @('solo-terminal-host.exe', 'conhost.exe', 'openconsole.exe', 'powershell.exe', 'pwsh.exe', 'cmd.exe', 'bash.exe', 'wsl.exe')
$LsNames = @('rust-analyzer.exe', 'rust-analyzer-proc-macro-srv.exe', 'node.exe', 'json-language-server.exe',
    'typescript-language-server.exe', 'vtsls.exe', 'taplo.exe', 'package-version-server.exe')

# main, terminals, language-servers or other, by name, command line and parent.
function Get-Category($Tree, [int]$ProcessId, [hashtable]$Memo) {
    if ($Memo.ContainsKey($ProcessId)) { return $Memo[$ProcessId] }
    $category = 'other'
    if ($ProcessId -eq $Tree.Root) { $category = 'main' }
    else {
        $proc = $Tree.Procs[$ProcessId]
        $name = $proc.Name.ToLowerInvariant()
        $parent = $proc.ParentPid
        $parentCategory = if ($Tree.Pids.ContainsKey($parent)) { Get-Category $Tree $parent $Memo } else { 'other' }
        if ($parentCategory -in 'terminals', 'ls') { $category = $parentCategory }
        elseif ($name -in 'cmd.exe', 'node.exe' -and (Get-CommandLine $Tree $ProcessId) -match '(?i)npm|language-server|node_modules') { $category = 'ls' }
        elseif ($TerminalNames -contains $name) { $category = 'terminals' }
        elseif ($LsNames -contains $name -or $name -like '*language-server*' -or $name -like '*-ls.exe') { $category = 'ls' }
        elseif ($name -eq 'code.exe') {
            $cmd = Get-CommandLine $Tree $ProcessId
            # VS Code's pty host is the utility process the consoles of its
            # terminals are started by.
            $consoles = @($Tree.Pids.Keys | Where-Object {
                    $Tree.Procs[$_].ParentPid -eq $ProcessId -and $Tree.Procs[$_].Name -in 'conhost.exe', 'OpenConsole.exe', 'powershell.exe' })
            if ($cmd -match 'node\.mojom\.NodeService' -and $consoles.Count -gt 0) { $category = 'terminals' }
            elseif ($cmd -match '(?i)server(Main|\.js|Worker)|tsserver|languageServer|language-server') { $category = 'ls' }
            elseif ($cmd -match '--type=(renderer|gpu-process)' -or $cmd -match 'network\.mojom' -or $cmd -notmatch '--type=') { $category = 'main' }
            else { $category = 'other' }
        }
    }
    $Memo[$ProcessId] = $category
    $category
}

# Memory by category, in MB.
function Get-MemorySample($Tree) {
    Update-Tree $Tree
    $memo = @{}
    $result = [ordered]@{}
    foreach ($c in 'main', 'terminals', 'ls', 'other', 'total') { $result["${c}_private_mb"] = 0.0; $result["${c}_ws_mb"] = 0.0 }
    $detail = @()
    foreach ($id in @($Tree.Pids.Keys)) {
        $s = [PerfNative]::ProcessStats($id)
        if (-not $s.Ok) { continue }
        $c = Get-Category $Tree $id $memo
        $p = $s.PrivateBytes / 1MB; $w = $s.WorkingSet / 1MB
        $result["${c}_private_mb"] += $p; $result["${c}_ws_mb"] += $w
        $result['total_private_mb'] += $p; $result['total_ws_mb'] += $w
        $detail += [ordered]@{ pid = $id; name = $Tree.Procs[$id].Name; category = $c; private_mb = [math]::Round($p, 1); ws_mb = [math]::Round($w, 1) }
    }
    $result['processes'] = $detail.Count
    foreach ($k in @($result.Keys)) { if ($result[$k] -is [double]) { $result[$k] = [math]::Round($result[$k], 1) } }
    @{ Summary = $result; Detail = $detail }
}

# CPU time of the whole tree (100 ns units) by category.
function Get-CpuTimes($Tree) {
    Update-Tree $Tree
    $memo = @{}
    $times = @{ main = 0L; terminals = 0L; 'ls' = 0L; other = 0L; total = 0L }
    $perPid = @{}
    foreach ($id in @($Tree.Pids.Keys)) {
        $s = [PerfNative]::ProcessStats($id)
        if (-not $s.Ok) { continue }
        $c = Get-Category $Tree $id $memo
        $times[$c] += $s.Cpu; $times['total'] += $s.Cpu
        $perPid[$id] = @{ Cpu = $s.Cpu; Category = $c }
    }
    @{ Times = $times; PerPid = $perPid; At = [Diagnostics.Stopwatch]::GetTimestamp(); System = [PerfNative]::SystemBusy() }
}

# Average CPU between two Get-CpuTimes samples, in percent of one core.
# Processes that exist in both samples count; new ones count from zero;
# ones that exited in between are lost (rare when idle).
function Get-CpuPercent($Before, $After) {
    $seconds = ($After.At - $Before.At) / [Diagnostics.Stopwatch]::Frequency
    $delta = @{ main = 0L; terminals = 0L; 'ls' = 0L; other = 0L; total = 0L }
    foreach ($id in $After.PerPid.Keys) {
        $now = $After.PerPid[$id]
        $was = if ($Before.PerPid.ContainsKey($id)) { $Before.PerPid[$id].Cpu } else { 0L }
        $d = [math]::Max(0L, $now.Cpu - $was)
        $delta[$now.Category] += $d; $delta['total'] += $d
    }
    $result = [ordered]@{}
    foreach ($c in 'main', 'terminals', 'ls', 'other', 'total') {
        $result["${c}_cpu_pct"] = [math]::Round(100.0 * $delta[$c] / 1e7 / $seconds, 2)
    }
    # What everything else on the machine used meanwhile (other programs).
    $system = 100.0 * ($After.System - $Before.System) / 1e7 / $seconds
    $result['background_cpu_pct'] = [math]::Round([math]::Max(0, $system - $result['total_cpu_pct']), 1)
    $result
}

function Measure-Cpu($Tree, [int]$Seconds) {
    $before = Get-CpuTimes $Tree
    $end = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $end) {
        Start-Sleep -Seconds ([math]::Min(10, [math]::Max(1, ($end - (Get-Date)).TotalSeconds)))
        Update-Tree $Tree
        if (-not $Tree.Pids.ContainsKey($Tree.Root)) { throw 'the editor exited' }
    }
    Get-CpuPercent $before (Get-CpuTimes $Tree)
}

# Waits until the tree's CPU stays under ThresholdPct (of one core) for
# QuietSeconds in a row, or MaxSeconds pass. Returns the seconds it took.
function Wait-Quiet($Tree, [int]$MaxSeconds, [double]$ThresholdPct = 3, [int]$QuietSeconds = 5) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $quiet = 0
    $last = Get-CpuTimes $Tree
    while ($watch.Elapsed.TotalSeconds -lt $MaxSeconds) {
        Start-Sleep -Seconds 1
        $now = Get-CpuTimes $Tree
        if (-not $Tree.Pids.ContainsKey($Tree.Root)) { throw 'the editor exited' }
        $pct = (Get-CpuPercent $last $now)['total_cpu_pct']
        $last = $now
        if ($pct -lt $ThresholdPct) { $quiet++ } else { $quiet = 0 }
        if ($quiet -ge $QuietSeconds) { return [math]::Round($watch.Elapsed.TotalSeconds - $QuietSeconds, 1) }
    }
    Write-Trace "  did not go quiet in $MaxSeconds s"
    [math]::Round($watch.Elapsed.TotalSeconds, 1)
}

# Waits until the rest of the machine is quiet (other programs using less
# than MaxPct of one core for 15 s in a row), so they disturb less. Gives up
# after MaxMinutes; the background_cpu_pct of each result shows what was left.
function Wait-SystemQuiet([double]$MaxPct = 150, [int]$MaxMinutes = 5) {
    $deadline = (Get-Date).AddMinutes($MaxMinutes)
    $quiet = 0
    $last = [PerfNative]::SystemBusy()
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 1
        $now = [PerfNative]::SystemBusy()
        $pct = 100.0 * ($now - $last) / 1e7
        $last = $now
        if ($pct -lt $MaxPct) { $quiet++ } else { $quiet = 0 }
        if ($quiet -ge 15) { return }
    }
    Write-Trace "  the machine stayed busy (other programs); measuring anyway"
}

# Stops every process of the tree (only processes this script started and
# their descendants).
function Stop-Tree($Tree) {
    if (-not $Tree) { return }
    Update-Tree $Tree
    foreach ($id in @($Tree.Pids.Keys)) {
        Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2
    Update-Tree $Tree
    foreach ($id in @($Tree.Pids.Keys)) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
}

# Stops the interactive shells of the tree's terminals (not helper shells,
# such as the -NonInteractive one Solo runs for completions).
function Stop-Shells($Tree) {
    Update-Tree $Tree
    foreach ($id in @($Tree.Pids.Keys)) {
        if ($Tree.Procs[$id].Name -ne 'powershell.exe') { continue }
        if ((Get-CommandLine $Tree $id) -match '-NonInteractive') { continue }
        Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2
}

# Closes the editor through its window (WM_CLOSE), waits for all its
# processes to end, and stops whatever is left after 30 s. A killed VS Code
# can leave its next launch with the same data folder without a window.
function Close-Editor($Tree) {
    $hwnd = Get-MainWindow $Tree
    if ($hwnd -ne [IntPtr]::Zero) { [PerfNative]::Close($hwnd) }
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
        Update-Tree $Tree
        if ($Tree.Pids.Count -eq 0) { return }
    }
    Stop-Tree $Tree
}

function Get-MainWindow($Tree) {
    Update-Tree $Tree
    $windows = [PerfNative]::MainWindows([int[]]@($Tree.Pids.Keys))
    if ($windows.Count -gt 0) { $windows[0] } else { [IntPtr]::Zero }
}

# Startup time: until the window is "ready", meaning it is visible, its
# thread answers a message within 100 ms, its picture has some content (at
# least 24 colors among sampled pixels) and the picture stays the same for
# three PrintWindow captures in a row, 250 ms apart. The time reported is
# that of the first of the three. Also returns the time the window appeared.
function Wait-Ready($Tree, [int]$MaxSeconds = 90, [string]$ShotPath) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $windowAt = $null; $hwnd = [IntPtr]::Zero
    $lastBlocks = $null; $sameCount = 0; $firstSameAt = 0.0
    while ($watch.Elapsed.TotalSeconds -lt $MaxSeconds) {
        if ($hwnd -eq [IntPtr]::Zero) {
            $hwnd = Get-MainWindow $Tree
            if ($hwnd -eq [IntPtr]::Zero) { Start-Sleep -Milliseconds 50; continue }
            $windowAt = $watch.Elapsed.TotalSeconds
        }
        $t = $watch.Elapsed.TotalSeconds
        $colors = 0
        $hash = 0
        $blocks = $null
        if ([PerfNative]::Responds($hwnd, 100)) {
            $hash = [PerfNative]::Capture($hwnd, $null, [ref]$colors)
            if ($hash -ne 0) { $blocks = [PerfNative]::LastBlocks }
        }
        # "The same": at most 2 of the 1,536 blocks changed, so a blinking
        # cursor (or caret in a chat box) doesn't keep the window "busy".
        if ($blocks -and $colors -ge 24 -and [PerfNative]::ChangedBlocks($blocks, $lastBlocks) -le 2) {
            $sameCount++
            if ($sameCount -ge 2) {
                if ($ShotPath) { [void][PerfNative]::Capture($hwnd, $ShotPath, [ref]$colors) }
                return @{ Hwnd = $hwnd; WindowSeconds = [math]::Round($windowAt, 3); ReadySeconds = [math]::Round($firstSameAt, 3) }
            }
        }
        else { $sameCount = 0; $firstSameAt = $t }
        $lastBlocks = $blocks
        Start-Sleep -Milliseconds 250
    }
    if ($hwnd -ne [IntPtr]::Zero) { Save-Shot $hwnd ('not-ready-{0:HHmmss}' -f (Get-Date)) }
    throw "the window wasn't ready in $MaxSeconds s"
}

function Save-Shot($Hwnd, [string]$Name) {
    $colors = 0
    try { [void][PerfNative]::Capture($Hwnd, (Join-Path $ResultDir "$Name.png"), [ref]$colors) } catch { }
}

# ---------------------------------------------------------------------------
# Input (posted to the editor's own window only)
# ---------------------------------------------------------------------------

function Send-Key($Hwnd, [string]$Key, [int]$Count = 1, [int]$DelayMs = 120) {
    [PerfNative]::SayActive($Hwnd)
    Start-Sleep -Milliseconds 30
    for ($i = 0; $i -lt $Count; $i++) {
        [PerfNative]::Key($Hwnd, $VK[$Key])
        Start-Sleep -Milliseconds $DelayMs
    }
}

function Send-Text($Hwnd, [string]$Text) {
    foreach ($c in $Text.ToCharArray()) {
        [PerfNative]::Chars($Hwnd, [string]$c)
        Start-Sleep -Milliseconds 25
    }
}

function Open-File($Editor, $Hwnd, [string]$Path) {
    Send-Key $Hwnd 'F13' -DelayMs 700
    Send-Text $Hwnd $Path
    Start-Sleep -Milliseconds 1500
    Send-Key $Hwnd 'Enter' -DelayMs 1200
}

# ---------------------------------------------------------------------------
# Scenarios
# ---------------------------------------------------------------------------

$Results = New-Object System.Collections.ArrayList
$Details = New-Object System.Collections.ArrayList

function Add-Result([string]$Editor, [string]$LsMode, [int]$Run, [string]$Scenario, $Values) {
    $row = [ordered]@{ editor = $Editor; language_servers = $LsMode; run = $Run; scenario = $Scenario }
    foreach ($k in $Values.Keys) { $row[$k] = $Values[$k] }
    [void]$Results.Add([pscustomobject]$row)
    Save-File (Join-Path $ResultDir 'results-raw.json') (ConvertTo-Json -Depth 6 -InputObject @($Results))
}

function Measure-State($Tree, [string]$Editor, [string]$LsMode, [int]$Run, [string]$Scenario, [int]$CpuSeconds, $Extra) {
    $memory = Get-MemorySample $Tree
    $cpu = if ($CpuSeconds -gt 0) { Measure-Cpu $Tree $CpuSeconds } else { [ordered]@{} }
    $values = [ordered]@{}
    foreach ($k in $memory.Summary.Keys) { $values[$k] = $memory.Summary[$k] }
    foreach ($k in $cpu.Keys) { $values[$k] = $cpu[$k] }
    $values['cpu_seconds'] = $CpuSeconds
    if ($Extra) { foreach ($k in $Extra.Keys) { $values[$k] = $Extra[$k] } }
    Add-Result $Editor $LsMode $Run $Scenario $values
    [void]$Details.Add([ordered]@{ editor = $Editor; language_servers = $LsMode; run = $Run; scenario = $Scenario; processes = $memory.Detail })
    Save-File (Join-Path $ResultDir 'processes.json') (ConvertTo-Json -Depth 8 -InputObject @($Details))
    Write-Trace ('  {0}: private {1} MB, working set {2} MB, CPU {3}% of a core' -f $Scenario, $values['total_private_mb'], $values['total_ws_mb'],
        $(if ($values.Contains('total_cpu_pct')) { $values['total_cpu_pct'] } else { '-' }))
}

function Get-RunDir([string]$Editor, [string]$LsMode, [int]$Run, [string]$Session) {
    Join-Path $WorkRoot "runs\$Editor-ls$LsMode-run$Run-$Session"
}

# Startup (first launch with a fresh data folder, then two warm launches
# with the same folder) and scenario a (the empty window).
function Invoke-EmptySession([string]$Editor, [int]$Run) {
    $dataDir = Get-RunDir $Editor 'off' $Run 'empty'
    Initialize-DataDir $Editor $dataDir $false
    $launch = Get-EditorLaunch $Editor $dataDir $false $null
    $startups = @()
    $tree = $null
    Wait-SystemQuiet
    try {
        for ($i = 0; $i -lt 3; $i++) {
            $tree = Start-Editor $launch
            $ready = Wait-Ready $Tree -ShotPath $(if ($i -eq 0) { Join-Path $ResultDir "$Editor-run$Run-start.png" })
            Write-Trace ('  launch {0}: window {1} s, ready {2} s' -f ($i + 1), $ready.WindowSeconds, $ready.ReadySeconds)
            $startups += $ready
            if ($i -lt 2) {
                # Wait for it to settle, then close it before the next launch.
                [void](Wait-Quiet $tree $SettleMaxSeconds)
                Close-Editor $tree; $tree = $null
                Start-Sleep -Seconds 3
            }
        }
        if ($Scenarios -contains 'startup') {
            $warm = @($startups[1].ReadySeconds, $startups[2].ReadySeconds) | Sort-Object
            Add-Result $Editor 'off' $Run 'startup' ([ordered]@{
                    first_window_s = $startups[0].WindowSeconds; first_ready_s = $startups[0].ReadySeconds
                    warm_window_s = [math]::Round((($startups[1].WindowSeconds + $startups[2].WindowSeconds) / 2), 3)
                    warm_ready_s = [math]::Round((($startups[1].ReadySeconds + $startups[2].ReadySeconds) / 2), 3)
                })
        }
        if ($Scenarios -contains 'a') {
            $settle = Wait-Quiet $tree $SettleMaxSeconds
            Save-Shot $startups[2].Hwnd "$Editor-run$Run-a"
            Measure-State $tree $Editor 'off' $Run 'a' $IdleCpuSeconds @{ settle_s = $settle }
        }
    }
    finally { Stop-Tree $tree }
}

# Scenarios b-g in one session with the project open.
function Invoke-ProjectSession([string]$Editor, [string]$LsMode, [int]$Run) {
    $ls = $LsMode -eq 'on'
    $dataDir = Get-RunDir $Editor $LsMode $Run 'project'
    Reset-Project
    Initialize-DataDir $Editor $dataDir $ls
    Set-Mode 'idle'
    $launch = Get-EditorLaunch $Editor $dataDir $ls $ProjectDir
    $idle = if ($ls) { $ShortCpuSeconds } else { $IdleCpuSeconds }
    $settleMax = if ($ls) { $LsSettleMaxSeconds } else { $SettleMaxSeconds }
    $tree = $null
    Wait-SystemQuiet
    try {
        $tree = Start-Editor $launch
        $ready = Wait-Ready $tree
        $hwnd = $ready.Hwnd
        Write-Trace ('  project window ready in {0} s' -f $ready.ReadySeconds)

        # b. The project open, no files.
        $settle = Wait-Quiet $tree $settleMax
        Save-Shot $hwnd "$Editor-ls$LsMode-run$Run-b"
        if ($Scenarios -contains 'b') { Measure-State $tree $Editor $LsMode $Run 'b' $idle @{ settle_s = $settle } }

        # c. Open 20 files; in each, add a line at the top, save, and scroll.
        if ($Scenarios -contains 'c') {
            $before = Get-CpuTimes $tree
            foreach ($file in $EditFiles) {
                Open-File $Editor $hwnd $file
                Send-Text $hwnd 'perf edit '
                Send-Key $hwnd 'Escape'
                Send-Key $hwnd 'F14' -DelayMs 300
                Send-Key $hwnd 'PageDown' -Count 15 -DelayMs 60
                Send-Key $hwnd 'PageUp' -Count 15 -DelayMs 60
            }
            $busy = Get-CpuPercent $before (Get-CpuTimes $tree)
            Save-Shot $hwnd "$Editor-ls$LsMode-run$Run-c"
            $edited = @($EditFiles | Where-Object { (Get-Content -Raw (Join-Path $ProjectDir $_)) -match 'perf edit ' }).Count
            $settle = Wait-Quiet $tree $settleMax
            Measure-State $tree $Editor $LsMode $Run 'c' $ShortCpuSeconds ([ordered]@{
                    settle_s = $settle; files_saved_with_edit = $edited; busy_total_cpu_pct = $busy['total_cpu_pct']
                })
        }

        # d. Project-wide search, three queries.
        if ($Scenarios -contains 'd') {
            $times = @()
            foreach ($key in 'F15', 'F16', 'F17') {
                Send-Key $hwnd $key -DelayMs 800
                Send-Key $hwnd 'Enter' -DelayMs 200
                $times += Wait-Quiet $tree 120 3 2
            }
            Save-Shot $hwnd "$Editor-ls$LsMode-run$Run-d"
            Measure-State $tree $Editor $LsMode $Run 'd' $ShortCpuSeconds ([ordered]@{ search_busy_s = (($times | Measure-Object -Sum).Sum) })
        }

        # e. Open a 200k-line log and search it.
        if ($Scenarios -contains 'e') {
            # Into the project only now, so the searches of d don't see it;
            # the file finders then open it like any other file.
            Copy-Item $LogFile (Join-Path $ProjectDir 'perf-200k.log')
            Start-Sleep -Seconds 3
            Send-Key $hwnd 'Escape' -Count 2
            Open-File $Editor $hwnd 'perf-200k.log'
            [void](Wait-Quiet $tree 60 3 2)
            Send-Key $hwnd 'F18' -DelayMs 600
            Send-Text $hwnd $LogQuery
            Send-Key $hwnd 'Enter' -Count 3 -DelayMs 300
            $busy = Wait-Quiet $tree 60 3 2
            Save-Shot $hwnd "$Editor-ls$LsMode-run$Run-e"
            Send-Key $hwnd 'Escape'
            Measure-State $tree $Editor $LsMode $Run 'e' $ShortCpuSeconds ([ordered]@{ search_busy_s = $busy })
        }

        # f. Terminals printing like coding agents, then idle.
        if ($Scenarios -contains 'f') {
            # Exactly $Terminals terminals in each editor: end the shells of
            # terminals already open (Solo opens one with the project).
            Stop-Shells $tree
            Set-Mode 'active'
            for ($i = 0; $i -lt $Terminals; $i++) {
                Send-Key $hwnd 'F19' -DelayMs 3000
                Send-Text $hwnd (Get-AgentCommand)
                Send-Key $hwnd 'Enter' -DelayMs 1000
            }
            Start-Sleep -Seconds 5
            Update-Tree $tree
            $shells = @($tree.Pids.Keys | Where-Object {
                    $tree.Procs[$_].Name -eq 'powershell.exe' -and (Get-CommandLine $tree $_) -notmatch '-NonInteractive' }).Count
            Save-Shot $hwnd "$Editor-ls$LsMode-run$Run-f-busy"
            if ($ThreadProfile -and $Editor -ne 'vscode') { Save-ThreadProfile $tree "$Editor-ls$LsMode-run$Run-f-busy" 20 }
            Measure-State $tree $Editor $LsMode $Run 'f-busy' $BusySeconds ([ordered]@{ shells = $shells })
            Set-Mode 'idle'
            Start-Sleep -Seconds 10
            [void](Wait-Quiet $tree 60)
            Save-Shot $hwnd "$Editor-ls$LsMode-run$Run-f-idle"
            if ($ThreadProfile -and $Editor -ne 'vscode') { Save-ThreadProfile $tree "$Editor-ls$LsMode-run$Run-f-idle" 60 }
            Measure-State $tree $Editor $LsMode $Run 'f-idle' $idle ([ordered]@{ shells = $shells })
        }

        # g. Close all files and terminals.
        if ($Scenarios -contains 'g') {
            # Stopping the terminals' shells ends the terminals (as typing
            # exit would); then close what is left.
            Stop-Shells $tree
            Send-Key $hwnd 'F21' -DelayMs 800
            Send-Key $hwnd 'F20' -DelayMs 800
            Send-Key $hwnd 'Escape'
            $settle = Wait-Quiet $tree $SettleMaxSeconds
            Save-Shot $hwnd "$Editor-ls$LsMode-run$Run-g"
            Measure-State $tree $Editor $LsMode $Run 'g' $ShortCpuSeconds ([ordered]@{ settle_s = $settle })
        }
    }
    finally {
        Set-Mode 'idle'
        Stop-Tree $tree
    }
}

# Which threads of the main process use the CPU over a window.
function Save-ThreadProfile($Tree, [string]$Name, [int]$Seconds) {
    $root = $Tree.Root
    $before = @{}
    foreach ($t in [PerfNative]::Threads($root)) { $before[$t.Tid] = $t }
    Start-Sleep -Seconds $Seconds
    $rows = foreach ($t in [PerfNative]::Threads($root)) {
        $was = if ($before.ContainsKey($t.Tid)) { $before[$t.Tid].Cpu } else { 0 }
        [pscustomobject]@{ tid = $t.Tid; name = $t.Name; start = $t.Start; cpu_pct = [math]::Round(100.0 * ($t.Cpu - $was) / 1e7 / $Seconds, 3) }
    }
    $rows = $rows | Sort-Object cpu_pct -Descending
    $rows | Export-Csv -NoTypeInformation -Path (Join-Path $ResultDir "threads-$Name.csv")
    Write-Trace ("  busiest threads of the main process: " + (($rows | Select-Object -First 5 | ForEach-Object { "$($_.name)@$($_.start)=$($_.cpu_pct)%" }) -join ', '))
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

if ($PrepareLsSeed) {
    if (-not $LsSeedRoot) { throw '-PrepareLsSeed needs -LsSeedRoot' }
    Reset-Project
    foreach ($editor in @($Editors | Where-Object { $_ -ne 'vscode' })) {
        $dataDir = Join-Path $LsSeedRoot $editor
        if (Test-Path $dataDir) { Remove-Item -Recurse -Force $dataDir }
        New-Item -ItemType Directory -Force $dataDir | Out-Null
        Write-ZedConfig $dataDir $true
        Write-Trace "downloading $editor's language servers into $dataDir"
        $tree = Start-Editor (Get-EditorLaunch $editor $dataDir $true $ProjectDir)
        try {
            $hwnd = (Wait-Ready $tree).Hwnd
            foreach ($file in $EditFiles) { if ($file -notlike '*.rs') { Open-File $editor $hwnd $file } }
            $json = Join-Path $dataDir 'languages/json-language-server/node_modules/vscode-langservers-extracted/bin/vscode-json-language-server'
            $deadline = (Get-Date).AddMinutes(30)
            while (-not (Test-Path $json) -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 5 }
            [void](Wait-Quiet $tree 600 3 30)
        }
        finally { Stop-Tree $tree }
    }
    Write-Trace 'done'
    return
}
if ($SummarizeFrom) {
    foreach ($dir in $SummarizeFrom) {
        foreach ($row in (Get-Content -Raw (Join-Path $dir 'results-raw.json') | ConvertFrom-Json)) { [void]$Results.Add($row) }
    }
    $Runs = 0
}
else { New-TestData }
$editorInfo = [ordered]@{}
foreach ($editor in $Editors) {
    $exe = switch ($editor) { 'solo' { $SoloExe } 'zed' { $ZedExe } 'vscode' { $CodeExe } }
    if (-not $SummarizeFrom -and (-not $exe -or -not (Test-Path $exe))) { throw "$editor isn't installed (looked for $exe)" }
    $editorInfo[$editor] = [ordered]@{ exe = $exe; version = (Get-FileVersionText $exe) }
}
$lsModes = switch ($LanguageServers) { 'off' { @('off') } 'on' { @('on') } 'both' { @('off', 'on') } }

$os = Get-CimInstance Win32_OperatingSystem
$cpuInfo = Get-CimInstance Win32_Processor | Select-Object -First 1
$machine = [ordered]@{
    cpu = $cpuInfo.Name.Trim(); cores = $cpuInfo.NumberOfCores; logical_processors = $Cores
    memory_gb = [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 1)
    os = "$($os.Caption) $($os.Version)"; gpu = ((Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name) -join '; ')
}
[ordered]@{ machine = $machine; editors = $editorInfo; started = (Get-Date).ToString('s'); parameters = $PSBoundParameters } |
    ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $ResultDir 'environment.json') -Encoding UTF8
Write-Trace "results: $ResultDir"
foreach ($e in $editorInfo.Keys) { Write-Trace "$e $($editorInfo[$e].version) $($editorInfo[$e].exe)" }

for ($run = $FirstRun; $run -lt $FirstRun + $Runs; $run++) {
    foreach ($editor in $Editors) {
        if ($Scenarios -contains 'startup' -or $Scenarios -contains 'a') {
            Write-Trace "run $run, $editor, empty window"
            try { Invoke-EmptySession $editor $run } catch { Write-Trace "  failed: $_" }
        }
        if (@($Scenarios | Where-Object { $_ -in 'b', 'c', 'd', 'e', 'f', 'g' }).Count -gt 0) {
            foreach ($lsMode in $lsModes) {
                Write-Trace "run $run, $editor, project, language servers $lsMode"
                try { Invoke-ProjectSession $editor $lsMode $run } catch { Write-Trace "  failed: $_ $($_.ScriptStackTrace)" }
            }
        }
    }
}

# Medians over the runs.
function Get-Median([double[]]$Values) {
    $sorted = @($Values | Sort-Object)
    $n = $sorted.Count
    if ($n -eq 0) { return $null }
    if ($n % 2) { $sorted[($n - 1) / 2] } else { ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2 }
}

$summary = foreach ($group in ($Results | Group-Object editor, language_servers, scenario)) {
    $first = $group.Group[0]
    $row = [ordered]@{ editor = $first.editor; language_servers = $first.language_servers; scenario = $first.scenario; runs = $group.Count }
    foreach ($prop in $first.PSObject.Properties.Name) {
        if ($prop -in 'editor', 'language_servers', 'run', 'scenario') { continue }
        $values = @($group.Group | ForEach-Object { $_.$prop } | Where-Object { $_ -is [ValueType] } | ForEach-Object { [double]$_ })
        if ($values.Count) { $row[$prop] = [math]::Round((Get-Median $values), 2) }
    }
    [pscustomobject]$row
}
$summary | Export-Csv -NoTypeInformation -Path (Join-Path $ResultDir 'summary.csv')
$summary | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $ResultDir 'summary.json') -Encoding UTF8
$Results | Export-Csv -NoTypeInformation -Path (Join-Path $ResultDir 'results-raw.csv')

$summary | Where-Object { $_.scenario -ne 'startup' } | Sort-Object scenario, language_servers, editor |
    Format-Table scenario, language_servers, editor, runs, total_private_mb, total_ws_mb, main_private_mb, terminals_private_mb, ls_private_mb, other_private_mb, total_cpu_pct -AutoSize | Out-String -Width 220 | Write-Host
$summary | Where-Object { $_.scenario -eq 'startup' } |
    Format-Table editor, runs, first_window_s, first_ready_s, warm_window_s, warm_ready_s -AutoSize | Out-String | Write-Host
Write-Trace "done: $ResultDir"
