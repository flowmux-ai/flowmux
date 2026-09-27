// SPDX-License-Identifier: GPL-3.0-or-later
// Every process is hidden, with independent stdout/stderr pipes. No desktop input.
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
public static class CliProbe {
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    public static string Quote(string value) {
        var result = new StringBuilder("\""); int slashes = 0;
        foreach (char ch in value) {
            if (ch == '\\') { slashes++; continue; }
            result.Append('\\', ch == '"' ? slashes * 2 + 1 : slashes); slashes = 0;
            result.Append(ch);
        }
        result.Append('\\', slashes * 2); return result.Append('"').ToString();
    }
    public static Process Start(string file, string[] args, string cwd, string isolated) {
        var info = new ProcessStartInfo(file, String.Join(" ", Array.ConvertAll(args, Quote)));
        // CMD parses /C as command source, not MS C runtime argv. All callers
        // provide fixed test commands and explicitly quote their own paths.
        if (System.IO.Path.GetFileName(file).Equals("cmd.exe", StringComparison.OrdinalIgnoreCase)) {
            if (args.Length != 3 || args[0] != "/D" || args[1] != "/C") throw new ArgumentException("Unexpected CMD probe shape");
            info.Arguments = "/D /S /C \"" + args[2] + "\"";
        }
        info.UseShellExecute = false; info.CreateNoWindow = true;
        info.WindowStyle = ProcessWindowStyle.Hidden; info.WorkingDirectory = cwd;
        info.RedirectStandardOutput = true; info.RedirectStandardError = true;
        info.StandardOutputEncoding = new UTF8Encoding(false);
        info.StandardErrorEncoding = new UTF8Encoding(false);
        info.EnvironmentVariables["FLOWMUX_TEST_BACKGROUND"] = "1";
        info.EnvironmentVariables["FLOWMUX_TEST_CONFIG_DIR"] = System.IO.Path.Combine(isolated,"config");
        info.EnvironmentVariables["FLOWMUX_TEST_STATE_DIR"] = System.IO.Path.Combine(isolated,"state");
        info.EnvironmentVariables["PATH"] = System.IO.Path.GetDirectoryName(file) + ";" + Environment.GetEnvironmentVariable("PATH");
        return Process.Start(info);
    }
}
