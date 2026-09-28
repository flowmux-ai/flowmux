// SPDX-License-Identifier: GPL-3.0-or-later
// Optional diagnostic only: owned memory bitmap/HDC, no HWND/desktop/IME/input.
// APIs: https://learn.microsoft.com/windows/win32/api/usp10/nf-usp10-scriptstringanalyse
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
public static class UniscribeNfdProbe {
    [StructLayout(LayoutKind.Sequential)] private struct Rect { public int Left,Top,Right,Bottom; }
    [DllImport("gdi32.dll")] private static extern IntPtr SelectObject(IntPtr dc,IntPtr value);
    [DllImport("gdi32.dll")] private static extern bool DeleteObject(IntPtr value);
    [DllImport("gdi32.dll")] private static extern int SetBkMode(IntPtr dc,int mode);
    [DllImport("gdi32.dll")] private static extern uint SetTextColor(IntPtr dc,uint color);
    [DllImport("gdi32.dll",CharSet=CharSet.Unicode)] private static extern int GetTextFace(IntPtr dc,int count,StringBuilder face);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] private static extern int DrawText(IntPtr dc,IntPtr text,int count,ref Rect rect,uint flags);
    [DllImport("usp10.dll")] private static extern int ScriptStringAnalyse(IntPtr dc,IntPtr text,int length,int glyphs,int charset,uint flags,int width,IntPtr control,IntPtr state,IntPtr dx,IntPtr tabs,IntPtr classes,out IntPtr analysis);
    [DllImport("usp10.dll")] private static extern int ScriptStringOut(IntPtr analysis,int x,int y,uint options,IntPtr rect,int min,int max,bool disabled);
    [DllImport("usp10.dll")] private static extern int ScriptStringFree(ref IntPtr analysis);
    [DllImport("kernel32.dll",SetLastError=true)] private static extern int NormalizeString(int form,IntPtr source,int length,IntPtr destination,int capacity);
    public sealed class Result {
        public string Engine,Form,Text,Utf8Hex,Utf16Hex,DrawingUtf8Hex,DrawingUtf16Hex,RequestedFont,SelectedFont,PixelSha256,Png;
        public bool InputUnchanged,DrawingCopyUnchanged,DrawingNormalizedC; public int InkPixels,InkWidth; public float FontPoints;
    }
    private static string Hex(byte[] bytes) { return BitConverter.ToString(bytes).Replace("-","").ToLowerInvariant(); }
    private static void Hr(int result,string operation) { if(result!=0)throw new InvalidOperationException(operation+" returned 0x"+result.ToString("x8")); }
    private static string NormalizedCopy(IntPtr original,int length) {
        int capacity=NormalizeString(1,original,length,IntPtr.Zero,0);
        if(capacity<=0)throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(),"NormalizeString estimate failed");
        for(int attempt=0;attempt<3;attempt++) {
            if(capacity>4096)throw new InvalidOperationException("Unexpected diagnostic normalization size");
            IntPtr copy=Marshal.AllocHGlobal(checked(capacity*2));
            try {
                int written=NormalizeString(1,original,length,copy,capacity);
                if(written>0)return Marshal.PtrToStringUni(copy,written);
                int error=Marshal.GetLastWin32Error();
                if(error==122 && written<0){capacity=-written;continue;}
                throw new System.ComponentModel.Win32Exception(error,"NormalizeString copy failed");
            } finally {Marshal.FreeHGlobal(copy);}
        }
        throw new InvalidOperationException("NormalizeString estimate did not converge");
    }
    public static Result[] Run(string directory) {return Run(directory,"Segoe UI",false);}
    public static Result[] Run(string directory,string fontName) {return Run(directory,fontName,false);}
    public static Result[] Run(string directory,string fontName,bool normalizeDrawingCopy) {
        Directory.CreateDirectory(directory);var results=new List<Result>();
        string[] samples={"\uD55C\uAE00","\u1112\u1161\u11AB\u1100\u1173\u11AF"};
        using(var font=new Font(fontName,11,FontStyle.Regular,GraphicsUnit.Point)) {
            if(!String.Equals(font.Name,fontName,StringComparison.OrdinalIgnoreCase) && !String.Equals(font.FontFamily.GetName(0x409),fontName,StringComparison.OrdinalIgnoreCase))throw new InvalidOperationException("Requested font is not installed: "+fontName+"; selected "+font.Name);
            for(int engine=0;engine<2;engine++)for(int form=0;form<2;form++) {
                string text=samples[form],name=fontName.Replace(" ","-")+"-"+(normalizeDrawingCopy?"drawingcopy-c":"raw")+"-"+(engine==0?"drawtext":"uniscribe")+"-"+(form==0?"nfc":"nfd");
                var row=new Result{Engine=engine==0?"DrawTextW":"ScriptStringAnalyse/Out",Form=form==0?"NFC":"NFD",Text=text,Utf8Hex=Hex(Encoding.UTF8.GetBytes(text)),Utf16Hex=Hex(Encoding.Unicode.GetBytes(text)),FontPoints=11,RequestedFont=fontName,DrawingNormalizedC=normalizeDrawingCopy,Png=Path.Combine(directory,name+".png")};
                if(File.Exists(row.Png))throw new IOException("Refusing to overwrite prior diagnostic: "+row.Png);
                using(var bitmap=new Bitmap(256,64,PixelFormat.Format32bppRgb)) {
                    bitmap.SetResolution(96,96);
                    using(var graphics=Graphics.FromImage(bitmap)) {
                        graphics.Clear(Color.White);IntPtr dc=graphics.GetHdc(),face=IntPtr.Zero,old=IntPtr.Zero,source=IntPtr.Zero,drawn=IntPtr.Zero,analysis=IntPtr.Zero;
                        try {
                            face=font.ToHfont();old=SelectObject(dc,face);source=Marshal.StringToHGlobalUni(text);
                            string drawing=normalizeDrawingCopy?NormalizedCopy(source,text.Length):text;
                            if(normalizeDrawingCopy && drawing!=samples[0])throw new InvalidOperationException("Win32 normalization did not yield the authored NFC fixture");
                            drawn=Marshal.StringToHGlobalUni(drawing);row.DrawingUtf8Hex=Hex(Encoding.UTF8.GetBytes(drawing));row.DrawingUtf16Hex=Hex(Encoding.Unicode.GetBytes(drawing));
                            SetBkMode(dc,1);SetTextColor(dc,0);var actual=new StringBuilder(128);GetTextFace(dc,actual.Capacity,actual);row.SelectedFont=actual.ToString();
                            if(!String.Equals(row.SelectedFont,fontName,StringComparison.OrdinalIgnoreCase) && !String.Equals(row.SelectedFont,font.Name,StringComparison.OrdinalIgnoreCase))throw new InvalidOperationException("Unexpected selected GDI font: "+row.SelectedFont);
                            if(engine==0) {var rect=new Rect{Left=12,Top=12,Right=244,Bottom=52};if(DrawText(dc,drawn,drawing.Length,ref rect,0x20|0x800)==0)throw new InvalidOperationException("DrawTextW failed");}
                            else {Hr(ScriptStringAnalyse(dc,drawn,drawing.Length,drawing.Length*3/2+16,-1,128|32|4096,0,IntPtr.Zero,IntPtr.Zero,IntPtr.Zero,IntPtr.Zero,IntPtr.Zero,out analysis),"ScriptStringAnalyse");Hr(ScriptStringOut(analysis,12,12,0,IntPtr.Zero,0,0,false),"ScriptStringOut");}
                            row.InputUnchanged=String.Equals(text,Marshal.PtrToStringUni(source,text.Length),StringComparison.Ordinal);
                            row.DrawingCopyUnchanged=String.Equals(drawing,Marshal.PtrToStringUni(drawn,drawing.Length),StringComparison.Ordinal);
                            if(!row.InputUnchanged || !row.DrawingCopyUnchanged)throw new InvalidOperationException("Original or drawing UTF-16 changed");
                        } finally {if(analysis!=IntPtr.Zero)ScriptStringFree(ref analysis);if(source!=IntPtr.Zero)Marshal.FreeHGlobal(source);if(drawn!=IntPtr.Zero)Marshal.FreeHGlobal(drawn);if(old!=IntPtr.Zero)SelectObject(dc,old);if(face!=IntPtr.Zero)DeleteObject(face);graphics.ReleaseHdc(dc);}
                    }
                    byte[] pixels=new byte[256*64*4];int offset=0,left=256,right=-1;
                    for(int y=0;y<64;y++)for(int x=0;x<256;x++){Color c=bitmap.GetPixel(x,y);pixels[offset++]=c.R;pixels[offset++]=c.G;pixels[offset++]=c.B;pixels[offset++]=c.A;if(c.R!=255||c.G!=255||c.B!=255){row.InkPixels++;left=Math.Min(left,x);right=Math.Max(right,x);}}
                    if(row.InkPixels==0)throw new InvalidOperationException("Empty memory render: "+name);
                    row.InkWidth=right-left+1;using(var sha=SHA256.Create())row.PixelSha256=Hex(sha.ComputeHash(pixels));bitmap.Save(row.Png,ImageFormat.Png);
                }
                results.Add(row);
            }
        }
        return results.ToArray();
    }
}
