# SPDX-License-Identifier: GPL-3.0-or-later
"""Collect completed F08 evidence only; never build, run tests or launch artifacts.

Default: files-actions-* checks and terminal files-actions-runs manifests.
Optional --check/--manifest add related runs; --artifact copies a text artifact;
--snapshot preserves a historical source snapshot, --current-snapshot also checks
current input hashes; --binary records a binary hash without copying it.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEST = ROOT / 'windows/evidence/2026-09-28/files-actions-diagnostics'
TERMINAL = {'passed', 'failed', 'cancelled', 'terminated', 'timed_out', 'timeout', 'interrupted',
            'driver_timeout', 'runner_error', 'evidence_error'}


def read(path):
    return json.loads(path.read_text(encoding='utf-8-sig'))


def sha(data):
    return hashlib.sha256(data).hexdigest()


def owned(value):
    path = (ROOT / value).resolve()
    if not path.is_relative_to(ROOT / 'windows') or not path.is_file():
        raise ValueError('Expected existing Windows repository artifact: ' + str(path))
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ('check', 'manifest', 'artifact', 'snapshot', 'current-snapshot', 'binary'):
        parser.add_argument('--' + option, action='append', default=[])
    args = parser.parse_args()
    index_path = DEST / 'manifest.json'
    index = read(index_path) if index_path.exists() else {
        'stage': 'F08', 'status': 'collecting', 'artifacts': {}, 'checks': {},
        'cases': {}, 'snapshots': {}, 'binaries': {},
        'normalization': 'UTF-8 without BOM; LF; trailing whitespace/blank EOF removed; JSON values unchanged',
    }
    if index['stage'] != 'F08' or index['status'] != 'collecting':
        raise ValueError('Refusing to append to another or frozen collection')
    DEST.mkdir(parents=True, exist_ok=True)
    for name, entry in index['artifacts'].items():
        data = (DEST / name).read_bytes()
        if len(data) != entry['bytes'] or sha(data) != entry['sha256']:
            raise ValueError('Preserved copy changed: ' + name)

    def copy(value):
        path = owned(value); raw = path.read_bytes()
        text = raw.decode('utf-8-sig').replace('\r\n', '\n').replace('\r', '\n')
        text = '\n'.join(line.rstrip() for line in text.split('\n')).rstrip('\n')
        data = (text + '\n' if text else '').encode()
        if path.suffix.lower() == '.json' and json.loads(raw.decode('utf-8-sig')) != json.loads(data):
            raise ValueError('JSON normalization changed values: ' + str(path))
        source = str(path.relative_to(ROOT)); name = sha(source.encode())[:12] + '-' + path.name
        entry = {'source': source, 'source_bytes': len(raw), 'source_sha256': sha(raw),
                 'bytes': len(data), 'sha256': sha(data)}
        target = DEST / name
        if name in index['artifacts'] and index['artifacts'][name] != entry:
            raise ValueError('Refusing changed historical source: ' + source)
        if target.exists() and target.read_bytes() != data:
            raise ValueError('Refusing changed historical destination: ' + name)
        if not target.exists():
            target.write_bytes(data)
        index['artifacts'][name] = entry
        return name

    checks = set((ROOT / 'windows/dist/checks').glob('files-actions-*/result.json'))
    checks.update(owned(value) for value in args.check)
    runs = set((ROOT / 'windows/dist/files-actions-runs').glob('*/manifest.json'))
    runs.update(owned(value) for value in args.manifest)
    for path in sorted(runs):
        run = read(path)
        if run.get('status') not in TERMINAL:
            continue
        manifest = copy(path)
        for ordinal, case in enumerate(run.get('results', [])):
            if case.get('status') not in TERMINAL:
                raise ValueError('Terminal manifest has an unfinished case: ' + str(path))
            files = {'manifest': manifest}
            for key in ('evidenceJson', 'driverOutput'):
                if case.get(key):
                    files[key] = copy(case[key])
            if case.get('runnerResult'):
                checks.add(owned(case['runnerResult']))
                files['runnerResult'] = copy(case['runnerResult'])
            if case['status'] == 'passed':
                native = read(owned(case['evidenceJson'])); runner = read(owned(case['runnerResult']))
                if (not str(native.get('status', '')).startswith('passed') or not native.get('checks')
                        or any(c.get('passed') is not True for c in native['checks'])
                        or runner.get('status') != 'passed' or runner.get('activeAfterCleanup') != 0):
                    raise ValueError('Passed case lacks native assertions or clean Job: ' + str(path))
            identity = str(path.relative_to(ROOT)) + '#' + str(ordinal)
            entry = {'case': case['case'], 'status': case['status'], 'files': files}
            if identity in index['cases'] and index['cases'][identity] != entry:
                raise ValueError('Preserved case changed: ' + identity)
            index['cases'][identity] = entry
    for path in sorted(checks):
        result = read(path)
        if result.get('status') not in TERMINAL:
            continue
        files = {'result': copy(path)}
        for stream in ('stdout.txt', 'stderr.txt'):
            if path.with_name(stream).is_file():
                files[stream] = copy(path.with_name(stream))
        index['checks'][str(path.relative_to(ROOT))] = {
            'status': result['status'], 'elapsed_seconds': result.get('elapsedSeconds'), 'files': files}
    for value in args.snapshot + args.current_snapshot:
        path = owned(value); snapshot = read(path)
        if not snapshot.get('files'):
            raise ValueError('Empty source snapshot')
        if value in args.current_snapshot:
            changed = [name for name, expected in snapshot['files'].items() if sha(owned(name).read_bytes()) != expected]
            if changed:
                raise ValueError('Current snapshot differs: ' + ', '.join(changed))
        index['snapshots'][copy(path)] = {'files': len(snapshot['files']), 'current_verified': value in args.current_snapshot}
    for value in args.artifact:
        copy(value)
    for value in args.binary:
        path = owned(value); before = path.stat(); digest = hashlib.sha256()
        with path.open('rb') as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                digest.update(chunk)
        after = path.stat()
        if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
            raise ValueError('Binary changed while hashing: ' + str(path))
        source = str(path.relative_to(ROOT)); key = source + '#' + digest.hexdigest()
        index['binaries'][key] = {'path': source, 'bytes': after.st_size, 'sha256': digest.hexdigest()}
    index['updated_utc'] = datetime.now(timezone.utc).isoformat()
    temporary = index_path.with_suffix('.json.tmp')
    temporary.write_text(json.dumps(index, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(index_path)
    print(json.dumps({key: len(index[key]) for key in ('artifacts', 'checks', 'cases', 'snapshots', 'binaries')}))


if __name__ == '__main__':
    main()
