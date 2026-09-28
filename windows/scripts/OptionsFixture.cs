// SPDX-License-Identifier: GPL-3.0-or-later
// Only direct messages to exact owned hidden HWNDs; no desktop input or focus.
// Optional composition messages test app guards, not OS IME/TSF behavior.
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class OptionsFixture {
    [StructLayout(LayoutKind.Sequential)] public struct Rect {public int Left,Top,Right,Bottom;}
    public sealed class Window {public long Handle,Owner,Style;public string Title;public int Width,Height;public uint Dpi;public bool Enabled,OwnerEnabled;}
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd,out uint pid);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool IsWindowEnabled(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern IntPtr GetParent(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr hwnd,uint command);
    [DllImport("user32.dll")] private static extern int GetDlgCtrlID(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr hwnd,out Rect rect);
    [DllImport("user32.dll")] private static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll",EntryPoint="GetWindowLongPtrW")] private static extern IntPtr GetWindowLongPtr(IntPtr hwnd,int index);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint message,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint message,IntPtr w,string l,uint flags,uint timeout,out IntPtr result);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint message,IntPtr w,StringBuilder l,uint flags,uint timeout,out IntPtr result);
    private static IntPtr Owned(long handle,int owner) {var hwnd=new IntPtr(handle);uint pid;GetWindowThreadProcessId(hwnd,out pid);if(pid!=owner||IsWindowVisible(hwnd)||GetForegroundWindow()==hwnd)throw new InvalidOperationException("Expected exact owned hidden HWND");return hwnd;}
    private static IntPtr Child(long parent,long child,int owner) {var root=Owned(parent,owner);var hwnd=Owned(child,owner);if(GetParent(hwnd)!=root)throw new InvalidOperationException("Control belongs to another parent");return hwnd;}
    private static IntPtr Message(IntPtr hwnd,uint message,IntPtr w,IntPtr l) {IntPtr result;if(SendMessageTimeout(hwnd,message,w,l,2,1000,out result)==IntPtr.Zero)throw new TimeoutException("Owned UI message exceeded one second");return result;}
    public static string Text(long handle,int owner) {var hwnd=Owned(handle,owner);var text=new StringBuilder(2048);IntPtr result;if(SendMessageTimeout(hwnd,0xD,new IntPtr(text.Capacity),text,2,1000,out result)==IntPtr.Zero)throw new TimeoutException("Owned text read failed");return text.ToString();}
    public static Window Describe(long handle,int owner) {var hwnd=Owned(handle,owner);Rect bounds;if(!GetWindowRect(hwnd,out bounds))throw new InvalidOperationException("Cannot inspect owned window");var parent=GetWindow(hwnd,4);return new Window{Handle=handle,Owner=parent.ToInt64(),Style=GetWindowLongPtr(hwnd,-16).ToInt64(),Title=Text(handle,owner),Width=bounds.Right-bounds.Left,Height=bounds.Bottom-bounds.Top,Dpi=GetDpiForWindow(hwnd),Enabled=IsWindowEnabled(hwnd),OwnerEnabled=parent==IntPtr.Zero||IsWindowEnabled(parent)};}
    public static void Click(long parent,long child,int owner) {var hwnd=Child(parent,child,owner);if(!IsWindowEnabled(hwnd))throw new InvalidOperationException("Owned action is disabled");Message(new IntPtr(parent),0x111,new IntPtr(GetDlgCtrlID(hwnd)),hwnd);Owned(parent,owner);}
    public static void SetText(long parent,long child,int owner,string value) {var hwnd=Child(parent,child,owner);if(!IsWindowEnabled(hwnd))throw new InvalidOperationException("Owned input is disabled");IntPtr result;if(SendMessageTimeout(hwnd,0xC,IntPtr.Zero,value,2,1000,out result)==IntPtr.Zero||result==IntPtr.Zero)throw new InvalidOperationException("Owned text edit failed");Owned(parent,owner);}
    // WM_SETTEXT does not emit EN_CHANGE for multiline EDIT. Notify its exact
    // owned parent explicitly when testing the same draft-change handler.
    public static void SetTextAndNotify(long parent,long child,int owner,string value) {SetText(parent,child,owner,value);var hwnd=Child(parent,child,owner);Message(new IntPtr(parent),0x111,new IntPtr((0x300<<16)|GetDlgCtrlID(hwnd)),hwnd);Owned(parent,owner);}
    public static void Select(long parent,long child,int owner,int index) {var hwnd=Child(parent,child,owner);if(!IsWindowEnabled(hwnd))throw new InvalidOperationException("Owned combo is disabled");if(Message(hwnd,0x14E,new IntPtr(index),IntPtr.Zero).ToInt64()!=index)throw new InvalidOperationException("Owned combo selection failed");Message(new IntPtr(parent),0x111,new IntPtr((1<<16)|GetDlgCtrlID(hwnd)),hwnd);Owned(parent,owner);}
    public sealed class Bounds {public long Parent;public int X,Y,Width,Height;}
    [DllImport("user32.dll")] private static extern int MapWindowPoints(IntPtr from,IntPtr to,ref Rect rect,uint count);
    public static long Parent(long handle,int owner) {return GetParent(Owned(handle,owner)).ToInt64();}
    public static Bounds RelativeBounds(long parent,long child,int owner) {var hwnd=Child(parent,child,owner);Rect bounds;if(!GetWindowRect(hwnd,out bounds))throw new InvalidOperationException("Cannot read owned control bounds");MapWindowPoints(IntPtr.Zero,new IntPtr(parent),ref bounds,2);return new Bounds{Parent=parent,X=bounds.Left,Y=bounds.Top,Width=bounds.Right-bounds.Left,Height=bounds.Bottom-bounds.Top};}
    // Exercises only the app's message guard, not OS IME/TSF composition behavior.
    public static void CompositionGuard(long parent,long child,int owner,bool active) {var hwnd=Child(parent,child,owner);Message(hwnd,active?0x10DU:0x10EU,IntPtr.Zero,IntPtr.Zero);Owned(parent,owner);}
    public static void Scroll(long window,int owner,bool end) {Message(Owned(window,owner),0x115,new IntPtr(end?7:6),IntPtr.Zero);}
}
