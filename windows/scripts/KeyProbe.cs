// SPDX-License-Identifier: GPL-3.0-or-later
// An owned ConPTY child. Mode control uses a private file, never desktop input.
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public static class KeyProbe {
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
                Console.Write("\x1b[?1004l\x1b[?1lKEY_PROBE_READY\r\n");
                mode = new Thread(() => {
                    string last = "";
                    while (!stopping) {
                        string value = "";
                        try {
                            using(var control = new FileStream(args[1], FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete))
                            using(var reader = new StreamReader(control, Encoding.UTF8)) value = reader.ReadToEnd();
                        } catch (IOException) {}
                        var parts = value.Split(':');
                        if (value != last && parts.Length == 2 && (parts[1] == "on" || parts[1] == "off" || parts[1] == "exit")) {
                            if(parts[1] == "exit") { Console.Write("KEY_PROBE_EXITED\r\n"); Environment.Exit(7); }
                            Console.Write("\x1b[?1" + (parts[1] == "on" ? "h" : "l") + "\r\nKEY_MODE_" + parts[0] + "\r\n");
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
                    int count = encoder.GetBytes(chars, 0, (int)read, bytes, 0, false);
                    file.Write(bytes, 0, count); file.Flush();
                }
            }
        } finally {
            stopping = true; if (mode != null) mode.Join(1000);
            Console.Write("\x1b[?1l");
            SetConsoleMode(input, previousInput); SetConsoleMode(output, previousOutput);
        }
        return 0;
    }
}
