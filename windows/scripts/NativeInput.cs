// SPDX-License-Identifier: GPL-3.0-or-later
// Safety: every keyboard batch refuses to run unless our exact HWND is foreground.
using System;
using System.Runtime.InteropServices;
using System.Threading;
public static class NativeInput {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] struct GuiInfo {
        public uint Size, Flags;
        public IntPtr Active, Focus, Capture, MenuOwner, MoveSize, Caret;
        public Rect CaretRect;
    }
    [StructLayout(LayoutKind.Sequential)] struct Keyboard {
        public ushort Key, Scan; public uint Flags, Time; public IntPtr Extra;
    }
    [StructLayout(LayoutKind.Sequential)] struct Mouse {
        public int X, Y; public uint Data, Flags, Time; public IntPtr Extra;
    }
    [StructLayout(LayoutKind.Explicit)] struct Union {
        [FieldOffset(0)] public Keyboard Keyboard;
        [FieldOffset(0)] public Mouse Mouse;
    }
    [StructLayout(LayoutKind.Sequential)] struct Input { public uint Type; public Union Value; }
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr window);
    [DllImport("user32.dll")] static extern bool AttachThreadInput(uint from, uint to, bool attach);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string className, string title);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr window, System.Text.StringBuilder text, int length);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] static extern bool GetGUIThreadInfo(uint thread, ref GuiInfo info);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern IntPtr LoadKeyboardLayout(string name, uint flags);
    [DllImport("user32.dll")] static extern IntPtr GetKeyboardLayout(uint thread);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint type);
    [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint count, Input[] inputs, int size);
    [DllImport("imm32.dll")] static extern IntPtr ImmGetContext(IntPtr window);
    [DllImport("imm32.dll")] static extern bool ImmReleaseContext(IntPtr window, IntPtr context);
    [DllImport("imm32.dll")] static extern bool ImmGetConversionStatus(IntPtr context, out uint conversion, out uint sentence);
    [DllImport("imm32.dll")] static extern bool ImmSetConversionStatus(IntPtr context, uint conversion, uint sentence);
    [DllImport("imm32.dll")] static extern bool ImmSetOpenStatus(IntPtr context, bool open);
    public static void Foreground(IntPtr window) {
        // Newly created WebView/console windows can still be settling. No input
        // is injected during these bounded focus attempts; every batch rechecks.
        for (int attempt=0; attempt<3; attempt++) {
            uint owner;
            uint foreground = GetWindowThreadProcessId(GetForegroundWindow(), out owner);
            uint current = GetCurrentThreadId();
            bool attached = foreground != current && AttachThreadInput(current, foreground, true);
            try { ShowWindow(window, 9); BringWindowToTop(window); SetForegroundWindow(window); }
            finally { if (attached) AttachThreadInput(current, foreground, false); }
            Thread.Sleep(250);
            if (GetForegroundWindow() == window) return;
        }
        RequireForeground(window);
    }
    public static IntPtr Focused(IntPtr window) {
        uint process; uint thread = GetWindowThreadProcessId(window, out process);
        var info = new GuiInfo(); info.Size = (uint)Marshal.SizeOf(typeof(GuiInfo));
        if (!GetGUIThreadInfo(thread, ref info)) throw new Exception("GetGUIThreadInfo failed");
        return info.Focus;
    }
    public static string Korean(IntPtr window) {
        RequireForeground(window);
        IntPtr focus = Focused(window);
        IntPtr layout = LoadKeyboardLayout("00000412", 0);
        if (layout == IntPtr.Zero) throw new Exception("Microsoft Korean keyboard layout is not available");
        PostMessage(window, 0x50, IntPtr.Zero, layout);
        PostMessage(focus, 0x50, IntPtr.Zero, layout);
        Thread.Sleep(200);
        IntPtr context = ImmGetContext(focus);
        if (context == IntPtr.Zero) {
            uint process; uint thread = GetWindowThreadProcessId(focus, out process);
            Key(window, 0x15, false); // Real Hangul toggle, handled by TSF rather than IMM32.
            return String.Format("TSF Hangul toggle; focus={0}; thread={1}; HKL={2}; requested={3}", focus, thread, GetKeyboardLayout(thread), layout);
        }
        uint conversion, sentence;
        bool read = ImmGetConversionStatus(context, out conversion, out sentence);
        bool open = ImmSetOpenStatus(context, true);
        bool set = ImmSetConversionStatus(context, conversion | 1, sentence);
        ImmGetConversionStatus(context, out conversion, out sentence);
        ImmReleaseContext(focus, context);
        return String.Format("read={0}; open={1}; set={2}; conversion={3}; focus={4}", read, open, set, conversion, focus);
    }
    public static void RequireForeground(IntPtr window) {
        if (GetForegroundWindow() != window) throw new Exception("Refusing keyboard input: target window lost foreground");
    }
    // Read-only inspection also works when the owned host stays hidden.
    public static string[] ButtonTitles(IntPtr window) {
        var result = new System.Collections.Generic.List<string>();
        IntPtr button = IntPtr.Zero;
        while ((button = FindWindowEx(window, button, "BUTTON", null)) != IntPtr.Zero) {
            var text = new System.Text.StringBuilder(4096);
            GetWindowText(button, text, text.Capacity);
            result.Add(text.ToString());
        }
        return result.ToArray();
    }
    public static void ClickButton(IntPtr window, string title) {
        RequireForeground(window);
        IntPtr button = FindWindowEx(window, IntPtr.Zero, "BUTTON", title);
        if (button == IntPtr.Zero) throw new Exception("Native button not found: " + title);
        IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            Rect rect;
            if (!GetWindowRect(button, out rect)) throw new Exception("GetWindowRect failed");
            var point = new Point { X=(rect.Left + rect.Right) / 2, Y=(rect.Top + rect.Bottom) / 2 };
            if (WindowFromPoint(point) != button) throw new Exception("Refusing mouse input: native button is obscured");
            SetCursorPos(point.X, point.Y);
        } finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
        RequireForeground(window);
        var inputs = new[] {
            new Input { Type=0, Value=new Union { Mouse=new Mouse { Flags=2 } } },
            new Input { Type=0, Value=new Union { Mouse=new Mouse { Flags=4 } } }
        };
        if (SendInput(2, inputs, Marshal.SizeOf(typeof(Input))) != 2)
            throw new Exception("Mouse SendInput failed");
    }
    public static IntPtr MenuWindow(IntPtr window) {
        uint owner; GetWindowThreadProcessId(window, out owner);
        IntPtr found = IntPtr.Zero;
        while ((found = FindWindowEx(IntPtr.Zero, found, "#32768", null)) != IntPtr.Zero) {
            uint process; GetWindowThreadProcessId(found, out process);
            if (process == owner) return found;
        }
        return IntPtr.Zero;
    }
    public static void Key(IntPtr window, ushort key, bool shift) {
        RequireForeground(window);
        var list = new System.Collections.Generic.List<Input>();
        if (shift) list.Add(Make(0x10, false));
        list.Add(Make(key, false)); list.Add(Make(key, true));
        if (shift) list.Add(Make(0x10, true));
        var inputs = list.ToArray();
        if (SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(Input))) != inputs.Length)
            throw new Exception("SendInput failed: " + Marshal.GetLastWin32Error());
    }
    static Input Make(ushort key, bool up) {
        uint scan = MapVirtualKey(key, 4); // MAPVK_VK_TO_VSC_EX preserves the E0 prefix.
        // Some IME layouts return a bare numpad scan code even with MAPVK_VK_TO_VSC_EX.
        bool navigation = (key >= 0x21 && key <= 0x28) || key == 0x2D || key == 0x2E;
        uint extended = navigation || (scan & 0xFF00) == 0xE000 ? 1u : 0u;
        return new Input { Type=1, Value=new Union { Keyboard=new Keyboard {
            Key=key, Scan=(ushort)(scan & 0xFF), Flags=extended | (up ? 2u : 0u) } } };
    }
}
