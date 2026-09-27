// SPDX-License-Identifier: GPL-3.0-or-later
// Native console-side byte evidence. Ctrl+Q terminates the probe.
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
public static class InputProbe {
    [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int id);
    [DllImport("kernel32.dll")] static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    [DllImport("kernel32.dll")] static extern bool SetConsoleMode(IntPtr handle, uint mode);
    [DllImport("kernel32.dll")] static extern bool SetConsoleCP(uint codepage);
    [DllImport("kernel32.dll")] static extern bool SetConsoleOutputCP(uint codepage);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool ReadConsoleW(IntPtr handle, [Out] char[] buffer, uint length, out uint read, IntPtr control);
    public static int Main(string[] args) {
        if (args.Length != 1) return 2;
        IntPtr input = GetStdHandle(-10);
        uint previous;
        if (!GetConsoleMode(input, out previous)) return 3;
        SetConsoleCP(65001); SetConsoleOutputCP(65001);
        if (!SetConsoleMode(input, 0x0200)) return 4;
        try {
            using (var file = new FileStream(args[0], FileMode.Create, FileAccess.Write, FileShare.ReadWrite)) {
                Console.InputEncoding = new UTF8Encoding(false);
                Console.OutputEncoding = new UTF8Encoding(false);
                Console.Write("\x1b[?1004l\x1b[?25lINPUT_PROBE_READY\r\n");
                // Read Unicode at the console boundary, independent of the legacy .NET
                // console stream's code-page cache, then record canonical UTF-8 bytes.
                var chars = new char[4096];
                var bytes = new byte[16384];
                var encoder = new UTF8Encoding(false, true).GetEncoder();
                while (true) {
                    uint read;
                    if (!ReadConsoleW(input, chars, (uint)chars.Length, out read, IntPtr.Zero))
                        throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                    if (read == 0) break;
                    int quit = Array.IndexOf(chars, (char)0x11, 0, (int)read);
                    int count = encoder.GetBytes(chars, 0, quit < 0 ? (int)read : quit, bytes, 0, false);
                    file.Write(bytes, 0, count); file.Flush();
                    if (quit >= 0) break;
                }
            }
        } finally { SetConsoleMode(input, previous); Console.Write("\x1b[?25h"); }
        return 0;
    }
}
