// SPDX-License-Identifier: GPL-3.0-or-later
// Bounded native messages only to an explicitly owned hidden Files child.
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
public static class FilesDockFixture {
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd,out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool IsChild(IntPtr parent,IntPtr child);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern IntPtr GetDlgItem(IntPtr hwnd,int id);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd,StringBuilder name,int count);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint msg,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint msg,IntPtr w,string l,uint flags,uint timeout,out IntPtr result);
    static void Validate(Process owner,long hostValue,long panelValue) {
        if(owner==null||owner.HasExited)throw new InvalidOperationException("Owned host exited");
        var host=new IntPtr(hostValue);var panel=new IntPtr(panelValue);
        foreach(var hwnd in new[]{host,panel}) {uint pid;GetWindowThreadProcessId(hwnd,out pid);if(hwnd==IntPtr.Zero||pid!=owner.Id||IsWindowVisible(hwnd)||GetForegroundWindow()==hwnd)throw new InvalidOperationException("Refusing unowned/visible Files HWND");}
        var name=new StringBuilder(128);GetClassName(panel,name,name.Capacity);
        if(name.ToString()!="flowmux.windows.files"||!IsChild(host,panel))throw new InvalidOperationException("Files hierarchy changed");
    }
    public static void Command(Process owner,long host,long panel,int id) {
        Validate(owner,host,panel);
        if(id!=7&&id!=8&&id!=9&&id!=13&&id!=14&&id!=15)throw new ArgumentOutOfRangeException("Owned form command");
        var button=GetDlgItem(new IntPtr(panel),id);if(button==IntPtr.Zero)throw new InvalidOperationException("Missing Files button");
        IntPtr result;if(SendMessageTimeout(new IntPtr(panel),0x111,new IntPtr(id),button,2,1000,out result)==IntPtr.Zero)throw new Win32Exception("Files form command exceeded one second");
        Validate(owner,host,panel);
    }
    public static void Destination(Process owner,long host,long panel,string text) {
        Validate(owner,host,panel);if(text==null||text.Length>16384)throw new ArgumentOutOfRangeException("Owned destination");
        var edit=GetDlgItem(new IntPtr(panel),12);if(edit==IntPtr.Zero)throw new InvalidOperationException("Missing native destination EDIT");
        IntPtr result;if(SendMessageTimeout(edit,0xC,IntPtr.Zero,text,2,1000,out result)==IntPtr.Zero)throw new Win32Exception("Owned destination update exceeded one second");
    }
}
