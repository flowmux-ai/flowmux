// SPDX-License-Identifier: GPL-3.0-or-later
using System;
using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Threading;
public static class ProcessTreeProbe {
    public static int Main(string[] args) {
        if (args.Length != 2) return 2;
        int depth = int.Parse(args[1]);
        File.AppendAllText(args[0], Process.GetCurrentProcess().Id + Environment.NewLine);
        if (depth > 0) {
            var info = new ProcessStartInfo(Assembly.GetExecutingAssembly().Location,
                "\"" + args[0] + "\" " + (depth - 1));
            info.UseShellExecute = false;
            info.CreateNoWindow = true;
            using (var child = Process.Start(info)) { }
        }
        Thread.Sleep(120000);
        return 0;
    }
}
