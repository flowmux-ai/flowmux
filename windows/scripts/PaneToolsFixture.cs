// SPDX-License-Identifier: GPL-3.0-or-later
// Production WM_COMMAND only, sent to an explicitly owned hidden native button.
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
public static class PaneToolsFixture {
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetParent(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern int GetDlgCtrlID(IntPtr window);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr window,StringBuilder name,int count);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr SendMessageTimeout(IntPtr window,uint message,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
    public static void Command(Process owner,long hostValue,long buttonValue) {
        if(owner==null||owner.HasExited)throw new InvalidOperationException("Owned host exited");
        var host=new IntPtr(hostValue);var button=new IntPtr(buttonValue);
        foreach(var window in new[]{host,button}) {uint pid;GetWindowThreadProcessId(window,out pid);if(window==IntPtr.Zero||pid!=owner.Id||IsWindowVisible(window)||GetForegroundWindow()==window)throw new InvalidOperationException("Refusing unowned/visible pane control");}
        var name=new StringBuilder(64);GetClassName(button,name,name.Capacity);
        if(GetParent(button)!=host||!name.ToString().Equals("Button",StringComparison.OrdinalIgnoreCase))throw new InvalidOperationException("Expected native host BUTTON");
        int id=GetDlgCtrlID(button);if(id<=0||id>65535)throw new InvalidOperationException("Unexpected control ID");
        IntPtr result;if(SendMessageTimeout(host,0x111,new IntPtr(id),button,2,1000,out result)==IntPtr.Zero)throw new Win32Exception("Pane command exceeded one second");
    }
}
