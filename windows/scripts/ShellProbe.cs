// SPDX-License-Identifier: GPL-3.0-or-later
// Compiled only by the hidden shell verifier; launched inside its owned ConPTY.
using System;
using System.IO;
using System.Text;
using System.Collections.Generic;
public static class ShellProbe {
    public static int Main(string[] args) {
        var fields = new List<string>();
        fields.Add(Environment.CurrentDirectory);
        fields.Add(Environment.GetEnvironmentVariable("FLOWMUX_SURFACE_ID") ?? "");
        for (int i = 1; i < args.Length; i++) fields.Add(args[i]);
        for (int i = 0; i < fields.Count; i++) fields[i] = Convert.ToBase64String(Encoding.UTF8.GetBytes(fields[i]));
        File.WriteAllLines(args[0], fields, new UTF8Encoding(false));
        Console.OutputEncoding = new UTF8Encoding(false);
        Console.WriteLine("SHELL_PROBE_한글");
        return 17;
    }
}
