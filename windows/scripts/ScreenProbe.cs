// SPDX-License-Identifier: GPL-3.0-or-later
// Output-only renderer controlled by an owned private file, never desktop input.
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public static class ScreenProbe {
    [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int id);
    [DllImport("kernel32.dll")] static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    [DllImport("kernel32.dll")] static extern bool SetConsoleMode(IntPtr handle, uint mode);
    public static int Main(string[] args) {
        if (args.Length != 1) return 2;
        var output = GetStdHandle(-11); uint previous;
        if (!GetConsoleMode(output, out previous) || !SetConsoleMode(output, previous | 4)) return 3;
        Console.OutputEncoding = new UTF8Encoding(false);
        Console.Write("\x1b[?1004l\x1b[2J\x1b[HINITIAL_한글_한_é_😀\r\n\x1b[32mGREEN_한글\x1b[0m\r\nSCREEN_PROBE_READY\r\n");
        string last = "";
        try {
            while (true) {
                string text = "";
                try { if (File.Exists(args[0])) text = File.ReadAllText(args[0]); } catch (IOException) {}
                var parts = text.Split(':');
                if (text != last && parts.Length == 2) {
                    bool top = false;
                    switch (parts[1]) {
                        case "rewrite": Console.Write("\x1b[HREPLACED_다른\x1b[K\r\nOBSOLETE_LONG_TEXT\rCURRENT_한글\x1b[K\r\n"); break;
                        case "history":
                            for (int i = 0; i < 400; i++) Console.Write("HISTORY_" + i.ToString("D4") + "_한글\r\n");
                            break;
                        case "wrap": Console.Write(new string('x', Console.WindowWidth - 2) + "한글_WRAP_한_😀\r\n"); break;
                        case "cursor_top": top = true; break;
                        case "alt_on":
                            Console.Write("\x1b[?1049h\x1b[2J\x1b[HALT_TOP_한글\x1b[" + Console.WindowHeight + ";1HFOOTER_한글_😀\x1b[2;1H");
                            top = true; break;
                        case "alt_off": Console.Write("\x1b[?1049l\r\nBACK_TO_NORMAL\r\n"); break;
                        case "exit": Console.Write("SCREEN_EXITED\r\n"); return 7;
                        default: return 4;
                    }
                    Console.Write("SCREEN_CONTROL_" + parts[0] + "\r\n");
                    if (top) Console.Write("\x1b[H");
                    last = text;
                }
                Thread.Sleep(20);
            }
        } finally { SetConsoleMode(output, previous); }
    }
}
