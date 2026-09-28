# SPDX-License-Identifier: GPL-3.0-or-later
"""Collect completed search-stage evidence; never launch a process under test."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
DEST = ROOT / 'windows/evidence/2026-09-28/editor-search-diagnostics'


def read(path):
    return json.loads(path.read_text(encoding='utf-8-sig'))


def digest(data):
    return hashlib.sha256(data).hexdigest()


def relative(path):
    return str(path.relative_to(ROOT))


def source_path(value):
    path = Path(value)
    return (path if path.is_absolute() else ROOT / path).resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', action='append', default=[])
    parser.add_argument('--artifact', action='append', default=[], help='NAME=PATH for a completed artifact')
    args = parser.parse_args()
    DEST.mkdir(parents=True, exist_ok=True)
    manifest_path = DEST / 'manifest.json'
    manifest = read(manifest_path) if manifest_path.exists() else {
        'format_version': 1, 'collection': 'Windows editor search',
        'parent_commit': 'e9fd70be1d41dd620ebba7485df44538efd6036c',
        'encoding': 'UTF-8 without BOM; LF; trailing whitespace and extra EOF blank lines removed. JSON values preserved.',
        'scope': 'Windows-only partial editor search acceptance; no physical input, IME or clipboard evidence.',
        'status': 'collecting', 'artifacts': [], 'attempts': [], 'checks': [],
    }
    inventory = {entry['path']: entry for entry in manifest['artifacts']}

    def copy(value, name):
        source = source_path(value)
        if not source.is_relative_to(ROOT) or not source.is_file():
            raise ValueError('Expected existing repository artifact: ' + str(source))
        if not re.fullmatch(r'[A-Za-z0-9_.-]+', name):
            raise ValueError('Invalid evidence name: ' + name)
        raw = source.read_bytes()
        text = raw.decode('utf-8-sig').replace('\r\n', '\n').replace('\r', '\n')
        text = '\n'.join(line.rstrip() for line in text.split('\n')).rstrip('\n')
        normalized = (text + '\n' if text else '').encode('utf-8')
        if source.suffix == '.json':
            assert json.loads(raw.decode('utf-8-sig')) == json.loads(normalized), source
        entry = {'path': name, 'source': relative(source), 'source_bytes': len(raw),
                 'source_sha256': digest(raw), 'bytes': len(normalized),
                 'sha256': digest(normalized), 'normalization_changed_bytes': raw != normalized}
        target = DEST / name
        if name in inventory:
            if any(inventory[name].get(key) != value for key, value in entry.items()) or not target.is_file() or target.read_bytes() != normalized:
                raise ValueError('Refusing to change preserved evidence: ' + name)
        else:
            if target.exists() and target.read_bytes() != normalized:
                raise ValueError('Conflicting untracked evidence destination exists: ' + name)
            if not target.exists():
                target.write_bytes(normalized)
            inventory[name] = entry
        return name

    frozen = read(ROOT / 'windows/dist/editor-search-source.json')
    copy('windows/dist/editor-search-source.json', 'pre-deadline-review-source-hashes.json')
    # This snapshot intentionally predates the later deadline review correction.
    # Its173 hashes were checked against current files at initial collection.
    # Do not compare a historical source boundary with newer production edits.
    manifest['frozen_source_count'] = len(frozen['files'])
    for snapshot_file, snapshot_name, note in [
        ('editor-search-source-final.json', 'post-deadline-pre-separator-source-hashes.json',
         'After SearchOpen deadline correction, before extended-path separator correction.'),
        ('editor-search-source-path-final.json', 'final-source-hashes.json',
         'After SearchOpen deadline and extended-path separator corrections.')]:
        final_snapshot = ROOT / 'windows/dist' / snapshot_file
        if not final_snapshot.exists():
            continue
        final = read(final_snapshot)
        copy(final_snapshot, snapshot_name)
        if not any(boundary['snapshot'] == snapshot_name for boundary in manifest.get('source_boundaries', [])):
            changed = [name for name, expected in final['files'].items()
                       if digest((ROOT / name).read_bytes()) != expected]
            if changed:
                raise ValueError('New frozen source changed: ' + ', '.join(changed))
            previous = frozen if snapshot_name != 'final-source-hashes.json' else read(ROOT / 'windows/dist/editor-search-source-final.json')
            manifest.setdefault('source_boundaries', []).append({'snapshot': snapshot_name,
                'files': len(final['files']), 'current_hashes_verified_at': datetime.now(timezone.utc).isoformat(),
                'changed_from_previous': [name for name, expected in final['files'].items() if previous['files'].get(name) != expected],
                'note': note})
            manifest['source_boundaries'][-1]['snapshot'] = snapshot_name

    runs = set((ROOT / 'windows/dist/editor-search-runs').glob('*/manifest.json'))
    runs.update(source_path(path) for path in args.manifest)
    known_attempts = {entry['runner_source'] for entry in manifest['attempts']}
    known_checks = {entry['source'] for entry in manifest['checks']}
    skipped = []
    for path in sorted(runs):
        data = read(path)
        if data.get('status') not in ('passed', 'failed'):
            skipped.append(relative(path))
            continue
        copy(path, 'run-' + path.parent.name + '.json')
        kind = 'search' if path.parent.parent.name == 'editor-search-runs' else 'regression'
        for result in data['results']:
            runner_path = source_path(result['runnerResult'])
            source = relative(runner_path)
            runner = read(runner_path)
            if runner.get('status') not in ('passed', 'failed', 'timed_out'):
                raise ValueError('Completed manifest includes pending runner: ' + source)
            case = result['case']
            stem = ('passed-' if kind == 'search' else 'regression-') + case
            if result['status'] != 'passed':
                stem = 'failed-' + kind + '-' + case + '-' + runner_path.parent.name[-36:]
            same_case = [attempt for attempt in manifest['attempts']
                         if attempt['case'] == case and attempt['kind'] == kind]
            if source not in known_attempts and same_case:
                stem += '-repeat-' + runner_path.parent.name[-36:]
            elif source in known_attempts:
                stem = next(attempt['stem'] for attempt in manifest['attempts'] if attempt['runner_source'] == source)
            files = {}
            for key, suffix in [('evidenceJson', '-native.json'), ('runnerResult', '-runner.json'),
                                ('runnerStdout', '-stdout.txt'), ('runnerStderr', '-stderr.txt'),
                                ('driverOutput', '-driver.txt')]:
                if result.get(key):
                    files[key] = copy(result[key], stem + suffix)
            native = read(source_path(result['evidenceJson']))
            if result['status'] == 'passed':
                assert native['status'].startswith('passed'), case
                assert native['checks'] and all(check['passed'] for check in native['checks']), case
                assert runner.get('activeAfterCleanup') == 0, case
            if source not in known_attempts:
                manifest['attempts'].append({'kind': kind, 'source_kind': result.get('kind'),
                    'case': case, 'stem': stem, 'status': result['status'], 'runner_source': source,
                    'runner_elapsed_seconds': runner['elapsedSeconds'],
                    'driver_elapsed_seconds': result.get('elapsedSeconds'),
                    'checks': native['checks'], 'files': files})
                known_attempts.add(source)

    represented = known_attempts | known_checks
    for path in sorted((ROOT / 'windows/dist/checks').glob('editor-*/*result.json')):
        result = read(path)
        if not result.get('name', '').startswith(('editor-search', 'editor-find')):
            continue
        if result.get('status') not in ('passed', 'failed', 'timed_out', 'cancelled', 'terminated'):
            skipped.append(relative(path))
            continue
        if relative(path) in known_attempts:
            continue
        stem = path.parent.name
        files = {'result': copy(path, stem + '-runner.json')}
        for stream in ('stdout', 'stderr'):
            sibling = path.with_name(stream + '.txt')
            if sibling.is_file():
                files[stream] = copy(sibling, stem + '-' + stream + '.txt')
        if relative(path) not in represented:
            manifest['checks'].append({'name': result['name'], 'source': relative(path),
                'status': result['status'], 'elapsed_seconds': result['elapsedSeconds'], 'files': files})
            represented.add(relative(path))

    historical_verifier = ROOT / 'windows/dist/editor-search-history/test-native-editor-search.ps1'
    if historical_verifier.exists():
        copy(historical_verifier, 'verifier-test-native-editor-search.ps1')
        post_deadline_verifier = ROOT / 'windows/dist/editor-search-history/post-deadline-test-native-editor-search.ps1'
        if post_deadline_verifier.exists():
            copy(post_deadline_verifier, 'verifier-post-deadline-test-native-editor-search.ps1')
            if (ROOT / 'windows/dist/editor-search-source-path-final.json').exists():
                copy('windows/scripts/test-native-editor-search.ps1', 'verifier-final-test-native-editor-search.ps1')
        else:
            copy('windows/scripts/test-native-editor-search.ps1', 'verifier-final-test-native-editor-search.ps1')
    for filename in ([] if historical_verifier.exists() else ['test-native-editor-search.ps1']) + ['verify-editor.ps1', 'EditorFixture.cs',
                     'CliProbe.cs', 'CheckJob.cs', 'run-check.ps1']:
        copy('windows/scripts/' + filename, 'verifier-' + filename)
    for specification in args.artifact:
        name, separator, path = specification.partition('=')
        if not separator:
            raise ValueError('Expected NAME=PATH')
        copy(path, name)
    manifest['artifacts'] = list(inventory.values())
    manifest['copied_at_utc'] = datetime.now(timezone.utc).isoformat()
    manifest['artifact_count'] = len(inventory)
    manifest['summary'] = {
        'native_case_executions': len(manifest['attempts']),
        'native_case_checks': sum(len(item['checks']) for item in manifest['attempts']),
        'distinct_search_cases': len({item['case'] for item in manifest['attempts'] if item['kind'] == 'search'}),
        'search_executions': sum(item['kind'] == 'search' for item in manifest['attempts']),
        'regression_executions': sum(item['kind'] == 'regression' for item in manifest['attempts']),
        'failed_native_case_executions': sum(item['status'] != 'passed' for item in manifest['attempts']),
        'standalone_checks': len(manifest['checks']),
        'nonpassing_standalone_checks': sum(item['status'] != 'passed' for item in manifest['checks']),
    }
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'artifacts': len(inventory), 'native_attempts': len(manifest['attempts']),
        'checks': len(manifest['checks']), 'frozen_sources': len(frozen['files']),
        'skipped_pending': skipped}, ensure_ascii=False))


if __name__ == '__main__':
    main()
