// SPDX-License-Identifier: GPL-3.0-or-later
// Owned loopback page, native text inspection and verified panel WM_CLOSE. No desktop input.
using System;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public sealed class FindFixture : IDisposable {
    private readonly TcpListener listener;
    private readonly Thread thread;
    private readonly ManualResetEvent slowReleased = new ManualResetEvent(false);
    private volatile bool stopped;
    public readonly string Origin;
    public static readonly string Unicode = "한글 한 é 😀";
    public static readonly string Injection = "');window.findInjected=true;//";

    public FindFixture() {
        listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        Origin = "http://127.0.0.1:" + ((IPEndPoint)listener.LocalEndpoint).Port;
        thread = new Thread(Run);
        thread.IsBackground = true;
        thread.Start();
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
                    var path = request.Split(' ')[1];
                    if (path.StartsWith("/slow", StringComparison.Ordinal)) {
                        // The verifier releases this after observing loading rejection.
                        if (!slowReleased.WaitOne(5000) || stopped) return;
                    }
                    bool second = path.StartsWith("/second", StringComparison.Ordinal);
                    string html = "<!doctype html><meta charset='utf-8'><title>Find fixture</title>" +
                        "<style>body{font:18px sans-serif}p{margin:4px}</style>" +
                        "<p id='start'>start boundary</p>" +
                        (second ? "<p id='other'>second surface needle</p>" :
                        "<p id='one'>needle first</p><p id='two'>needle second</p><p id='three'>needle third</p>" +
                        "<p id='upper'>CASEtoken</p><p id='lower'>casetoken</p>" +
                        "<p id='unicode'>" + WebUtility.HtmlEncode(Unicode) + "</p>" +
                        "<p id='injection'>" + WebUtility.HtmlEncode(Injection) + "</p>") +
                        "<p id='manual'>user selection stays</p><p id='end'>end boundary</p>";
                    var body = Encoding.UTF8.GetBytes(html);
                    var header = Encoding.ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n" +
                        "Cache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n" +
                        "Content-Length: " + body.Length + "\r\nConnection: close\r\n\r\n");
                    stream.Write(header, 0, header.Length);
                    stream.Write(body, 0, body.Length);
                }
            } catch (IOException) {} catch (SocketException) {} catch (ObjectDisposedException) {}
        }
    }
    public void ReleaseSlow() { slowReleased.Set(); }
    public void Dispose() {
        stopped = true;
        slowReleased.Set();
        listener.Stop();
        thread.Join(3000);
        // Serve callbacks can still be unwinding; do not close their event handle.
    }
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message, IntPtr capacity,
        StringBuilder text, uint flags, uint timeout, out IntPtr result);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll", EntryPoint = "PostMessageW", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);
    public static void PostClose(long hwnd, int ownerPid) {
        uint actualPid;
        var window = new IntPtr(hwnd);
        if (hwnd == 0 || GetWindowThreadProcessId(window, out actualPid) == 0 || actualPid != (uint)ownerPid || IsWindowVisible(window))
            throw new IOException("Refusing WM_CLOSE for a visible window or a window outside the owned host");
        if (!PostMessage(window, 0x0010, IntPtr.Zero, IntPtr.Zero))
            throw new IOException("Cannot post WM_CLOSE to owned hidden find panel");
    }
    public static string ReadText(long hwnd) {
        var text = new StringBuilder(32768);
        IntPtr result;
        if (SendMessageTimeout(new IntPtr(hwnd), 0x000D, new IntPtr(text.Capacity), text, 2, 1000, out result) == IntPtr.Zero)
            throw new IOException("Cannot read owned find query control");
        return text.ToString();
    }
}
