// SPDX-License-Identifier: GPL-3.0-or-later
// Owned loopback popup pages only. No window activation, physical input or clipboard access.
using System;
using System.Collections.Concurrent;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Threading;

public sealed class PopupFixture : IDisposable {
    private readonly TcpListener listener;
    private readonly Thread thread;
    private readonly ManualResetEvent slowReleased = new ManualResetEvent(false);
    private readonly ManualResetEvent popupReleased = new ManualResetEvent(false);
    private readonly ConcurrentQueue<string> requests = new ConcurrentQueue<string>();
    private volatile bool stopped;
    public readonly string Origin;
    public int ChildRequests;
    public int SlowRequests;
    public int CompletedScheduledRequests;
    public int PopupGateRequests;
    public static readonly string Unicode = "새 탭 한글 한 é 😀 + &";

    public PopupFixture() {
        listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        Origin = "http://127.0.0.1:" + ((IPEndPoint)listener.LocalEndpoint).Port;
        thread = new Thread(Run);
        thread.IsBackground = true;
        thread.Start();
    }
    private static string Js(string value) {
        return "\"" + value.Replace("\\", "\\\\").Replace("\"", "\\\"")
            .Replace("\r", "\\r").Replace("\n", "\\n").Replace("<", "\\u003c")
            .Replace("\u2028", "\\u2028").Replace("\u2029", "\\u2029") + "\"";
    }
    private string Document(string path) {
        string role = path.StartsWith("/child", StringComparison.Ordinal) || path.StartsWith("/slow-child", StringComparison.Ordinal)
            ? "child" : path.StartsWith("/next", StringComparison.Ordinal) ? "next" : "source";
        string link = "/child?message=" + Uri.EscapeDataString(Unicode);
        string blank = "<!doctype html><meta charset='utf-8'><title>Popup blank</title><p id='message'>" +
            WebUtility.HtmlEncode(Unicode) + "</p>";
        return "<!doctype html><meta charset='utf-8'><title>Popup " + role + "</title>" +
            "<h1 id='role'>" + role + "</h1><p id='message'></p>" +
            "<a id='target-blank' target='_blank' rel='opener' href='" + WebUtility.HtmlEncode(link) + "'>" +
            WebUtility.HtmlEncode(Unicode) + "</a><a id='next' href='/next'>next document</a>" +
            "<script>const fixtureUnicode=" + Js(Unicode) + ";const fixtureBlank=" + Js(blank) + ";" +
            "const fixtureRole=" + Js(role) + ";" + @"
window.popupFixture={role:fixtureRole,unicode:fixtureUnicode,sourceToken:'owned_'+Date.now()+'_'+Math.random(),refs:[],opens:[],messages:[]};
const pf=window.popupFixture;
pf.message=new URLSearchParams(location.search).get('message')||fixtureUnicode;
document.getElementById('message').textContent=pf.message;
pf.observe=function(){
  let openerToken=null,openerError=null;
  try {openerToken=window.opener&&window.opener.popupFixture?window.opener.popupFixture.sourceToken:null;}
  catch(error){openerError=String(error);}
  return {role:pf.role,message:pf.message,sourceToken:pf.sourceToken,hasOpener:window.opener!==null,
    openerToken,openerError,href:location.href,ipc:typeof window.ipc,host:typeof window.flowmuxHost,
    identity:typeof window.__flowmuxIdentity,settings:typeof window.__flowmuxSettings,documentFocused:document.hasFocus()};
};
pf.openUrl=function(url,name){
  let child=null,error=null;
  try {child=window.open(url,name||'_blank');}catch(value){error=String(value);}
  const index=pf.refs.push(child)-1;
  const result={index,url,returned:child!==null,error};pf.opens.push(result);return result;
};
pf.openBlank=function(){
  const result=pf.openUrl('about:blank');const child=pf.refs[result.index];
  result.populated=false;
  if(child){try {
    child.document.open();child.document.write(fixtureBlank);child.document.close();
    child.popupFixture={role:'blank-child',message:fixtureUnicode,sourceToken:'blank_'+pf.sourceToken};
    result.populated=true;
  }catch(error){result.populateError=String(error);}}
  return result;
};
pf.queueOpen=function(url,delay){
  if(!Number.isInteger(delay)||delay<0||delay>2000)throw new Error('Invalid owned fixture delay');
  const item={url,delay,fired:false,result:null};pf.scheduled=item;
  setTimeout(()=>{item.fired=true;item.result=pf.openUrl(url);new Image().src='/event?scheduled=completed';},delay);
  return {scheduled:true,delay};
};
pf.gatedOpen=function(url){
  const item={url,fired:false,result:null};pf.scheduled=item;
  const image=new Image();pf.gateImage=image;
  image.onload=()=>{item.fired=true;item.result=pf.openUrl(url);new Image().src='/event?scheduled=completed';};
  image.onerror=()=>{item.error='owned gate did not load';};
  image.src='/popup-gate?request='+Date.now()+'_'+Math.random();return {armed:true};
};
pf.flood=function(count){
  if(!Number.isInteger(count)||count<1||count>64)throw new Error('Invalid owned fixture count');
  const results=[];for(let i=0;i<count;i++)results.push(pf.openUrl('/child?flood='+i));return results;
};
pf.reference=function(index){
  const child=pf.refs[index];if(!child)return {exists:false};
  try{return {exists:true,closed:child.closed};}catch(error){return {exists:true,error:String(error)};}
};
if(window.opener){try {
  window.opener.popupFixture.messages.push({sourceToken:pf.sourceToken,role:pf.role,message:pf.message,href:location.href});
}catch(error){pf.openerWriteError=String(error);}}
</script>";
    }
    private void Run() {
        while (!stopped) {
            try {
                var client = listener.AcceptTcpClient();
                ThreadPool.QueueUserWorkItem(_ => Serve(client));
            } catch (SocketException) { if (!stopped) throw; }
              catch (ObjectDisposedException) { if (!stopped) throw; }
        }
    }
    private void Serve(TcpClient client) {
        using (client) {
            try {
                client.ReceiveTimeout = 3000;
                client.SendTimeout = 3000;
                using (var stream = client.GetStream()) {
                    var reader = new StreamReader(stream, Encoding.ASCII, false, 1024, true);
                    var request = reader.ReadLine();
                    if (request == null) return;
                    string line;
                    while (!String.IsNullOrEmpty(line = reader.ReadLine())) {}
                    var parts = request.Split(' ');
                    if (parts.Length < 2) return;
                    var path = new Uri(new Uri(Origin), parts[1]).AbsolutePath;
                    if (requests.Count < 256) requests.Enqueue(parts[1]);
                    if (path == "/event") Interlocked.Increment(ref CompletedScheduledRequests);
                    if (path == "/popup-gate") {
                        Interlocked.Increment(ref PopupGateRequests);
                        if (!popupReleased.WaitOne(5000) || stopped) return;
                        var pixel = Convert.FromBase64String("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7");
                        var pixelHeader = Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\nCache-Control: no-store\r\n" +
                            "Content-Length: " + pixel.Length + "\r\nConnection: close\r\n\r\n");
                        stream.Write(pixelHeader, 0, pixelHeader.Length);stream.Write(pixel, 0, pixel.Length);
                        return;
                    }
                    if (path.StartsWith("/child", StringComparison.Ordinal)) Interlocked.Increment(ref ChildRequests);
                    if (path.StartsWith("/slow-child", StringComparison.Ordinal)) {
                        Interlocked.Increment(ref SlowRequests);
                        // This delays HTTP loading, not the host's native NewWindowRequested deferral.
                        if (!slowReleased.WaitOne(5000) || stopped) return;
                    }
                    var body = Encoding.UTF8.GetBytes(Document(path));
                    var header = Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n" +
                        "Cache-Control: no-store\r\nContent-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; connect-src 'none'\r\n" +
                        "Content-Length: " + body.Length + "\r\nConnection: close\r\n\r\n");
                    stream.Write(header, 0, header.Length);
                    stream.Write(body, 0, body.Length);
                }
            } catch (IOException) {} catch (SocketException) {} catch (ObjectDisposedException) {}
        }
    }
    public string[] RequestLog() { return requests.ToArray(); }
    public void BlockSlow() { slowReleased.Reset(); }
    public void ReleaseSlow() { slowReleased.Set(); }
    public void BlockPopup() { popupReleased.Reset(); }
    public void ReleasePopup() { popupReleased.Set(); }
    public void Dispose() {
        stopped = true;slowReleased.Set();popupReleased.Set();listener.Stop();thread.Join(3000);
        // Existing Serve callbacks can still be unwinding; their gate lives until process exit.
    }
}
