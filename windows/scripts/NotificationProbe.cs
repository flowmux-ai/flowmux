// SPDX-License-Identifier: GPL-3.0-or-later
// Owned output-only ConPTY probe; ST termination avoids audible BEL output.
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public static class NotificationProbe {
    [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int id);
    [DllImport("kernel32.dll")] static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    [DllImport("kernel32.dll")] static extern bool SetConsoleMode(IntPtr handle, uint mode);
    public static int Main(string[] args) {
        if (args.Length != 1) return 2;
        var output=GetStdHandle(-11); uint previous;
        if (!GetConsoleMode(output,out previous) || !SetConsoleMode(output,previous|4)) return 3;
        Console.OutputEncoding=new UTF8Encoding(false);
        Console.Write("NOTIFICATION_PROBE_READY\r\n");
        string last="";
        try {
            while (true) {
                string text="";
                try {
                    using (var file=new FileStream(args[0],FileMode.Open,FileAccess.Read,FileShare.ReadWrite|FileShare.Delete))
                    using (var reader=new StreamReader(file,Encoding.UTF8)) text=reader.ReadToEnd();
                } catch (IOException) {}
                var parts=text.Split(':');
                if (text!=last && parts.Length==2) {
                    switch(parts[1]) {
                        case "osc":
                            Console.Write("\x1b]9;OSC9_한글_한_😀\x1b\\");
                            Console.Write("\x1b]99;;needs approval_한글\x1b\\");
                            Console.Write("\x1b]777;notify;Build_한글;error_한_😀\x1b\\");
                            var cwd=Path.Combine(Path.GetDirectoryName(args[0]),"cwd_한글");
                            Directory.CreateDirectory(cwd);
                            Console.Write("\x1b]9;9;"+cwd+"\x1b\\\x1b]9;4;1;75\x1b\\");
                            break;
                        case "burst":
                            var burst=new StringBuilder();
                            for(int i=0;i<1000;i++) burst.Append("\x1b]9;done\x1b\\");
                            burst.Append("\x1b]9;error after burst\x1b\\");
                            Console.Write(burst.ToString());break;
                        case "exit": Console.Write("NOTIFICATION_EXITED\r\n"); return 7;
                        default: return 4;
                    }
                    Console.Write("NOTIFICATION_CONTROL_"+parts[0]+"\r\n");last=text;
                }
                Thread.Sleep(20);
            }
        } finally {SetConsoleMode(output,previous);}
    }
}
