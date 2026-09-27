// SPDX-License-Identifier: GPL-3.0-or-later
// Loopback-only HTTP fixture and read-only native control inspection. No UI input.
using System;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public sealed class BrowserFixture : IDisposable {
    private readonly TcpListener listener;
    private readonly Thread thread;
    private volatile bool stopped;
    public readonly string Origin;
    public BrowserFixture() {
        listener=new TcpListener(IPAddress.Loopback,0);listener.Start();
        Origin="http://127.0.0.1:"+((IPEndPoint)listener.LocalEndpoint).Port;
        thread=new Thread(Run);thread.IsBackground=true;thread.Start();
    }
    private void Run() {
        while(!stopped) {try {var client=listener.AcceptTcpClient();ThreadPool.QueueUserWorkItem(_=>Serve(client));} catch(SocketException) {if(!stopped)throw;} }
    }
    private void Serve(TcpClient client) {
        using(client) {try {
            client.ReceiveTimeout=3000;client.SendTimeout=3000;
            using(var stream=client.GetStream()) {
                var reader=new StreamReader(stream,Encoding.ASCII,false,1024,true);
                var request=reader.ReadLine();if(request==null)return;
                string line;while(!String.IsNullOrEmpty(line=reader.ReadLine())) {}
                var path=request.Split(' ')[1];
                if(path.StartsWith("/slow")) {for(int i=0;i<50&&!stopped;i++)Thread.Sleep(100);}
                if(path.StartsWith("/fail")) return;
                if(path.StartsWith("/redirect")) {
                    var redirect=Encoding.ASCII.GetBytes("HTTP/1.1 302 Found\r\nLocation: /two\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    stream.Write(redirect,0,redirect.Length);return;
                }
                var second=path.StartsWith("/two");
                var label=second?"둘째":"첫째";
                var html="<!doctype html><meta charset='utf-8'><title>"+label+" 한글 한 é 😀</title>"+
                    "<h1 id='label'>"+label+" 한글 한 é 😀</h1><input id='entry'><a href='/two'>다음</a>"+
                    "<script>window.fixtureLoad=Math.random();window.fixtureText='한글 한 é 😀';</script>";
                var body=Encoding.UTF8.GetBytes(html);
                var header=Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; connect-src 'none'\r\nContent-Length: "+body.Length+"\r\nConnection: close\r\n\r\n");
                stream.Write(header,0,header.Length);stream.Write(body,0,body.Length);
            }
        } catch(IOException) {} catch(ObjectDisposedException) {} }
    }
    public void Dispose() {stopped=true;listener.Stop();thread.Join(3000);}
    [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    private static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint msg,IntPtr w,StringBuilder text,uint flags,uint timeout,out IntPtr result);
    public static string ReadText(long hwnd) {
        var text=new StringBuilder(32768);IntPtr result;
        if(SendMessageTimeout(new IntPtr(hwnd),0x000D,new IntPtr(text.Capacity),text,2,1000,out result)==IntPtr.Zero)throw new IOException("Cannot read owned control");
        return text.ToString();
    }
}
