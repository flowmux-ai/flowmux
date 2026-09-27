// SPDX-License-Identifier: GPL-3.0-or-later
// An owned ConPTY child. Mode control uses a private file, never desktop input.
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public static class PasteProbe {
    [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int id);
    [DllImport("kernel32.dll")] static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    [DllImport("kernel32.dll")] static extern bool SetConsoleMode(IntPtr handle, uint mode);
    [DllImport("kernel32.dll")] static extern bool SetConsoleCP(uint codepage);
    [DllImport("kernel32.dll")] static extern bool SetConsoleOutputCP(uint codepage);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool ReadConsoleW(IntPtr handle, [Out] char[] buffer, uint length, out uint read, IntPtr control);
    static volatile bool stopping;
    public static int Main(string[] args) {
        if (args.Length != 2) return 2;
        var input = GetStdHandle(-10); var output = GetStdHandle(-11);
        uint previousInput, previousOutput;
        if (!GetConsoleMode(input, out previousInput) || !GetConsoleMode(output, out previousOutput)) return 3;
        SetConsoleCP(65001); SetConsoleOutputCP(65001);
        if (!SetConsoleMode(input, 0x0200) || !SetConsoleMode(output, previousOutput | 4)) return 4;
        Thread mode = null;
        try {
            Console.OutputEncoding = new UTF8Encoding(false);
            using (var file = new FileStream(args[0], FileMode.Create, FileAccess.Write, FileShare.ReadWrite)) {
                Console.Write("\x1b[?1004l\x1b[?2004lPASTE_PROBE_READY\r\n");
                mode = new Thread(() => {
                    string last = "";
                    while (!stopping) {
                        string value = "";
                        try { if (File.Exists(args[1])) value = File.ReadAllText(args[1]); } catch (IOException) {}
                        var parts = value.Split(':');
                        if (value != last && parts.Length == 2 && (parts[1] == "on" || parts[1] == "off")) {
                            Console.Write("\x1b[?2004" + (parts[1] == "on" ? "h" : "l") + "\r\nPASTE_MODE_" + parts[0] + "\r\n");
                            last = value;
                        }
                        Thread.Sleep(20);
                    }
                });
                mode.IsBackground = true; mode.Start();
                var chars = new char[4096]; var bytes = new byte[16384];
                var encoder = new UTF8Encoding(false, true).GetEncoder();
                while (true) {
                    uint read;
                    if (!ReadConsoleW(input, chars, (uint)chars.Length, out read, IntPtr.Zero))
                        throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                    if (read == 0) break;
                    int quit = Array.IndexOf(chars, (char)0x11, 0, (int)read);
                    int count = encoder.GetBytes(chars, 0, quit < 0 ? (int)read : quit, bytes, 0, quit >= 0);
                    file.Write(bytes, 0, count); file.Flush();
                    if (quit >= 0) break;
                }
            }
        } finally {
            stopping = true; if (mode != null) mode.Join(1000);
            Console.Write("\x1b[?2004l");
            SetConsoleMode(input, previousInput); SetConsoleMode(output, previousOutput);
        }
        return 0;
    }
}
