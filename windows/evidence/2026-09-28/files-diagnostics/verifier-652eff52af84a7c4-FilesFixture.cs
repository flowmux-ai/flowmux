// SPDX-License-Identifier: GPL-3.0-or-later
// Owned fixture filesystem and read-only native LISTBOX probes. No input/focus APIs.
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

public sealed class FilesFixture : IDisposable {
    public readonly string Root;
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateDirectoryW(string path, IntPtr security);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern SafeFileHandle CreateFileW(string path, uint access, uint share, IntPtr security, uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool DeleteFileW(string path);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool DeviceIoControl(SafeFileHandle handle, uint code, byte[] input, int inputSize, IntPtr output, int outputSize, out int returned, IntPtr overlapped);
    public FilesFixture(string directory) {
        Root=Path.GetFullPath(Path.Combine(directory,"files 한글 한 😀"));
        EnsureDirectory(Root);
    }
    static string Extended(string path) { return "\\\\?\\"+path.Replace('/','\\'); }
    public string File(string relative) {
        if(String.IsNullOrEmpty(relative) || relative.Length>12000 || relative.StartsWith("/") || relative.StartsWith("\\") || relative.Contains(":")) throw new ArgumentException("Expected bounded relative owned fixture path");
        string[] parts=relative.Replace('/','\\').Split('\\');
        foreach(string part in parts) if(String.IsNullOrEmpty(part) || part=="." || part==".." || part.EndsWith(".") || part.EndsWith(" ")) throw new ArgumentException("Invalid fixture component");
        return Root+"\\"+String.Join("\\",parts);
    }
    static void EnsureDirectory(string path) {
        string[] parts=path.Replace('/','\\').Split('\\');
        string current=parts[0]+"\\";
        for(int i=1;i<parts.Length;i++) {
            if(parts[i].Length==0) continue;
            current+=(current.EndsWith("\\")?"":"\\")+parts[i];
            if(!CreateDirectoryW(Extended(current),IntPtr.Zero)) {
                int error=Marshal.GetLastWin32Error(); if(error!=183) throw new Win32Exception(error,"Cannot create owned fixture directory");
            }
        }
    }
    public string Directory(string relative) { string path=File(relative);EnsureDirectory(path);return path; }
    public string LongRoot(string prefix) {
        string relative=prefix;
        for(int i=0;File(relative).Length<=300;i++) relative+="/긴경로_한_😀_"+i.ToString("D2")+"_abcdefghij";
        return Directory(relative);
    }
    public string Relative(string path) {
        if(!path.StartsWith(Root+"\\",StringComparison.Ordinal)) throw new ArgumentException("Path is not ordinally inside owned fixture");
        return path.Substring(Root.Length+1).Replace('\\','/');
    }
    public string Write(string relative,string text,bool bom,bool crlf) { return WriteBytes(relative,EditorFixture.Encode(text,bom,crlf)); }
    public string WriteBytes(string relative,byte[] bytes) {
        if(bytes.Length>2*1024*1024) throw new ArgumentException("Fixture write exceeds two MiB");
        string path=File(relative);EnsureDirectory(path.Substring(0,path.LastIndexOf('\\')));
        using(SafeFileHandle handle=CreateFileW(Extended(path),0x40000000,1,IntPtr.Zero,2,0x80,IntPtr.Zero)) {
            if(handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(),"Owned fixture write open failed");
            using(var stream=new FileStream(handle,FileAccess.Write)) stream.Write(bytes,0,bytes.Length);
        }
        return path;
    }
    public bool BytesEqual(string path,byte[] expected) {
        Relative(path);
        using(SafeFileHandle handle=CreateFileW(Extended(path),0x80000000,7,IntPtr.Zero,3,0x80,IntPtr.Zero)) {
            if(handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(),"Owned fixture read failed");
            using(var stream=new FileStream(handle,FileAccess.Read)) {
                if(stream.Length!=expected.Length) return false;
                for(int i=0;i<expected.Length;i++) if(stream.ReadByte()!=expected[i]) return false;
            }
        }
        return true;
    }
    public void DeleteOwned(string relative) {
        if(!DeleteFileW(Extended(File(relative)))) throw new Win32Exception(Marshal.GetLastWin32Error(),"Owned fixture deletion failed");
    }
    public void Populate(string relative,int count) {
        if(count<0 || count>1500) throw new ArgumentException("Fixture file count is bounded at1500");
        for(int i=0;i<count;i++) Write(relative+"/entry-"+i.ToString("D4")+" 한글.txt","row "+i+"\n",false,false);
    }
    // A mount-point reparse leaf points only at another directory in this fixture.
    // No symlink privilege, shell process, external target or recursive deletion.
    public string Junction(string relative,string targetRelative) {
        string link=Directory(relative),target=Directory(targetRelative);
        string substitute="\\??\\"+target,print=target;
        byte[] sub=Encoding.Unicode.GetBytes(substitute),display=Encoding.Unicode.GetBytes(print);
        byte[] data=new byte[16+sub.Length+2+display.Length+2];
        Buffer.BlockCopy(BitConverter.GetBytes(0xA0000003u),0,data,0,4);
        Buffer.BlockCopy(BitConverter.GetBytes((ushort)(data.Length-8)),0,data,4,2);
        Buffer.BlockCopy(BitConverter.GetBytes((ushort)sub.Length),0,data,10,2);
        Buffer.BlockCopy(BitConverter.GetBytes((ushort)(sub.Length+2)),0,data,12,2);
        Buffer.BlockCopy(BitConverter.GetBytes((ushort)display.Length),0,data,14,2);
        Buffer.BlockCopy(sub,0,data,16,sub.Length);Buffer.BlockCopy(display,0,data,18+sub.Length,display.Length);
        using(SafeFileHandle handle=CreateFileW(Extended(link),0x40000000,0,IntPtr.Zero,3,0x02200000,IntPtr.Zero)) {
            if(handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error(),"Owned junction open failed");
            int returned;if(!DeviceIoControl(handle,0x000900A4,data,data.Length,IntPtr.Zero,0,out returned,IntPtr.Zero)) throw new Win32Exception(Marshal.GetLastWin32Error(),"Owned junction creation failed");
        }
        return link;
    }
    public void ReleaseLocks() {}
    public void Dispose() {}
}

public sealed class FilesListSnapshot {
    public long Host,Panel,List;
    public uint ProcessId;
    public int Count,Caret,Top;
    public string[] Text;
    public int[] Selected;
    public long ElapsedMs;
}
public static class FilesListProbe {
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint GetProcessId(IntPtr handle);
    [DllImport("user32.dll", SetLastError=true)] static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern int GetClassNameW(IntPtr window,StringBuilder name,int capacity);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool IsChild(IntPtr parent,IntPtr child);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr window,uint flags);
    [DllImport("user32.dll", EntryPoint="GetWindowLongW")] static extern int GetWindowLong(IntPtr window,int index);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr SendMessageTimeoutW(IntPtr window,uint message,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true,EntryPoint="SendMessageTimeoutW")] static extern IntPtr SendText(IntPtr window,uint message,IntPtr w,StringBuilder l,uint flags,uint timeout,out IntPtr result);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true,EntryPoint="SendMessageTimeoutW")] static extern IntPtr SendIndices(IntPtr window,uint message,IntPtr w,[Out]int[] l,uint flags,uint timeout,out IntPtr result);
    static string Class(IntPtr window) { var name=new StringBuilder(256);if(GetClassNameW(window,name,name.Capacity)==0) throw new Win32Exception(Marshal.GetLastWin32Error());return name.ToString(); }
    static uint Remaining(Stopwatch clock) { long ms=4000-clock.ElapsedMilliseconds;if(ms<=0) throw new TimeoutException("Owned LISTBOX snapshot exceeded four seconds");return (uint)Math.Min(250,ms); }
    static long Query(IntPtr window,uint message,int parameter,Stopwatch clock) {
        IntPtr result;if(SendMessageTimeoutW(window,message,new IntPtr(parameter),IntPtr.Zero,3,Remaining(clock),out result)==IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(),"Owned LISTBOX query failed or timed out");return result.ToInt64();
    }
    static uint Validate(Process owned,IntPtr host,IntPtr panel,IntPtr list) {
        if(owned==null || owned.HasExited) throw new InvalidOperationException("Owned host is not running");
        uint pid=GetProcessId(owned.Handle);if(pid==0 || pid!=(uint)owned.Id) throw new InvalidOperationException("Owned process handle differs");
        foreach(IntPtr window in new IntPtr[]{host,panel,list}) {
            uint owner;if(window==IntPtr.Zero || GetWindowThreadProcessId(window,out owner)==0 || owner!=pid || IsWindowVisible(window) || GetForegroundWindow()==window) throw new InvalidOperationException("Refusing a non-owned, visible or foreground HWND");
        }
        if(Class(host)!="flowmux.windows.native" || Class(panel)!="flowmux.windows.files" || !String.Equals(Class(list),"ListBox",StringComparison.OrdinalIgnoreCase) || !IsChild(host,panel) || !IsChild(panel,list) || GetAncestor(list,2)!=host) throw new InvalidOperationException("Owned Files child hierarchy/class differs");
        int style=GetWindowLong(list,-16);
        if((style&0x40000000)==0 || (style&0x30)!=0 || (style&0x800)==0) throw new InvalidOperationException("Expected ordinary extended-selection child LISTBOX, not owner-drawn memory");
        return pid;
    }
    public static FilesListSnapshot Read(Process owned,long hostValue,long panelValue,long listValue) {
        var clock=Stopwatch.StartNew();IntPtr host=new IntPtr(hostValue),panel=new IntPtr(panelValue),list=new IntPtr(listValue);
        uint pid=Validate(owned,host,panel,list);int count=checked((int)Query(list,0x018B,0,clock));
        if(count<0 || count>20000) throw new InvalidOperationException("Native LISTBOX count exceeds retained Files cap");
        var text=new string[count];
        for(int i=0;i<count;i++) {
            int length=checked((int)Query(list,0x018A,i,clock));if(length<0 || length>32768) throw new InvalidOperationException("Native LISTBOX text exceeds bounded length");
            var buffer=new StringBuilder(length+1);IntPtr result;
            if(SendText(list,0x0189,new IntPtr(i),buffer,3,Remaining(clock),out result)==IntPtr.Zero || result.ToInt64()!=length) throw new InvalidOperationException("Native LISTBOX text failed, timed out or changed");
            text[i]=buffer.ToString();if(text[i].Length!=length) throw new InvalidOperationException("LISTBOX Unicode text length differs");
        }
        int selectedCount=checked((int)Query(list,0x0190,0,clock));if(selectedCount<0 || selectedCount>count) throw new InvalidOperationException("Invalid native selection count");
        var selected=new int[selectedCount];
        if(selectedCount>0) {IntPtr result;if(SendIndices(list,0x0191,new IntPtr(selectedCount),selected,3,Remaining(clock),out result)==IntPtr.Zero || result.ToInt64()!=selectedCount) throw new InvalidOperationException("Native selected-index read failed");}
        foreach(int index in selected) if(index<0 || index>=count) throw new InvalidOperationException("Native selected index escaped rows");
        int caret=checked((int)Query(list,0x019F,0,clock)),top=checked((int)Query(list,0x018E,0,clock));
        if(Query(list,0x018B,0,clock)!=count || Validate(owned,host,panel,list)!=pid) throw new InvalidOperationException("Owned list identity/count changed during snapshot");
        return new FilesListSnapshot{Host=hostValue,Panel=panelValue,List=listValue,ProcessId=pid,Count=count,Text=text,Selected=selected,Caret=caret,Top=top,ElapsedMs=clock.ElapsedMilliseconds};
    }
}
