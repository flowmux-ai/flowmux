// SPDX-License-Identifier: GPL-3.0-or-later
// Owned hidden-host lifetime probes. No input, focus, clipboard or production hooks.
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
using System.Threading.Tasks;
using Microsoft.Win32.SafeHandles;

public sealed class EditorOpenWorkerPause : IDisposable {
    private const string WorkerName = "flowmux-editor-open";
    [DllImport("user32.dll", SetLastError=true)] static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern int GetClassName(IntPtr window, StringBuilder name, int count);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint GetProcessId(IntPtr process);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint GetProcessIdOfThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint GetThreadId(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenThread(uint access, bool inherit, uint threadId);
    [DllImport("kernel32.dll")] static extern int GetThreadDescription(IntPtr thread, out IntPtr description);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr memory);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint SuspendThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);

    private IntPtr thread;
    private bool suspended;
    public uint ProcessId { get; private set; }
    public uint ThreadId { get; private set; }
    public string Description { get; private set; }
    public uint PreviousSuspendCount { get; private set; }
    public uint PreviousResumeCount { get; private set; }
    public bool Resumed { get; private set; }

    private static string ReadDescription(IntPtr handle) {
        IntPtr text = IntPtr.Zero;
        try {
            int result = GetThreadDescription(handle, out text);
            if (result < 0) Marshal.ThrowExceptionForHR(result);
            return text == IntPtr.Zero ? String.Empty : Marshal.PtrToStringUni(text);
        } finally {
            if (text != IntPtr.Zero) LocalFree(text);
        }
    }

    private static uint CheckHiddenHost(Process owned, IntPtr window, uint expectedProcess) {
        if (owned == null || owned.HasExited || GetProcessId(owned.Handle) != expectedProcess ||
            expectedProcess == 0 || expectedProcess != (uint)owned.Id)
            throw new InvalidOperationException("Owned process handle identity differs or host exited");
        uint windowProcess;
        uint uiThread = GetWindowThreadProcessId(window, out windowProcess);
        var name = new StringBuilder(256);
        if (window == IntPtr.Zero || uiThread == 0 || windowProcess != expectedProcess ||
            IsWindowVisible(window) || GetForegroundWindow() == window ||
            GetClassName(window, name, name.Capacity) == 0 || name.ToString() != "flowmux.windows.native")
            throw new InvalidOperationException("Refusing to pause a worker outside the owned hidden flowmux host");
        return uiThread;
    }

    public EditorOpenWorkerPause(Process owned, long hiddenHostHwnd) {
        if (owned == null || owned.HasExited) throw new InvalidOperationException("Owned host is not running");
        ProcessId = GetProcessId(owned.Handle);
        IntPtr window = new IntPtr(hiddenHostHwnd);
        uint uiThread = CheckHiddenHost(owned, window, ProcessId);
        try {
            owned.Refresh();
            foreach (ProcessThread candidate in owned.Threads) {
                IntPtr handle = IntPtr.Zero;
                try {
                    uint candidateId = (uint)candidate.Id;
                    if (candidateId == uiThread) continue;
                    // Merely opening a handle changes no thread state. Check its
                    // owning PID again to exclude an exited/reused thread ID.
                    handle = OpenThread(0x0002 | 0x0800, false, candidateId);
                    if (handle == IntPtr.Zero) continue;
                    if (GetProcessIdOfThread(handle) != ProcessId || GetThreadId(handle) != candidateId) continue;
                    string description = ReadDescription(handle);
                    if (!String.Equals(description, WorkerName, StringComparison.Ordinal)) continue;
                    if (thread != IntPtr.Zero) throw new InvalidOperationException("Owned host has more than one editor Open worker");
                    thread = handle;handle = IntPtr.Zero;
                    ThreadId = candidateId;Description = description;
                } finally {
                    if (handle != IntPtr.Zero) CloseHandle(handle);
                    candidate.Dispose();
                }
            }
            if (thread == IntPtr.Zero) throw new InvalidOperationException("Owned editor Open worker was not found; warm it before pausing");
            if (CheckHiddenHost(owned, window, ProcessId) != uiThread ||
                GetProcessIdOfThread(thread) != ProcessId || GetThreadId(thread) != ThreadId || ThreadId == uiThread ||
                !String.Equals(ReadDescription(thread), WorkerName, StringComparison.Ordinal))
                throw new InvalidOperationException("Owned worker identity changed before suspension");
            PreviousSuspendCount = SuspendThread(thread);
            if (PreviousSuspendCount == UInt32.MaxValue) throw new Win32Exception(Marshal.GetLastWin32Error());
            suspended = true;
            if (PreviousSuspendCount != 0) throw new InvalidOperationException("Owned Open worker was already suspended; refusing nested suspension");
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
    ~EditorOpenWorkerPause() {
        if (thread == IntPtr.Zero) return;
        if (suspended) ResumeThread(thread);
        CloseHandle(thread);
    }
}

// One request per connection, as required by the real native IPC server.
// The connection stays open after Request returns, so the caller can hold an
// acknowledged quit peer during the server's bounded two-second close wait.
public sealed class EditorOwnedPipe : IDisposable {
    private const int MaxFrameBytes = 1024 * 1024;
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint GetProcessId(IntPtr process);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetNamedPipeServerProcessId(SafePipeHandle pipe, out uint processId);
    private NamedPipeClientStream pipe;
    private bool used;
    public uint ProcessId { get; private set; }

    private static void CheckBudget(int milliseconds) {
        if (milliseconds < 1 || milliseconds > 5000)
            throw new ArgumentOutOfRangeException("milliseconds", "Owned raw IPC budget must be 1..5000ms");
    }

    public EditorOwnedPipe(Process owned, string fullPipeName, int timeoutMs) {
        CheckBudget(timeoutMs);
        if (owned == null || owned.HasExited) throw new InvalidOperationException("Owned host is not running");
        ProcessId = GetProcessId(owned.Handle);
        if (ProcessId == 0 || ProcessId != (uint)owned.Id) throw new InvalidOperationException("Owned process handle identity differs");
        string prefix = "\\\\.\\pipe\\flowmux-" + ProcessId + "-";
        Guid nonce;
        if (fullPipeName == null || !fullPipeName.StartsWith(prefix, StringComparison.Ordinal) ||
            !Guid.TryParseExact(fullPipeName.Substring(prefix.Length), "D", out nonce))
            throw new ArgumentException("Raw IPC pipe does not have the exact owned host PID and nonce shape");
        pipe = new NamedPipeClientStream(".", fullPipeName.Substring(9), PipeDirection.InOut,
            PipeOptions.Asynchronous, TokenImpersonationLevel.Identification);
        try {
            pipe.Connect(timeoutMs);
            uint serverProcess;
            if (!GetNamedPipeServerProcessId(pipe.SafePipeHandle, out serverProcess))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            if (owned.HasExited || GetProcessId(owned.Handle) != ProcessId || serverProcess != ProcessId)
                throw new InvalidOperationException("Connected named-pipe server is not the exact owned host");
        } catch {
            Dispose();
            throw;
        }
    }

    private void WaitWithin(Task operation, Stopwatch elapsed, int milliseconds, string step) {
        long remaining = milliseconds - elapsed.ElapsedMilliseconds;
        if (remaining <= 0 || !operation.Wait((int)remaining)) {
            Dispose(); // Cancel only this owned connection's pending I/O.
            throw new TimeoutException("Owned raw IPC " + step + " exceeded its total request deadline; not retried");
        }
    }

    public string Request(string oneLineJson, int timeoutMs) {
        CheckBudget(timeoutMs);
        if (pipe == null) throw new ObjectDisposedException("EditorOwnedPipe");
        if (used) throw new InvalidOperationException("Native IPC allows exactly one request per connection");
        if (String.IsNullOrEmpty(oneLineJson) || oneLineJson.IndexOf('\n') >= 0 || oneLineJson.IndexOf('\r') >= 0)
            throw new ArgumentException("Raw IPC request must be one nonempty JSON line");
        byte[] frame = new UTF8Encoding(false, true).GetBytes(oneLineJson + "\n");
        if (frame.Length > MaxFrameBytes) throw new ArgumentException("Raw IPC request exceeds one MiB");
        used = true;
        var elapsed = Stopwatch.StartNew();
        try {
            Task write = pipe.WriteAsync(frame, 0, frame.Length);
            WaitWithin(write, elapsed, timeoutMs, "write");
            var buffer = new byte[4096];
            using (var reply = new MemoryStream()) {
                while (true) {
                    Task<int> read = pipe.ReadAsync(buffer, 0, buffer.Length);
                    WaitWithin(read, elapsed, timeoutMs, "read");
                    // WaitWithin established completion; this does not introduce
                    // a second or unbounded wait on the asynchronous operation.
                    int count = read.GetAwaiter().GetResult();
                    if (count == 0) throw new EndOfStreamException("Owned IPC server closed before its newline reply");
                    int end = Array.IndexOf(buffer, (byte)'\n', 0, count);
                    int append = end >= 0 ? end : count;
                    if (reply.Length + append > MaxFrameBytes) throw new IOException("Owned IPC reply exceeds one MiB");
                    reply.Write(buffer, 0, append);
                    if (end >= 0) return new UTF8Encoding(false, true).GetString(reply.ToArray());
                }
            }
        } catch {
            Dispose();
            throw;
        }
    }

    public void Dispose() {
        if (pipe == null) return;
        pipe.Dispose();pipe = null;
    }
}
