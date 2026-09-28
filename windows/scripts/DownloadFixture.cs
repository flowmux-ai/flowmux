// SPDX-License-Identifier: GPL-3.0-or-later
// Owned loopback attachments only; never writes files or interacts with the desktop.
using System;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Threading;
public sealed class DownloadFixture : IDisposable {
    private readonly TcpListener listener;
    private readonly Thread thread;
    private volatile bool stopped;
    public readonly string Origin;
    public int BrokenRequests;
    public static readonly string Name="한 글 한 é 😀.txt";
    public static readonly string Text="다운로드 한글 한 é 😀\r\nsecond line\n";
    public DownloadFixture() {
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
                if(path.StartsWith("/dom")) {
                    var html=Encoding.UTF8.GetBytes("<!doctype html><meta charset='utf-8'><title>Downloads 한글</title><h1>Owned download fixture</h1><a href='/one'>한글 파일</a>");
                    var header=Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: "+html.Length+"\r\nConnection: close\r\n\r\n");
                    stream.Write(header,0,header.Length);stream.Write(html,0,html.Length);return;
                }
                bool slow=path.StartsWith("/slow"), empty=path.StartsWith("/empty"), broken=path.StartsWith("/broken"), unknown=path.StartsWith("/unknown");
                if(broken)Interlocked.Increment(ref BrokenRequests);
                var payload=Encoding.UTF8.GetBytes(empty?"":Text);
                var filename=slow?"취소 한 😀.bin":empty?"empty.txt":broken?"broken.txt":unknown?"unknown.txt":Name;
                long length=slow?1024*1024:broken?payload.Length+200000:payload.Length;
                var headers="HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename*=UTF-8''"+Uri.EscapeDataString(filename)+"\r\nCache-Control: no-store\r\nConnection: close\r\n";
                if(!unknown)headers+="Content-Length: "+length+"\r\n";
                var bytes=Encoding.ASCII.GetBytes(headers+"\r\n");stream.Write(bytes,0,bytes.Length);stream.Flush();
                if(slow) {
                    var chunk=new byte[4096];for(int i=0;i<chunk.Length;i++)chunk[i]=(byte)(i%251);
                    for(int n=0;n<256&&!stopped;n++) {stream.Write(chunk,0,chunk.Length);stream.Flush();Thread.Sleep(100);}
                } else {stream.Write(payload,0,payload.Length);stream.Flush();}
            }
        } catch(IOException) {} catch(ObjectDisposedException) {} }
    }
    public void Dispose() {stopped=true;listener.Stop();thread.Join(3000);}
}
