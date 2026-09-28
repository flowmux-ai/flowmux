// SPDX-License-Identifier: GPL-3.0-or-later
// Exact owned fixture bytes and optional sharing locks. No input, focus or clipboard APIs.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

public sealed class EditorFixture : IDisposable {
    public readonly string Root;
    private readonly string stateRoot;
    private readonly List<FileStream> locks = new List<FileStream>();
    public static readonly string Unicode = "한글 한 é 😀 \"quoted\" \\ path & <tag>";
    public static readonly string Original = Unicode + "\nsecond line\n";
    public static readonly string Edited = "edited " + Unicode + "\nlast line\n";
    public static readonly string External = "external " + Unicode + "\nexternal second\n";

    public EditorFixture(string directory) {
        Root = Path.GetFullPath(Path.Combine(directory, "files 한글 한 😀"));
        stateRoot = Path.GetFullPath(Path.Combine(directory, "state"));
        Directory.CreateDirectory(Root);
    }
    public string File(string name) {
        string path = Path.GetFullPath(Path.Combine(Root, name));
        if (!path.StartsWith(Root + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            throw new ArgumentException("Fixture path escaped its owned root");
        return path;
    }
    public static byte[] Encode(string text, bool bom, bool crlf) {
        if (text.Contains("\r")) throw new ArgumentException("Fixture logical text must use LF only");
        byte[] content = new UTF8Encoding(false, true).GetBytes(crlf ? text.Replace("\n", "\r\n") : text);
        if (!bom) return content;
        byte[] result = new byte[content.Length + 3];
        result[0] = 0xef;result[1] = 0xbb;result[2] = 0xbf;
        Buffer.BlockCopy(content, 0, result, 3, content.Length);
        return result;
    }
    public string Write(string name, string text, bool bom, bool crlf) {
        return WriteBytes(name, Encode(text, bom, crlf));
    }
    public string WriteBytes(string name, byte[] bytes) {
        string path = File(name);
        Directory.CreateDirectory(Path.GetDirectoryName(path));
        System.IO.File.WriteAllBytes(path, bytes);
        return path;
    }
    public bool BytesEqual(string path, byte[] expected) {
        string full = Path.GetFullPath(path);
        if (!full.StartsWith(Root + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            throw new ArgumentException("Fixture read escaped its owned root");
        byte[] actual = System.IO.File.ReadAllBytes(full);
        if (actual.Length != expected.Length) return false;
        for (int index = 0; index < actual.Length; index++) if (actual[index] != expected[index]) return false;
        return true;
    }
    public void LockAgainstReplacement(string name) {
        locks.Add(new FileStream(File(name), FileMode.Open, FileAccess.Read, FileShare.Read));
    }
    public void LockAgainstRead(string name) {
        locks.Add(new FileStream(File(name), FileMode.Open, FileAccess.Read, FileShare.None));
    }
    public void DeleteOwned(string name) {
        System.IO.File.Delete(File(name));
    }
    public long LastWriteTicks(string name) {
        string path = File(name);
        if (!System.IO.File.Exists(path)) throw new FileNotFoundException("Owned fixture is missing", path);
        return System.IO.File.GetLastWriteTimeUtc(path).Ticks;
    }
    public long RewritePreservingTimestamp(string name, string text, bool bom, bool crlf) {
        string path = File(name);
        byte[] replacement = Encode(text, bom, crlf);
        var original = new FileInfo(path);
        if (!original.Exists || original.Length != replacement.Length)
            throw new InvalidOperationException("Timestamp fixture rewrite requires equal byte lengths");
        DateTime timestamp = original.LastWriteTimeUtc;
        System.IO.File.WriteAllBytes(path, replacement);
        System.IO.File.SetLastWriteTimeUtc(path, timestamp);
        if (new FileInfo(path).Length != replacement.Length || System.IO.File.GetLastWriteTimeUtc(path).Ticks != timestamp.Ticks)
            throw new InvalidOperationException("Owned rewrite did not preserve its length and exact UTC timestamp");
        return timestamp.Ticks;
    }
    public void LockStateAgainstReplacement(string path) {
        string full = Path.GetFullPath(path);
        if (!full.StartsWith(stateRoot + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            throw new ArgumentException("Checkpoint lock escaped its owned state directory");
        locks.Add(new FileStream(full, FileMode.Open, FileAccess.Read, FileShare.Read));
    }
    public void RenameFile(string source, string destination) {
        System.IO.File.Move(File(source), File(destination));
    }
    public void RenameDirectory(string source, string destination) {
        Directory.Move(File(source), File(destination));
    }
    public void ReleaseLocks() {
        foreach (FileStream stream in locks) stream.Dispose();
        locks.Clear();
    }
    public void Dispose() { ReleaseLocks(); }
}

// Read-only snapshot of visible top-level windows belonging to this owned host.
// Other applications' window titles, process details and contents are not read.
public static class EditorWindowProbe {
    private delegate bool EnumWindowsCallback(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll", SetLastError=true)] static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", SetLastError=true)] static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint GetProcessId(IntPtr process);

    public static long[] VisibleTopLevelWindows(Process owned) {
        if (owned == null || owned.HasExited) throw new InvalidOperationException("Owned host is not running");
        uint processId = GetProcessId(owned.Handle);
        if (processId == 0 || processId != (uint)owned.Id) throw new InvalidOperationException("Owned process handle identity differs");
        var windows = new List<long>();
        EnumWindowsCallback callback = delegate(IntPtr window, IntPtr parameter) {
            uint owner;
            if (GetWindowThreadProcessId(window, out owner) != 0 && owner == processId && IsWindowVisible(window))
                windows.Add(window.ToInt64());
            return true;
        };
        if (!EnumWindows(callback, IntPtr.Zero)) throw new Win32Exception(Marshal.GetLastWin32Error());
        GC.KeepAlive(callback);
        if (owned.HasExited || GetProcessId(owned.Handle) != processId) throw new InvalidOperationException("Owned host exited during window snapshot");
        windows.Sort();
        return windows.ToArray();
    }
}

// Only the verified UI thread of an already owned hidden host may be paused.
// IPC worker threads keep running; this never suspends a process or sends input.
public sealed class EditorUiThreadPause : IDisposable {
    [DllImport("user32.dll", SetLastError=true)] static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern int GetClassName(IntPtr window, StringBuilder name, int count);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint GetProcessId(IntPtr process);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint GetProcessIdOfThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenThread(uint access, bool inherit, uint threadId);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint SuspendThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    private IntPtr thread;
    private bool suspended;
    public uint ProcessId { get; private set; }
    public uint ThreadId { get; private set; }
    public uint PreviousSuspendCount { get; private set; }
    public uint PreviousResumeCount { get; private set; }
    public bool Resumed { get; private set; }

    public EditorUiThreadPause(Process owned, long windowHandle) {
        if (owned == null || owned.HasExited) throw new InvalidOperationException("Owned host is not running");
        ProcessId = GetProcessId(owned.Handle);
        if (ProcessId == 0 || ProcessId != (uint)owned.Id) throw new InvalidOperationException("Owned process handle identity differs");
        IntPtr window = new IntPtr(windowHandle);
        uint windowProcess;
        ThreadId = GetWindowThreadProcessId(window, out windowProcess);
        var name = new StringBuilder(256);
        if (window == IntPtr.Zero || ThreadId == 0 || windowProcess != ProcessId ||
            IsWindowVisible(window) || GetForegroundWindow() == window ||
            GetClassName(window, name, name.Capacity) == 0 || name.ToString() != "flowmux.windows.native")
            throw new InvalidOperationException("Refusing to pause a window that is not the owned hidden flowmux host");
        thread = OpenThread(0x0002 | 0x0800, false, ThreadId); // SUSPEND_RESUME | QUERY_LIMITED_INFORMATION
        if (thread == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
        try {
            uint checkedProcess;
            if (owned.HasExited || GetProcessIdOfThread(thread) != ProcessId ||
                GetWindowThreadProcessId(window, out checkedProcess) != ThreadId || checkedProcess != ProcessId ||
                IsWindowVisible(window) || GetForegroundWindow() == window)
                throw new InvalidOperationException("Owned HWND/thread identity changed before suspension");
            PreviousSuspendCount = SuspendThread(thread);
            if (PreviousSuspendCount == UInt32.MaxValue) throw new Win32Exception(Marshal.GetLastWin32Error());
            suspended = true;
            if (PreviousSuspendCount != 0) throw new InvalidOperationException("Owned UI thread was already suspended; refusing nested suspension");
        } catch {
            Dispose();
            throw;
        }
    }

    public void Dispose() {
        if (thread == IntPtr.Zero) return;
        if (suspended) {
            PreviousResumeCount = ResumeThread(thread);
            if (PreviousResumeCount == UInt32.MaxValue) throw new Win32Exception(Marshal.GetLastWin32Error());
            suspended = false;
            Resumed = true;
        }
        CloseHandle(thread);thread = IntPtr.Zero;
        GC.SuppressFinalize(this);
    }
    ~EditorUiThreadPause() {
        if (thread == IntPtr.Zero) return;
        if (suspended) ResumeThread(thread);
        CloseHandle(thread);
    }
}
