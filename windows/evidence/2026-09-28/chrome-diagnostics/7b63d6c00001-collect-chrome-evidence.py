# SPDX-License-Identifier: GPL-3.0-or-later
"""Collect chrome evidence, never run checks. --check/--native add related results;
--artifact adds text/JSON; --hash records binaries without copying them. --since
filters automatic discovery by Unix mtime. PNGs are copied only below 500 KB.
"""
import argparse, hashlib, json
from datetime import datetime, timezone
from pathlib import Path, PureWindowsPath
ROOT = Path(__file__).resolve().parents[2]
DEST = ROOT / 'windows/evidence/2026-09-28/chrome-diagnostics'
TERMINAL = {'passed','failed','cancelled','terminated','timed_out','timeout','interrupted','driver_timeout','runner_error','evidence_error'}
def read(path): return json.loads(path.read_text(encoding='utf-8-sig'))
def sha(data): return hashlib.sha256(data).hexdigest()
def owned(value):
    win = PureWindowsPath(value)
    path = Path('/mnt/c').joinpath(*win.parts[1:]) if win.drive.upper() == 'C:' else ROOT / value
    path = path.resolve()
    if not path.is_relative_to(ROOT / 'windows') or not path.is_file(): raise ValueError('Expected owned Windows artifact: ' + str(path))
    return path

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ('check','native','artifact','hash'): parser.add_argument('--'+key, action='append', default=[])
    parser.add_argument('--since', type=float, default=0)
    args = parser.parse_args(); target = DEST / 'manifest.json'
    index = read(target) if target.exists() else {'stage':'native-chrome','status':'collecting','artifacts':{},'checks':{},'native':{},'hashes':{}}
    if index['stage'] != 'native-chrome' or index['status'] != 'collecting': raise ValueError('Collection is frozen or belongs to another stage')
    DEST.mkdir(parents=True, exist_ok=True)
    for name, record in index['artifacts'].items():
        data = (DEST / name).read_bytes()
        if len(data) != record['bytes'] or sha(data) != record['sha256']: raise ValueError('Preserved copy changed: '+name)
    def copy(value):
        path = owned(value); raw = path.read_bytes(); source = str(path.relative_to(ROOT))
        if path.suffix.lower() == '.png':
            if len(raw) >= 500000 or not raw.startswith(b'\x89PNG\r\n\x1a\n'): raise ValueError('Expected PNG below500KB: '+source)
            data = raw
        else:
            if path.suffix.lower() in {'.bmp','.exe','.dll','.com'}: raise ValueError('Use --hash for large/binary artifacts: '+source)
            text = raw.decode('utf-8-sig').replace('\r\n','\n').replace('\r','\n')
            text = '\n'.join(line.rstrip() for line in text.split('\n')).rstrip('\n'); data = (text+'\n' if text else '').encode()
            if path.suffix.lower() == '.json' and json.loads(raw.decode('utf-8-sig')) != json.loads(data): raise ValueError('JSON values changed')
        name = sha(source.encode())[:12]+'-'+path.name
        entry = {'source':source,'source_bytes':len(raw),'source_sha256':sha(raw),'bytes':len(data),'sha256':sha(data)}
        if name in index['artifacts'] and index['artifacts'][name] != entry: raise ValueError('Historical source changed: '+source)
        out = DEST / name
        if out.exists() and out.read_bytes() != data: raise ValueError('Historical destination changed: '+name)
        if not out.exists(): out.write_bytes(data)
        index['artifacts'][name] = entry; return name
    checks = {p for p in (ROOT/'windows/dist/checks').glob('chrome-*/result.json') if p.stat().st_mtime >= args.since}
    checks.update(owned(v) for v in args.check)
    for path in sorted(checks):
        result = read(path)
        if result.get('status') not in TERMINAL: continue
        files = {'result':copy(path)}
        for stream in ('stdout.txt','stderr.txt'):
            if path.with_name(stream).is_file(): files[stream] = copy(path.with_name(stream))
        index['checks'][str(path.relative_to(ROOT))] = {'status':result['status'],'elapsed_seconds':result.get('elapsedSeconds'),'files':files}
    native = {p for p in (ROOT/'windows/dist/evidence').glob('chrome-*/native-chrome-background.json') if p.stat().st_mtime >= args.since}
    native.update(owned(v) for v in args.native)
    for path in sorted(native):
        result = read(path); status = result.get('status','')
        if status not in TERMINAL and not status.startswith('passed_'): continue
        files = {'evidence':copy(path)}; colors = [str(o['background']).lower() for o in result.get('observations',[]) if 'background' in o]
        blank = any(c in {'#000000','#ff00ff'} for c in colors)
        for ordinal, artifact in enumerate(result.get('artifacts',[])):
            image = owned(artifact['path'])
            if image.suffix.lower() != '.png': continue
            raw = image.read_bytes()
            if len(raw) != artifact['bytes'] or sha(raw) != artifact['sha256'].lower(): raise ValueError('Referenced PNG differs: '+str(image))
            files['png'+str(ordinal)] = copy(image)
        acceptance = 'related_case_only'
        if path.name == 'native-chrome-background.json':
            acceptance = 'capture_only_not_visual_acceptance' if result.get('baseline') or blank else 'native_chrome_subset_only' if status.startswith('passed_') else 'failed_or_incomplete'
        index['native'][str(path.relative_to(ROOT))] = {'original_status':status,'acceptance':acceptance,'sampled_backgrounds':colors,'blank_or_placeholder_sample':blank,'files':files}
    for value in args.artifact: copy(value)
    for value in args.hash:
        path = owned(value); before = path.stat(); digest = hashlib.sha256()
        with path.open('rb') as stream:
            for chunk in iter(lambda:stream.read(1024*1024),b''): digest.update(chunk)
        after = path.stat()
        if (before.st_size,before.st_mtime_ns) != (after.st_size,after.st_mtime_ns): raise ValueError('Artifact changed while hashing')
        source = str(path.relative_to(ROOT)); index['hashes'][source+'#'+digest.hexdigest()] = {'path':source,'bytes':after.st_size,'sha256':digest.hexdigest()}
    index['updated_utc'] = datetime.now(timezone.utc).isoformat()
    temporary = target.with_suffix('.json.tmp'); temporary.write_text(json.dumps(index,ensure_ascii=False,indent=2)+'\n'); temporary.replace(target)
    print(json.dumps({k:len(index[k]) for k in ('artifacts','checks','native','hashes')}))
if __name__ == '__main__': main()
