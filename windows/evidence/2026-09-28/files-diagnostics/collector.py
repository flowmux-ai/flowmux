# SPDX-License-Identifier: GPL-3.0-or-later
"""Append completed Files-stage artifacts; never launch a build or test process.

Examples (run only when evidence collection is authorized):
  python3 windows/dist/collect-files-evidence.py
  python3 windows/dist/collect-files-evidence.py --manifest PATH
  python3 windows/dist/collect-files-evidence.py \
    --source-snapshot final-source-hashes.json=windows/dist/files-source-final.json \
    --artifact release-entrypoints.json=windows/evidence/2026-09-28/native-files-release-entrypoints.json

Completed failed/timed-out/interrupted attempts are preserved alongside passes.
No article, readiness claim, review approval, or historical F06 file is written.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
DEST = ROOT / 'windows/evidence/2026-09-28/files-diagnostics'
HISTORY = ROOT / 'windows/dist/files-history'
PARENT = '32ea82fef998831d3877fbfb1214aa65ed413100'
TERMINAL = {'passed', 'failed', 'timed_out', 'cancelled', 'terminated',
            'driver_timeout', 'runner_error', 'evidence_error', 'interrupted'}
VERIFIERS = ('verify-files.ps1', 'FilesFixture.cs', 'CliProbe.cs', 'EditorFixture.cs',
             'CheckJob.cs', 'run-check.ps1')


def utc():
    return datetime.now(timezone.utc).isoformat()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read(path):
    return json.loads(path.read_text(encoding='utf-8-sig'))


def relative(path):
    return str(path.relative_to(ROOT))


def source_path(value):
    path = Path(value)
    path = (path if path.is_absolute() else ROOT / path).resolve()
    if not path.is_relative_to(ROOT) or not path.is_file():
        raise ValueError('Expected an existing repository artifact: ' + str(path))
    return path


def specification(value):
    name, separator, path = value.partition('=')
    if not separator or not path:
        raise ValueError('Expected NAME=PATH: ' + value)
    return name, path


def normalize(raw, path):
    text = raw.decode('utf-8-sig').replace('\r\n', '\n').replace('\r', '\n')
    text = '\n'.join(line.rstrip() for line in text.split('\n')).rstrip('\n')
    result = (text + '\n' if text else '').encode('utf-8')
    if path.suffix.lower() == '.json' and json.loads(raw.decode('utf-8-sig')) != json.loads(result):
        raise ValueError('Normalization changed JSON values: ' + str(path))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', action='append', default=[], help='Additional completed native run manifest; other run directories are regressions')
    parser.add_argument('--check', action='append', default=[], help='Additional completed bounded-runner result.json')
    parser.add_argument('--source-snapshot', action='append', default=[], help='NAME=PATH; new snapshot must match current source bytes')
    parser.add_argument('--historical-snapshot', action='append', default=[], help='NAME=PATH; preserve an explicitly historical snapshot without asserting current source equality')
    parser.add_argument('--artifact', action='append', default=[], help='NAME=PATH; append a completed textual artifact')
    args = parser.parse_args()
    DEST.mkdir(parents=True, exist_ok=True)
    manifest_path = DEST / 'manifest.json'
    manifest = read(manifest_path) if manifest_path.exists() else {
        'format_version': 1, 'collection': 'Windows native Files', 'parent_commit': PARENT,
        'encoding': 'UTF-8 without BOM; LF; trailing whitespace and extra EOF blank lines removed. JSON values preserved.',
        'scope': 'Windows-only partial Files evidence; hidden owned hosts; no physical input, IME or clipboard acceptance.',
        'status': 'collecting', 'collection_complete': False,
        'artifacts': [], 'attempts': [], 'checks': [], 'source_boundaries': [],
    }
    if manifest.get('parent_commit') != PARENT or manifest.get('collection') != 'Windows native Files':
        raise ValueError('Destination is not this Files stage; refusing to modify another collection')
    if manifest.get('status') != 'collecting':
        raise ValueError('Collection is frozen for review; refusing to append silently')
    inventory = {item['path']: item for item in manifest['artifacts']}
    skipped = []

    def copy(value, name, original_source=None):
        if not re.fullmatch(r'[A-Za-z0-9_.-]+', name) or name in {'.', '..', 'manifest.json'}:
            raise ValueError('Invalid evidence name: ' + name)
        source = source_path(value)
        raw = source.read_bytes()
        normalized = normalize(raw, source)
        entry = {'path': name, 'source': relative(source), 'source_bytes': len(raw),
                 'source_sha256': digest(raw), 'bytes': len(normalized),
                 'sha256': digest(normalized), 'normalization_changed_bytes': raw != normalized}
        if original_source is not None:
            entry['original_source'] = original_source
            entry['source_note'] = 'Exact raw source bytes archived by SHA256 before normalization; later verifier edits do not overwrite this boundary.'
        target = DEST / name
        if name in inventory:
            if inventory[name] != entry or not target.is_file() or target.read_bytes() != normalized:
                raise ValueError('Refusing to change preserved evidence: ' + name)
        else:
            if target.exists() and target.read_bytes() != normalized:
                raise ValueError('Conflicting evidence destination: ' + name)
            if not target.exists():
                target.write_bytes(normalized)
            inventory[name] = entry
        return name

    # Validate every previously copied artifact before appending. Original mutable
    # verifier files use their immutable raw history archive as source instead.
    for entry in manifest['artifacts']:
        target = DEST / entry['path']
        if not target.is_file() or len(target.read_bytes()) != entry['bytes'] or digest(target.read_bytes()) != entry['sha256']:
            raise ValueError('Preserved evidence changed: ' + entry['path'])

    previous_snapshot = None
    if manifest['source_boundaries']:
        previous_snapshot = read(DEST / manifest['source_boundaries'][-1]['snapshot'])
    for historical, options in [(True, args.historical_snapshot), (False, args.source_snapshot)]:
        for option in options:
            name, value = specification(option)
            path = source_path(value)
            snapshot = read(path)
            files = snapshot.get('files')
            if not isinstance(files, dict) or not files:
                raise ValueError('Snapshot must contain a nonempty files/hash mapping')
            if any(not filename.startswith('windows/') or not re.fullmatch('[0-9a-f]{64}', expected)
                   for filename, expected in files.items()):
                raise ValueError('Expected Windows-only source hashes')
            if not historical:
                changed = [filename for filename, expected in files.items()
                           if digest(source_path(filename).read_bytes()) != expected]
                if changed:
                    raise ValueError('Frozen source bytes changed: ' + ', '.join(changed))
            copy(path, name)
            existing = next((item for item in manifest['source_boundaries'] if item['snapshot'] == name), None)
            if existing is None:
                boundary = {'snapshot': name, 'files': len(files), 'historical': historical,
                            'recorded_at_utc': utc(), 'note': snapshot.get('note')}
                if not historical:
                    boundary['current_hashes_verified_at'] = utc()
                if previous_snapshot is not None:
                    before = previous_snapshot['files']
                    boundary['changed_from_previous'] = sorted(filename for filename in set(before) | set(files)
                                                               if before.get(filename) != files.get(filename))
                manifest['source_boundaries'].append(boundary)
                previous_snapshot = snapshot

    runs = set((ROOT / 'windows/dist/files-runs').glob('*/manifest.json'))
    runs.update(source_path(value) for value in args.manifest)
    runs = [(path, read(path)) for path in sorted(runs)]
    # A completed runner inside a still-running serial manifest is not a new
    # standalone check; wait for that manifest's final immutable copy.
    reserved_native_runners = {relative(source_path(result['runnerResult']))
                              for _, data in runs for result in data.get('results', [])
                              if result.get('runnerResult')}
    known_attempts = {item['identity'] for item in manifest['attempts']}
    native_runner_sources = {item['runner_source'] for item in manifest['attempts'] if item.get('runner_source')}
    for path, data in runs:
        if data.get('status') not in TERMINAL:
            skipped.append({'source': relative(path), 'status': data.get('status')})
            continue
        run_name = copy(path, 'run-' + path.parent.name + '.json')
        kind = 'files' if path.parent.parent.name == 'files-runs' else 'regression'
        for ordinal, result in enumerate(data.get('results', [])):
            if result.get('status') not in TERMINAL:
                raise ValueError('Terminal run manifest contains an unfinished attempt: ' + relative(path))
            case = result['case']
            if not re.fullmatch(r'[a-z0-9-]+', case):
                raise ValueError('Unexpected case name: ' + case)
            runner_path = source_path(result['runnerResult']) if result.get('runnerResult') else None
            runner_source = relative(runner_path) if runner_path else None
            identity = runner_source or (relative(path) + '#' + str(ordinal))
            runner = read(runner_path) if runner_path else None
            if runner is not None and runner.get('status') not in TERMINAL:
                raise ValueError('Completed run references a nonterminal runner: ' + runner_source)
            prior = next((item for item in manifest['attempts'] if item['identity'] == identity), None)
            unique = runner_path.parent.name[-36:] if runner_path else path.parent.name + '-' + str(ordinal)
            stem = prior['stem'] if prior else kind + '-' + case + '-' + unique
            files = {'run_manifest': run_name}
            for key, suffix in [('evidenceJson', '-native.json'), ('runnerResult', '-runner.json'),
                                ('runnerStdout', '-stdout.txt'), ('runnerStderr', '-stderr.txt'),
                                ('driverOutput', '-driver.txt')]:
                if result.get(key):
                    files[key] = copy(result[key], stem + suffix)
            native = read(source_path(result['evidenceJson'])) if result.get('evidenceJson') else None
            checks = native.get('checks', []) if native else []
            if result['status'] == 'passed':
                if (not native or not str(native.get('status', '')).startswith('passed') or not checks
                        or any(check.get('passed') is not True for check in checks)
                        or not runner or runner.get('status') != 'passed' or runner.get('activeAfterCleanup') != 0):
                    raise ValueError('Passed label lacks matching native checks and clean Job result: ' + identity)
            if identity not in known_attempts:
                manifest['attempts'].append({'identity': identity, 'kind': kind,
                    'source_kind': result.get('kind'), 'case': case, 'stem': stem,
                    'status': result['status'], 'runner_source': runner_source,
                    'runner_elapsed_seconds': runner.get('elapsedSeconds') if runner else None,
                    'driver_elapsed_seconds': result.get('elapsedSeconds'),
                    'evidence_status': native.get('status') if native else None,
                    'missing_native_evidence': native is None, 'checks': checks, 'files': files})
                known_attempts.add(identity)
            if runner_source:
                native_runner_sources.add(runner_source)

    check_paths = set((ROOT / 'windows/dist/checks').glob('files-*/result.json'))
    check_paths.update(source_path(value) for value in args.check)
    known_checks = {item['source'] for item in manifest['checks']}
    for path in sorted(check_paths):
        source = relative(path)
        if source in native_runner_sources or source in reserved_native_runners:
            continue
        result = read(path)
        if result.get('status') not in TERMINAL:
            skipped.append({'source': source, 'status': result.get('status')})
            continue
        stem = path.parent.name
        files = {'result': copy(path, stem + '-runner.json')}
        for stream in ('stdout', 'stderr'):
            sibling = path.with_name(stream + '.txt')
            if sibling.is_file():
                files[stream] = copy(sibling, stem + '-' + stream + '.txt')
        if source not in known_checks:
            manifest['checks'].append({'name': result.get('name'), 'source': source,
                'status': result['status'], 'elapsed_seconds': result.get('elapsedSeconds'), 'files': files})
            known_checks.add(source)

    # Preserve exact raw verifier bytes under ignored history before normalizing.
    # SHA-addressed names allow later failed/fixed verifier versions to coexist.
    HISTORY.mkdir(parents=True, exist_ok=True)
    for filename in VERIFIERS:
        source = source_path('windows/scripts/' + filename)
        raw = source.read_bytes()
        sha = digest(raw)
        archived = HISTORY / (sha + '-' + filename)
        if archived.exists() and archived.read_bytes() != raw:
            raise ValueError('Immutable verifier history hash collision')
        if not archived.exists():
            archived.write_bytes(raw)
        copy(archived, 'verifier-' + sha[:16] + '-' + filename, relative(source))
    for option in args.artifact:
        name, value = specification(option)
        copy(value, name)

    manifest['artifacts'] = list(inventory.values())
    manifest['artifact_count'] = len(inventory)
    manifest['last_collected_at_utc'] = utc()
    manifest['summary'] = {
        'native_case_executions': len(manifest['attempts']),
        'native_case_checks': sum(len(item['checks']) for item in manifest['attempts']),
        'distinct_files_cases': len({item['case'] for item in manifest['attempts'] if item['kind'] == 'files'}),
        'files_executions': sum(item['kind'] == 'files' for item in manifest['attempts']),
        'regression_executions': sum(item['kind'] == 'regression' for item in manifest['attempts']),
        'nonpassing_native_case_executions': sum(item['status'] != 'passed' for item in manifest['attempts']),
        'standalone_checks': len(manifest['checks']),
        'nonpassing_standalone_checks': sum(item['status'] != 'passed' for item in manifest['checks']),
    }
    temporary = manifest_path.with_suffix('.json.tmp')
    temporary.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    temporary.replace(manifest_path)
    print(json.dumps({'artifacts': len(inventory), 'native_attempts': len(manifest['attempts']),
        'checks': len(manifest['checks']), 'source_boundaries': len(manifest['source_boundaries']),
        'skipped_pending': skipped}, ensure_ascii=False))


if __name__ == '__main__':
    main()
