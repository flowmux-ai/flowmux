// SPDX-License-Identifier: GPL-3.0-or-later
// Private-file-driven terminal renderer; never reads desktop input/clipboard.
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public static class SelectionProbe {
    [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int id);
    [DllImport("kernel32.dll")] static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    [DllImport("kernel32.dll")] static extern bool SetConsoleMode(IntPtr handle, uint mode);
    public static int Main(string[] args) {
        if (args.Length != 1) return 2;
        var output = GetStdHandle(-11); uint previous;
        if (!GetConsoleMode(output, out previous) || !SetConsoleMode(output, previous | 4)) return 3;
        Console.OutputEncoding = new UTF8Encoding(false);
        Console.Write("\x1b[?1004l\x1b[2J\x1b[HSELECT_한글_한_😀_END\r\nSELECTION_PROBE_READY\r\n");
        string last = "";
        try {
            while (true) {
                string text = "";
                try { if (File.Exists(args[0])) text = File.ReadAllText(args[0]); } catch (IOException) {}
                var parts = text.Split(':');
                if (text != last && parts.Length == 2) {
                    switch (parts[1]) {
                        case "rewrite": Console.Write("\x1b[HREPLACED_다른_문자_END\x1b[K\r\n"); break;
                        case "clear": Console.Write("\x1b[2J\x1b[HCLEARED_SCREEN\r\n"); break;
                        case "alt_on": Console.Write("\x1b[?1049h\x1b[2J\x1b[HALTERNATE_한글_😀\r\n"); break;
                        case "alt_off": Console.Write("\x1b[?1049l\r\nBACK_TO_NORMAL\r\n"); break;
                        case "large":
                            for (int i = 0; i < 2500; i++) Console.Write("LONG_SELECTION_" + i + "_한글_" + new string('x', 70) + "\r\n");
                            break;
                        case "exit": Console.Write("SELECTION_EXITED\r\n"); return 7;
                        default: return 4;
                    }
                    Console.Write("SELECTION_CONTROL_" + parts[0] + "\r\n");
                    last = text;
                }
                Thread.Sleep(20);
            }
        } finally { SetConsoleMode(output, previous); }
    }
}
