// SPDX-License-Identifier: GPL-3.0-or-later
// Own the entire hidden check tree before allowing its first instruction to run.
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;

public sealed class CheckJob : IDisposable {
    [StructLayout(LayoutKind.Sequential)] struct Limits {
        public long PerProcess, PerJob;
        public uint Flags;
        public UIntPtr Minimum, Maximum;
        public uint ActiveLimit;
        public UIntPtr Affinity;
        public uint Priority, Scheduling;
    }
    [StructLayout(LayoutKind.Sequential)] struct ExtendedLimits {
        public Limits Basic;
        public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes;
        public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
    }
    [StructLayout(LayoutKind.Sequential)] struct Accounting {
        public long User, Kernel, PeriodUser, PeriodKernel;
        public uint Faults, Total, Active, Terminated;
    }
    [StructLayout(LayoutKind.Sequential)] struct Security {
        public int Size; public IntPtr Descriptor; public int Inherit;
    }
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct Startup {
        public int Size; public string Reserved, Desktop, Title;
        public uint X, Y, Width, Height, CharsX, CharsY, Fill, Flags;
        public short Show, ReservedSize;
        public IntPtr ReservedBytes, Input, Output, Error;
    }
    [StructLayout(LayoutKind.Sequential)] struct ProcessInfo {
        public IntPtr Process, Thread; public int Id, ThreadId;
    }
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr CreateJobObject(IntPtr security, string name);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetInformationJobObject(IntPtr job, int kind, ref ExtendedLimits limits, int size);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool QueryInformationJobObject(IntPtr job, int kind, out Accounting accounting, int size, IntPtr returned);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool TerminateJobObject(IntPtr job, uint code);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr process, out uint code);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr CreateFile(string path, uint access, uint share, ref Security security, uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateProcess(string app, StringBuilder command, IntPtr processSecurity, IntPtr threadSecurity, bool inherit, uint flags, IntPtr environment, string cwd, ref Startup startup, out ProcessInfo process);
    IntPtr job, process;
    public int Id { get; private set; }
    static void Check(bool success) { if (!success) throw new Win32Exception(Marshal.GetLastWin32Error()); }
    static IntPtr Open(string path, uint access, uint creation) {
        var security = new Security { Size = Marshal.SizeOf(typeof(Security)), Inherit = 1 };
        var handle = CreateFile(path, access, 3, ref security, creation, 0x80, IntPtr.Zero);
        if (handle == new IntPtr(-1)) throw new Win32Exception(Marshal.GetLastWin32Error());
        return handle;
    }
    public CheckJob(string executable, string[] args, string cwd, string stdout, string stderr) {
        IntPtr input = IntPtr.Zero, output = IntPtr.Zero, error = IntPtr.Zero, thread = IntPtr.Zero;
        try {
            job = CreateJobObject(IntPtr.Zero, null);
            Check(job != IntPtr.Zero);
            var limits = new ExtendedLimits();
            limits.Basic.Flags = 0x2000; // KILL_ON_JOB_CLOSE, no breakaway.
            Check(SetInformationJobObject(job, 9, ref limits, Marshal.SizeOf(typeof(ExtendedLimits))));
            input = Open("NUL", 0x80000000, 3);
            output = Open(stdout, 0x40000000, 1); // CREATE_NEW: logs never overwrite another run.
            error = Open(stderr, 0x40000000, 1);
            var startup = new Startup { Size = Marshal.SizeOf(typeof(Startup)), Flags = 0x101,
                Show = 0, Input = input, Output = output, Error = error };
            var command = new StringBuilder(CliProbe.Quote(executable));
            foreach (string arg in args) command.Append(' ').Append(CliProbe.Quote(arg));
            ProcessInfo info;
            Check(CreateProcess(executable, command, IntPtr.Zero, IntPtr.Zero, true,
                0x08000004, IntPtr.Zero, cwd, ref startup, out info)); // NO_WINDOW | SUSPENDED
            process = info.Process; thread = info.Thread; Id = info.Id;
            Check(AssignProcessToJobObject(job, process));
            Check(ResumeThread(thread) != 0xffffffff);
        } catch {
            if (process != IntPtr.Zero) TerminateProcess(process, 125);
            Dispose(); throw;
        } finally {
            foreach (var handle in new[] { input, output, error, thread })
                if (handle != IntPtr.Zero) CloseHandle(handle);
        }
    }
    public bool Wait(int milliseconds) {
        uint result = WaitForSingleObject(process, (uint)milliseconds);
        if (result == 0xffffffff) throw new Win32Exception(Marshal.GetLastWin32Error());
        return result == 0;
    }
    public uint ExitCode { get { uint code; Check(GetExitCodeProcess(process, out code)); return code; } }
    public uint Active {
        get { Accounting data; Check(QueryInformationJobObject(job, 1, out data, Marshal.SizeOf(typeof(Accounting)), IntPtr.Zero)); return data.Active; }
    }
    public void Stop(uint code) { Check(TerminateJobObject(job, code)); }
    public void Dispose() {
        if (job != IntPtr.Zero) { CloseHandle(job); job = IntPtr.Zero; }
        if (process != IntPtr.Zero) { CloseHandle(process); process = IntPtr.Zero; }
    }
}
