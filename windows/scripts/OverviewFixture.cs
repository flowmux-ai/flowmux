// SPDX-License-Identifier: GPL-3.0-or-later
// Direct messages to exact owned hidden windows; never desktop focus/input.
using System;
using System.Runtime.InteropServices;
public static class OverviewFixture {
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd,out uint pid);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern IntPtr GetParent(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool IsChild(IntPtr parent,IntPtr child);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] private static extern bool PostMessage(IntPtr hwnd,uint msg,IntPtr w,IntPtr l);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint msg,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
    private static IntPtr Owned(long handle,int owner) {var hwnd=new IntPtr(handle);uint pid;GetWindowThreadProcessId(hwnd,out pid);if(pid!=owner||IsWindowVisible(hwnd)||GetForegroundWindow()==hwnd)throw new InvalidOperationException("Expected exact owned hidden HWND");return hwnd;}
    public static long Parent(long handle,int owner) {return GetParent(Owned(handle,owner)).ToInt64();}
    public static void Key(long root,long target,int owner,int key) {var parent=Owned(root,owner);var child=Owned(target,owner);if(child!=parent&&!IsChild(parent,child))throw new InvalidOperationException("Key target is outside the owned overlay");if(!PostMessage(child,0x100,new IntPtr(key),new IntPtr(1)))throw new InvalidOperationException("Owned key message failed");}
    public static void Scroll(long root,int owner,bool end) {var hwnd=Owned(root,owner);IntPtr result;if(SendMessageTimeout(hwnd,0x115,new IntPtr(end?7:6),IntPtr.Zero,2,1000,out result)==IntPtr.Zero)throw new TimeoutException("Owned scroll exceeded one second");}
}
