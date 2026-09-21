#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
import os,time,json,sys,importlib.util,subprocess
from pathlib import Path
from types import SimpleNamespace
from PIL import Image
from Xlib import X,XK,display
from Xlib.ext import xtest
sys.dont_write_bytecode=True
repo=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('fixture',repo/'scripts/test-ssh-workspace-gui.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
h=m.Harness(SimpleNamespace(gui=os.environ.get('AUDIT_GUI',str(repo/'target/debug/flowmux')),cli=os.environ.get('AUDIT_CLI',str(repo/'target/debug/flowmuxctl')),protected_pid=[int(p.name) for p in Path('/proc').iterdir() if p.name.isdigit() and (p/'comm').exists() and (p/'comm').read_text().strip()=='flowmux']))
h.env.update(GTK_USE_PORTAL='0',GTK_IM_MODULE='ibus',IBUS_ENABLE_SYNC_MODE=os.environ.get('AUDIT_SYNC','1'),GSETTINGS_BACKEND='memory',IBUS_ADDRESS='unix:path='+str(h.root/'ibus.sock'))
if os.environ.get('AUDIT_NAV'):h.env['FLOWMUX_ENABLE_IBUS_NAV_WORKAROUND']='1'
shell=h.root/'shell';shell.write_text('#!/bin/sh\nexec /bin/bash --noprofile --norc\n');shell.chmod(0o755)
(h.root/'config/flowmux').mkdir()
(h.root/'config/flowmux/options.json').write_text(json.dumps({'default_shell':str(shell),'terminal_minimap_enabled':False,'cursor_blink':False,'system_notifications_enabled':False}))
sink=h.root/'input_sink.py'
sink.write_text("#!/usr/bin/python3\n# SPDX-License-Identifier: GPL-3.0-or-later\nimport os,sys,tty,termios,time,select,json\nold=termios.tcgetattr(0);tty.setraw(0)\npath=sys.argv[1]\ntry:\n    os.write(1,b'\\x1b[?1004h\\x1b[?25l\\r\\nINPUT READY: ')\n    with open(path,'w',buffering=1) as out:\n        end=time.monotonic()+40\n        while time.monotonic()<end:\n            if select.select([0],[],[],.1)[0]:\n                data=os.read(0,4096)\n                out.write(json.dumps({'time':time.monotonic(),'hex':data.hex(),'text':data.decode('utf-8','replace')})+'\\n')\nfinally:\n    os.write(1,b'\\x1b[?1004l\\x1b[?25h\\r\\n')\n    termios.tcsetattr(0,termios.TCSANOW,old)\n")
phases=[]
try:
    h.start_display()
    log=(h.root/'ibus.log').open('w')
    ibus=h.spawn(['ibus-daemon','--panel=disable','--emoji-extension=disable','--address='+h.env['IBUS_ADDRESS'],'--cache=none'],stdout=log,stderr=log)
    m.wait_for(lambda:(h.root/'ibus.sock').exists(),'private IBus')
    app,socket=h.window('ime')
    ws=h.rpc(socket,'workspace_create',name='isolated IME',root=str(h.root))['workspace_created']['id']
    pane=h.workspace(socket,ws)['panes'][0]['id']
    d=display.Display(h.env['DISPLAY'])
    def main_window():
        return next((w for w in d.screen().root.query_tree().children if w.get_attributes().map_state==X.IsViewable and (p:=w.get_full_property(d.intern_atom('_NET_WM_PID'),X.AnyPropertyType)) is not None and int(p.value[0])==app.pid),None)
    w=m.wait_for(main_window,'test window');w.set_input_focus(X.RevertToParent,X.CurrentTime);d.sync()
    h.rpc(socket,'pane_focus',pane=pane)
    def key(name,delay=.08,mods=()):
        codes=[d.keysym_to_keycode(XK.string_to_keysym(k)) for k in (*mods,name)]
        assert all(codes),(name,codes)
        for code in codes:xtest.fake_input(d,X.KeyPress,code)
        for code in reversed(codes):xtest.fake_input(d,X.KeyRelease,code)
        d.sync();time.sleep(delay)
    def shot(name):
        root=d.screen().root;g=root.get_geometry();raw=root.get_image(0,0,g.width,g.height,X.ZPixmap,0xffffffff)
        Image.frombytes('RGB',(g.width,g.height),raw.data,'raw','BGRX').save(h.root/(name+'.png'))
    h.send(socket,pane,'/usr/bin/python3 '+str(sink)+' '+str(h.root/'input.jsonl'))
    m.wait_for(lambda:'INPUT READY' in h.screen(socket,pane),'raw input sink')
    subprocess.run(['ibus','engine','xkb:us::eng'],env=h.env,check=True,capture_output=True);time.sleep(.4)
    def phase(name,action):
        start=time.monotonic();action();time.sleep(.15);phases.append({'name':name,'start':start,'end':time.monotonic()});shot(name)
    phase('english',lambda:[key(k) for k in 'abc'])
    phase('single_enter',lambda:key('Return'))
    phase('fast_three_enter',lambda:[key('Return',.001) for _ in range(3)])
    result=subprocess.run(['ibus','engine','hangul'],env=h.env,capture_output=True,text=True,timeout=15)
    assert result.returncode==0,result.stderr
    time.sleep(.4)
    key('space',mods=('Shift_L',))
    phase('hangul_preedit',lambda:[key(k) for k in 'rksk'])
    phase('hangul_enter',lambda:key('Return'))
    phase('hangul_backspace',lambda:([key(k) for k in 'gks'],key('BackSpace')))
    phase('hangul_symbol',lambda:key('question',mods=('Shift_L',)))
    phase('hangul_shift_enter',lambda:([key(k) for k in 'rk'],key('Return',mods=('Shift_L',))))
    (h.root/'phases.json').write_text(json.dumps(phases,indent=2))
    records=[json.loads(line) for line in (h.root/'input.jsonl').read_text().splitlines()]
    received=''.join(record['text'] for record in records)
    text=received.replace('\x1b[I','').replace('\x1b[O','')
    expected='abc\r\r\r\r가나\r'+('한\x7f' if os.environ.get('AUDIT_NAV') else '하')+'?가\x1b\r'
    result={'received':text,'expected':expected,'focus_out':received.count('\x1b[O'),'sync':h.env['IBUS_ENABLE_SYNC_MODE'],'navigation_workaround':bool(os.environ.get('AUDIT_NAV'))}
    (h.root/'result.json').write_text(json.dumps(result,ensure_ascii=False,indent=2))
    print(json.dumps(result,ensure_ascii=False),flush=True)
    assert text == expected, result
    # Close only our isolated window; user PID is checked by the harness.
    h.close_window(app)
finally:
    for child in reversed(h.children):h.stop(child)
    h.events.close()
