// SPDX-License-Identifier: GPL-3.0-or-later
// Read-only existence/hash evidence for paths owned by the Files fixture.
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

public static class FilesActionsFixture {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern uint GetFileAttributesW(string path);
    public static bool Exists(FilesFixture fixture,string path) {
        if(fixture==null) throw new ArgumentNullException("fixture");
        fixture.Relative(path); // Reject paths outside the ordinal owned root.
        uint attributes=GetFileAttributesW("\\\\?\\"+path.Replace('/','\\'));
        if(attributes!=0xffffffffu) return true;
        int error=Marshal.GetLastWin32Error();
        if(error==2 || error==3) return false;
        throw new Win32Exception(error,"Owned action fixture existence probe failed");
    }
    public static string Sha256(byte[] bytes) {
        using(var hash=SHA256.Create()) return BitConverter.ToString(hash.ComputeHash(bytes)).Replace("-","").ToLowerInvariant();
    }
}
