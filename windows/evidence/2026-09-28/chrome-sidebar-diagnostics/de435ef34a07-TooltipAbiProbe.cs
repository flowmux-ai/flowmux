// Ignored diagnostic only. Creates owned hidden native HWNDs; never shows or focuses them.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class TooltipAbiProbe {
    [StructLayout(LayoutKind.Sequential)] struct Rect { public int left,top,right,bottom; }
    [StructLayout(LayoutKind.Sequential)] struct Init { public uint size,classes; }
    [StructLayout(LayoutKind.Sequential)] struct Tool {
        public uint size,flags; public IntPtr window,id; public Rect rect;
        public IntPtr instance,text,lParam,reserved;
    }
    public sealed class Result {
        public int pointerBytes,structBytes,reservedOffset,lParamOffset,cbSize;
        public bool init,addMessageReturned,getMessageReturned;
        public long addResult,getResult;
        public int addError,getError;
        public string text;
    }
    [DllImport("comctl32.dll")] static extern bool InitCommonControlsEx(ref Init data);
    [DllImport("kernel32.dll",CharSet=CharSet.Unicode)] static extern IntPtr GetModuleHandle(string name);
    [DllImport("kernel32.dll")] static extern void SetLastError(uint error);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr CreateWindowEx(uint ex,string cls,string name,uint style,int x,int y,int w,int h,IntPtr parent,IntPtr menu,IntPtr instance,IntPtr param);
    [DllImport("user32.dll",SetLastError=true)] static extern bool DestroyWindow(IntPtr window);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr SendMessageTimeout(IntPtr window,uint message,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
    public static Result[] Run() {
        if(IntPtr.Size!=8) throw new InvalidOperationException("This diagnostic requires 64-bit PowerShell.");
        var init=new Init {size=(uint)Marshal.SizeOf(typeof(Init)),classes=4};
        bool initialized=InitCommonControlsEx(ref init);
        var module=GetModuleHandle(null);IntPtr root=IntPtr.Zero,button=IntPtr.Zero;
        var results=new List<Result>();
        int size=Marshal.SizeOf(typeof(Tool)),reserved=Marshal.OffsetOf(typeof(Tool),"reserved").ToInt32(),param=Marshal.OffsetOf(typeof(Tool),"lParam").ToInt32();
        try {
            root=CreateWindowEx(0x08000000,"STATIC","owned hidden tooltip ABI",0x80000000,0,0,1,1,IntPtr.Zero,IntPtr.Zero,module,IntPtr.Zero);
            if(root==IntPtr.Zero)throw new Win32Exception(Marshal.GetLastWin32Error());
            button=CreateWindowEx(0,"BUTTON","Settings",0x40000000,0,0,20,20,root,IntPtr.Zero,module,IntPtr.Zero);
            if(button==IntPtr.Zero)throw new Win32Exception(Marshal.GetLastWin32Error());
            if(IsWindowVisible(root)||IsWindowVisible(button))throw new InvalidOperationException("Owned probe must remain hidden.");
            foreach(int cb in new[]{size,reserved,param}) {
                IntPtr tooltip=IntPtr.Zero,retained=IntPtr.Zero,native=IntPtr.Zero,buffer=IntPtr.Zero;
                try {
                    tooltip=CreateWindowEx(0x08000000,"tooltips_class32",null,0x80000003,0,0,0,0,root,IntPtr.Zero,module,IntPtr.Zero);
                    if(tooltip==IntPtr.Zero)throw new Win32Exception(Marshal.GetLastWin32Error());
                    if(IsWindowVisible(tooltip))throw new InvalidOperationException("Owned tooltip became visible.");
                    retained=Marshal.StringToHGlobalUni("Settings 한글");
                    var tool=new Tool {size=(uint)cb,flags=0x11,window=root,id=button,text=retained};
                    native=Marshal.AllocHGlobal(size);Marshal.StructureToPtr(tool,native,false);
                    IntPtr add;SetLastError(0);
                    var addReturned=SendMessageTimeout(tooltip,1074,IntPtr.Zero,native,2,1000,out add);int addError=Marshal.GetLastWin32Error();
                    buffer=Marshal.AllocHGlobal(2048);for(int n=0;n<2048;n++)Marshal.WriteByte(buffer,n,0);
                    tool.text=buffer;Marshal.StructureToPtr(tool,native,false);
                    IntPtr get;SetLastError(0);
                    var getReturned=SendMessageTimeout(tooltip,1080,new IntPtr(1024),native,2,1000,out get);int getError=Marshal.GetLastWin32Error();
                    results.Add(new Result {pointerBytes=IntPtr.Size,structBytes=size,reservedOffset=reserved,lParamOffset=param,cbSize=cb,init=initialized,addMessageReturned=addReturned!=IntPtr.Zero,addResult=add.ToInt64(),addError=addError,getMessageReturned=getReturned!=IntPtr.Zero,getResult=get.ToInt64(),getError=getError,text=Marshal.PtrToStringUni(buffer)});
                } finally {
                    if(tooltip!=IntPtr.Zero)DestroyWindow(tooltip);
                    if(native!=IntPtr.Zero)Marshal.FreeHGlobal(native);
                    if(buffer!=IntPtr.Zero)Marshal.FreeHGlobal(buffer);
                    if(retained!=IntPtr.Zero)Marshal.FreeHGlobal(retained);
                }
            }
        } finally {
            if(button!=IntPtr.Zero)DestroyWindow(button);
            if(root!=IntPtr.Zero)DestroyWindow(root);
        }
        return results.ToArray();
    }
}
