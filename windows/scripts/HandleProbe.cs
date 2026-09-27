// SPDX-License-Identifier: GPL-3.0-or-later
// Counts handle types for the test host only; never reads other process objects.
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class HandleProbe {
    [StructLayout(LayoutKind.Sequential)] struct Entry {
        public IntPtr Object; public UIntPtr Process, Handle;
        public uint Access; public ushort Trace, Type;
        public uint Attributes, Reserved;
    }
    [StructLayout(LayoutKind.Sequential)] struct UnicodeString {
        public ushort Length, MaximumLength; public IntPtr Buffer;
    }
    [DllImport("ntdll.dll")] static extern int NtQuerySystemInformation(int kind, IntPtr buffer, int size, out int needed);
    [DllImport("ntdll.dll")] static extern int NtQueryObject(IntPtr handle, int kind, IntPtr buffer, int size, out int needed);
    [DllImport("kernel32.dll")] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")] static extern bool DuplicateHandle(IntPtr source, IntPtr handle, IntPtr target, out IntPtr copy, uint access, bool inherit, uint options);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] static extern uint GetProcessId(IntPtr handle);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder name, ref uint size);
    public static Dictionary<string, int> Types(uint pid) {
        int size = 1024 * 1024;
        IntPtr buffer = IntPtr.Zero, process = IntPtr.Zero;
        try {
            while (true) {
                buffer = Marshal.AllocHGlobal(size);
                int needed, status = NtQuerySystemInformation(64, buffer, size, out needed);
                if (status == 0) break;
                Marshal.FreeHGlobal(buffer); buffer = IntPtr.Zero;
                if (status != unchecked((int)0xC0000004) || size > 128 * 1024 * 1024) throw new Exception("Handle enumeration failed: " + status);
                size = Math.Max(size * 2, needed + 4096);
            }
            process = OpenProcess(0x40, false, pid);
            if (process == IntPtr.Zero) throw new Exception("Cannot inspect the test process");
            long count = Marshal.ReadInt64(buffer);
            int stride = Marshal.SizeOf(typeof(Entry));
            var names = new Dictionary<ushort, string>();
            var result = new Dictionary<string, int>();
            for (long index = 0; index < count; index++) {
                var entry = (Entry)Marshal.PtrToStructure(IntPtr.Add(buffer, checked(16 + (int)index * stride)), typeof(Entry));
                if (entry.Process.ToUInt64() != pid) continue;
                string name;
                if (!names.TryGetValue(entry.Type, out name)) {
                    name = "Type" + entry.Type;
                    IntPtr copy;
                    if (DuplicateHandle(process, new IntPtr((long)entry.Handle.ToUInt64()), GetCurrentProcess(), out copy, 0, false, 2)) {
                        IntPtr info = Marshal.AllocHGlobal(4096);
                        try {
                            int needed;
                            if (NtQueryObject(copy, 2, info, 4096, out needed) == 0) {
                                var text = (UnicodeString)Marshal.PtrToStructure(info, typeof(UnicodeString));
                                name = Marshal.PtrToStringUni(text.Buffer, text.Length / 2);
                            }
                        } finally { Marshal.FreeHGlobal(info); CloseHandle(copy); }
                    }
                    names[entry.Type] = name;
                }
                int previous; result.TryGetValue(name, out previous); result[name] = previous + 1;
                if (name == "Process") {
                    IntPtr copy;
                    if (DuplicateHandle(process, new IntPtr((long)entry.Handle.ToUInt64()), GetCurrentProcess(), out copy, 0, false, 2)) {
                        try {
                            uint length = 32768; var path = new StringBuilder((int)length);
                            string image = QueryFullProcessImageName(copy, 0, path, ref length) ? System.IO.Path.GetFileName(path.ToString()) : "unknown";
                            string key = "ProcessImage:" + image;
                            result.TryGetValue(key, out previous); result[key] = previous + 1;
                            if (image == "unknown") {
                                string detail = "UnknownProcessAccess:" + entry.Access.ToString("X") + ":pid=" + GetProcessId(copy);
                                result.TryGetValue(detail, out previous); result[detail] = previous + 1;
                            }
                        } finally { CloseHandle(copy); }
                    }
                }
            }
            return result;
        } finally {
            if (buffer != IntPtr.Zero) Marshal.FreeHGlobal(buffer);
            if (process != IntPtr.Zero) CloseHandle(process);
        }
    }
}
