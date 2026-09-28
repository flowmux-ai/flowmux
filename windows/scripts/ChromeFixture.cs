// SPDX-License-Identifier: GPL-3.0-or-later
// Inspect and render only native controls in an explicitly owned, hidden host.
// No desktop capture, focus, input injection, clipboard or WebView capture.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;
public static class ChromeFixture {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left,Top,Right,Bottom; }
    public sealed class Control {
        public long Handle; public string Class,Text; public int X,Y,Width,Height;
        public long Style,Font; public bool Shown,Enabled;
    }
    private delegate bool EnumProc(IntPtr hwnd,IntPtr data);
    [DllImport("user32.dll")] private static extern bool EnumChildWindows(IntPtr parent,EnumProc callback,IntPtr data);
    [DllImport("user32.dll")] private static extern IntPtr GetParent(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd,out uint pid);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool IsWindowEnabled(IntPtr hwnd);
    [DllImport("user32.dll",SetLastError=true)] private static extern bool SetWindowPos(IntPtr hwnd,IntPtr after,int x,int y,int width,int height,uint flags);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr hwnd,out Rect rect);
    [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr hwnd,out Rect rect);
    [DllImport("user32.dll")] private static extern int MapWindowPoints(IntPtr from,IntPtr to,ref Rect rect,uint count);
    [DllImport("user32.dll",EntryPoint="GetWindowLongPtrW")] private static extern IntPtr GetWindowLongPtr(IntPtr hwnd,int index);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] private static extern int GetClassName(IntPtr hwnd,StringBuilder text,int count);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint msg,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint msg,IntPtr w,StringBuilder l,uint flags,uint timeout,out IntPtr result);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    private static void Owned(IntPtr hwnd,int owner) {
        uint pid;GetWindowThreadProcessId(hwnd,out pid);
        if(pid!=owner || IsWindowVisible(hwnd) || GetForegroundWindow()==hwnd)
            throw new InvalidOperationException("Expected exact owned hidden HWND");
    }
    private static IntPtr Message(IntPtr hwnd,uint message,IntPtr w,IntPtr l) {
        IntPtr result;
        if(SendMessageTimeout(hwnd,message,w,l,2,1000,out result)==IntPtr.Zero)
            throw new Win32Exception("Owned native paint/read exceeded one second");
        return result;
    }
    public static Control[] Read(long root,int owner) {
        var parent=new IntPtr(root);Owned(parent,owner);var found=new List<Control>();
        EnumChildWindows(parent,delegate(IntPtr hwnd,IntPtr unused) {
            if(GetParent(hwnd)!=parent) return true;
            var cls=new StringBuilder(256);GetClassName(hwnd,cls,cls.Capacity);
            if(cls.ToString()!="Button" && cls.ToString()!="Static") return true;
            Owned(hwnd,owner);var text=new StringBuilder(4096);IntPtr result;
            if(SendMessageTimeout(hwnd,0x000D,new IntPtr(text.Capacity),text,2,1000,out result)==IntPtr.Zero)
                throw new InvalidOperationException("Cannot read owned native label");
            Rect bounds;if(!GetWindowRect(hwnd,out bounds))throw new Win32Exception();
            MapWindowPoints(IntPtr.Zero,parent,ref bounds,2);
            long style=GetWindowLongPtr(hwnd,-16).ToInt64();
            found.Add(new Control {Handle=hwnd.ToInt64(),Class=cls.ToString(),Text=text.ToString(),X=bounds.Left,Y=bounds.Top,
                Width=bounds.Right-bounds.Left,Height=bounds.Bottom-bounds.Top,Style=style,
                Font=Message(hwnd,0x31,IntPtr.Zero,IntPtr.Zero).ToInt64(),Shown=(style&0x10000000)!=0,Enabled=IsWindowEnabled(hwnd)});
            return true;
        },IntPtr.Zero);
        return found.ToArray();
    }
    public static int[] Size(long root,int owner) {
        var hwnd=new IntPtr(root);Owned(hwnd,owner);Rect r;
        if(!GetClientRect(hwnd,out r))throw new Win32Exception();return new[]{r.Right,r.Bottom};
    }
    public static string Pixel(string path,int x,int y) {
        using(var bitmap=new Bitmap(path)) {var c=bitmap.GetPixel(x,y);return String.Format("#{0:x2}{1:x2}{2:x2}",c.R,c.G,c.B);}
    }
    public static void Resize(long root,int owner,int width,int height) {
        var hwnd=new IntPtr(root);Owned(hwnd,owner);Rect client,outer;
        if(width<400 || height<300 || width>1600 || height>1200)throw new ArgumentOutOfRangeException("Owned test dimensions");
        if(!GetClientRect(hwnd,out client) || !GetWindowRect(hwnd,out outer))throw new Win32Exception();
        // NOMOVE | NOZORDER | NOACTIVATE, deliberately without SHOWWINDOW.
        if(!SetWindowPos(hwnd,IntPtr.Zero,0,0,width+outer.Right-outer.Left-client.Right,
            height+outer.Bottom-outer.Top-client.Bottom,0x16))throw new Win32Exception();
        Owned(hwnd,owner);
    }
    public static int ColorCount(string path,int x,int y,int width,int height,string color) {
        int count=0;var expected=ColorTranslator.FromHtml(color).ToArgb();
        using(var bitmap=new Bitmap(path)) {
            for(int row=y;row<y+height;row++)for(int column=x;column<x+width;column++)
                if(bitmap.GetPixel(column,row).ToArgb()==expected)count++;
        }
        return count;
    }
    public static void Png(string bitmapPath,string pngPath) {
        using(var bitmap=new Bitmap(bitmapPath)) bitmap.Save(pngPath,ImageFormat.Png);
    }
}
