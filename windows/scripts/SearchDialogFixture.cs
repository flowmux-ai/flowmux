// SPDX-License-Identifier: GPL-3.0-or-later
// Owned HWND diagnostics/messages only. No SendInput, focus changes or clipboard.
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class SearchDialogFixture {
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
    [DllImport("user32.dll")] static extern IntPtr GetParent(IntPtr window);
    [DllImport("user32.dll")] static extern int GetDlgCtrlID(IntPtr window);
    [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr window);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr window,uint command);
    [DllImport("user32.dll",EntryPoint="GetWindowLongPtrW")] public static extern IntPtr WindowLong(IntPtr window,int index);
    [DllImport("user32.dll",EntryPoint="SendMessageTimeoutW",CharSet=CharSet.Unicode)] static extern IntPtr Native(IntPtr window,uint message,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
    [DllImport("user32.dll",EntryPoint="SendMessageTimeoutW",CharSet=CharSet.Unicode)] static extern IntPtr Read(IntPtr window,uint message,IntPtr w,StringBuilder text,uint flags,uint timeout,out IntPtr result);
    [DllImport("user32.dll",EntryPoint="SendMessageTimeoutW",CharSet=CharSet.Unicode)] static extern IntPtr Write(IntPtr window,uint message,IntPtr w,string text,uint flags,uint timeout,out IntPtr result);
    static void Owned(IntPtr window,int expectedPid) {uint pid;GetWindowThreadProcessId(window,out pid);if(window==IntPtr.Zero||pid!=(uint)expectedPid)throw new Exception("Refusing non-owned search HWND");}
    static IntPtr Send(IntPtr window,uint message,IntPtr w,IntPtr l,int pid) {Owned(window,pid);IntPtr result;if(Native(window,message,w,l,2,1000,out result)==IntPtr.Zero)throw new Exception("Owned search message timed out");return result;}
    public static string Text(IntPtr window,int pid) {Owned(window,pid);var text=new StringBuilder(16384);IntPtr result;if(Read(window,0x000D,(IntPtr)text.Capacity,text,2,1000,out result)==IntPtr.Zero)throw new Exception("Owned caption read timed out");return text.ToString();}
    public static void SetText(IntPtr window,string text,int pid) {Owned(window,pid);IntPtr result;if(Write(window,0x000C,IntPtr.Zero,text,2,1000,out result)==IntPtr.Zero)throw new Exception("Owned query update timed out");}
    public static void Click(IntPtr control,int pid) {Owned(control,pid);Send(GetParent(control),0x0111,(IntPtr)GetDlgCtrlID(control),control,pid);}
    public static void ActivateRow(IntPtr list,int index,int pid) {Send(list,0x0186,(IntPtr)index,IntPtr.Zero,pid);Send(GetParent(list),0x0111,(IntPtr)((2<<16)|GetDlgCtrlID(list)),list,pid);}
    public static string RowText(IntPtr list,int index,int pid) {int size=Send(list,0x018A,(IntPtr)index,IntPtr.Zero,pid).ToInt32();if(size<0||size>8192)throw new Exception("Invalid native result label length");var text=new StringBuilder(size+1);IntPtr result;if(Read(list,0x0189,(IntPtr)index,text,2,1000,out result)==IntPtr.Zero)throw new Exception("Native result label read timed out");return text.ToString();}
    public static void Close(IntPtr window,int pid) {Send(window,0x0010,IntPtr.Zero,IntPtr.Zero,pid);}
}
public static class SearchDialogProbe {
    public static void Main(string[] args) {
        Console.OutputEncoding=new UTF8Encoding(false);
        for(int i=0;i<705;i++)Console.WriteLine("SEARCH_DIALOG_한글_한_😀 row_"+i+" e\u0301");
        Console.WriteLine("SEARCH_DIALOG_PROBE_READY");
        while(Console.ReadLine()!=null) {}
    }
}
