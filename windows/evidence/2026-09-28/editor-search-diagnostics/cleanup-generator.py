# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate a read-only Windows PID check from recorded editor diagnostics.

This generator only reads evidence and writes editor-search-cleanup.ps1 plus metadata.
Re-run after the native unit/entrypoint evidence has been copied. It never queries
or controls processes. Run the generated script through scripts/run-check.ps1.
"""

from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import re


STAMP = re.compile(
    r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})"
    r"(?:\.(\d{1,7}))?(Z|[+-]\d{2}:\d{2})$"
)
WINDOWS_TARGET = re.compile(r"^[A-Za-z]:[\\/]")


def finish_utc(value, source):
    """Keep all seven DateTimeOffset fractional digits when normalizing to UTC."""
    match = STAMP.fullmatch(value) if isinstance(value, str) else None
    if match is None:
        raise ValueError(f"{source}: missing/invalid finish timestamp with timezone")
    whole, fraction, offset = match.groups()
    fraction = (fraction or "").ljust(7, "0")
    if offset == "Z":
        zone = timezone.utc
    else:
        hours, minutes = map(int, offset[1:].split(":"))
        if hours > 14 or minutes > 59 or (hours == 14 and minutes):
            raise ValueError(f"{source}: invalid DateTimeOffset timezone")
        sign = 1 if offset[0] == "+" else -1
        zone = timezone(sign * timedelta(hours=hours, minutes=minutes))
    utc = datetime.strptime(whole, "%Y-%m-%dT%H:%M:%S").replace(tzinfo=zone).astimezone(timezone.utc)
    ticks = ((utc.toordinal() - 1) * 86400 + utc.hour * 3600 + utc.minute * 60 + utc.second) * 10_000_000 + int(fraction)
    return ticks, f"{utc:%Y-%m-%dT%H:%M:%S}.{fraction}Z"


def main():
    dist = Path(__file__).resolve().parent
    evidence = dist.parent / "evidence" / "2026-09-28"
    diagnostics = evidence / "editor-search-diagnostics"
    if not diagnostics.is_dir():
        raise ValueError("editor-search-diagnostics must exist")
    records = {}
    included = []
    excluded = []
    ignored = []

    def read(path):
        return json.loads(path.read_text(encoding="utf-8-sig"))

    def remember(pid, finished, source, role):
        if type(pid) is not int or not 0 < pid <= 2_147_483_647:
            raise ValueError(f"{source}: invalid Windows PID {pid!r}")
        ticks, normalized = finish_utc(finished, source)
        item = records.setdefault(pid, {
            "pid": pid, "finishedUtc": normalized, "latestSource": source,
            "ticks": ticks, "sources": [],
        })
        item["sources"].append({"source": source, "role": role, "finished": finished, "finishedUtc": normalized})
        if ticks > item["ticks"]:
            item.update(ticks=ticks, finishedUtc=normalized, latestSource=source)

    def remember_array(values, finished, source, role):
        if not isinstance(values, list):
            raise ValueError(f"{source}: {role} must be a PID array")
        for pid in values:
            remember(pid, finished, source, role)

    def regression_check_ids(value, finished, source, path="checks"):
        # These fields are emitted by our pane/workspace verifiers. Do not infer
        # PIDs from numeric HWNDs, UUIDs, arbitrary strings or command arguments.
        if isinstance(value, dict):
            for key, child in value.items():
                role = f"{path}.{key}"
                if key == "pid":
                    remember(child, finished, source, role)
                elif key in ("stoppedPids", "survivorPids"):
                    remember_array(child, finished, source, role)
                elif isinstance(child, (dict, list)):
                    regression_check_ids(child, finished, source, role)
        elif isinstance(value, list):
            for index, child in enumerate(value):
                regression_check_ids(child, finished, source, f"{path}[{index}]")

    for path in sorted(diagnostics.glob("*.json")):
        source = path.relative_to(evidence).as_posix()
        if path.name == "manifest.json":
            continue
        # The inventory must not recursively add its own bounded runner PID.
        # A rerun checks the same stage processes plus newly collected work.
        if path.name.endswith("-runner.json") and "cleanup" in path.name:
            excluded.append(source)
            continue
        document = read(path)
        if path.name.endswith("-runner.json"):
            if not isinstance(document, dict):
                raise ValueError(f"{source}: runner must be an object")
            target = document.get("target")
            if isinstance(target, str) and WINDOWS_TARGET.match(target):
                # run-check.ps1 records a real Windows child process ID. Cargo/
                # npm run-check.py artifacts instead have a POSIX command list.
                remember(document.get("pid"), document.get("finished"), source, "windows-bounded-runner")
                included.append(source)
            elif isinstance(document.get("command"), list):
                excluded.append(source)
            else:
                raise ValueError(f"{source}: runner PID namespace is unknown")
            continue
        if not path.name.endswith("-native.json"):
            # Additional explicitly named entrypoint arrays are handled below.
            if "entrypoint" not in path.name:
                ignored.append(source)
            continue
        if not isinstance(document, dict):
            raise ValueError(f"{source}: native evidence must be an object")
        finished = document.get("finished")
        finish_utc(finished, source)  # Missing timestamps never silently skip PIDs.
        hosts = document.get("hosts", [])
        if not isinstance(hosts, list):
            raise ValueError(f"{source}: hosts must be an array")
        for index, host in enumerate(hosts):
            role = f"hosts[{index}]"
            if type(host) is int:
                remember(host, finished, source, role)
            elif isinstance(host, dict) and "pid" in host:
                remember(host["pid"], finished, source, role + ".pid")
            elif isinstance(host, dict) and {"first", "restored", "originalShells", "restoredShells"} <= host.keys():
                for key in ("first", "restored"):
                    remember(host[key], finished, source, role + "." + key)
                for key in ("originalShells", "restoredShells"):
                    remember_array(host[key], finished, source, role + "." + key)
            else:
                raise ValueError(f"{source}: unknown owned host schema")
        remember_array(document.get("shells", []), finished, source, "shells")
        # Only the explicit owned-client array is admitted; no numeric thread IDs,
        # HWNDs, command arguments or arbitrary observation fields become PIDs.
        remember_array(document.get("clientPids", []), finished, source, "clientPids")
        if path.name == "regression-browser-wait-native.json":
            remember_array(document.get("waitClientPids", []), finished, source, "waitClientPids")
        if path.name == "regression-ipc-limits-native.json":
            remember(document.get("pid"), finished, source, "ipc-limits-host")
            # This verifier serializes its single shell as a scalar; multiple
            # shells would form an array. Accept only these explicit PID shapes.
            shell_ids = document.get("shellPids", [])
            if type(shell_ids) is int:
                remember(shell_ids, finished, source, "shellPids")
            else:
                remember_array(shell_ids, finished, source, "shellPids")
        if path.name.startswith("regression-"):
            regression_check_ids(document.get("checks", []), finished, source)
        # Observations may repeat hosts/clients; arbitrary command-failure PID
        # fields are excluded. No external/browser renderer PID is inferred.
        included.append(source)

    # Bounded runner objects were already inventoried above; only the doctor
    # result arrays belong to this second schema.
    entrypoint_paths = {p for p in diagnostics.glob("*entrypoints*.json")
                        if not p.name.endswith("-runner.json")}
    entrypoint_paths.update(evidence.glob("native-editor-search*entrypoints*.json"))
    for path in sorted(entrypoint_paths):
        source = path.relative_to(evidence).as_posix()
        document = read(path)
        if not isinstance(document, list) or not document:
            raise ValueError(f"{source}: release entrypoints must be a nonempty array")
        for entry in document:
            if not isinstance(entry, dict):
                raise ValueError(f"{source}: invalid release entrypoint")
            remember(entry.get("pid"), entry.get("finishedUtc"), source, "release-entrypoint")
        included.append(source)
    if not records:
        raise ValueError("No owned Windows process IDs were recorded")

    owned = []
    for pid in sorted(records):
        item = records[pid]
        del item["ticks"]
        owned.append(item)
    manifest = {
        "generatedUtc": datetime.now(timezone.utc).isoformat(),
        "scope": "Recorded editor search/editor regression hosts/shells, explicit clientPids, Windows bounded runner IDs and copied stage release entrypoints; excludes POSIX and this inventory own cleanup runners",
        "limitations": [
            "Checks only recorded process IDs; older pane/state/workspace verifiers do not record every launched host or shell.",
            "WebView renderer descendants are not individually inventoried; native runner activeAfterCleanup job counts are separate evidence.",
            "No process is stopped or modified by this check.",
        ],
        "includedSources": included,
        "excludedRunnerSources": excluded,
        "ignoredWithoutProcessIds": ignored,
        "records": owned,
    }
    payload = json.dumps(manifest, ensure_ascii=True, indent=2)
    (dist / "editor-search-cleanup-metadata.json").write_text(payload + "\n", encoding="utf-8", newline="\n")
    script = r'''# SPDX-License-Identifier: GPL-3.0-or-later
# Generated by generate-editor-search-cleanup.py. Read-only: never stops a process.
# Invoke through windows/scripts/run-check.ps1 with a bounded deadline.
$ErrorActionPreference='Stop'
$OutputEncoding=[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$manifest=@'
__MANIFEST__
'@|ConvertFrom-Json
$output=Join-Path $PSScriptRoot '..\evidence\2026-09-28\native-editor-search-cleanup.json'
$observations=@(foreach($record in $manifest.records) {
    $ownedPid=[int]$record.pid
    $item=[ordered]@{
        pid=$ownedPid
        recordedFinishedUtc=$record.finishedUtc
        latestSource=$record.latestSource
        sources=$record.sources
        exists=$null
        name=$null
        startedUtc=$null
        startTimeMethod=$null
        startTimePrimaryError=$null
        verifiedReused=$false
        state='unverified'
        error=$null
    }
    $process=$null
    try {
        $finished=[DateTimeOffset]::Parse([string]$record.finishedUtc,[Globalization.CultureInfo]::InvariantCulture).UtcDateTime
        try {
            $process=Get-Process -Id $ownedPid -ErrorAction Stop
        } catch {
            if($_.FullyQualifiedErrorId -like 'NoProcessFoundForGivenId*') {
                $item.exists=$false
                $item.state='absent'
            } else { throw }
        }
        if($process) {
            $item.exists=$true
            $item.name=$process.ProcessName
            $started=$null
            try {
                $rawStart=$process.StartTime
                if($null -eq $rawStart){throw 'Get-Process.StartTime is unavailable'}
                $started=$rawStart.ToUniversalTime()
                $item.startTimeMethod='Get-Process.StartTime'
            } catch {
                $item.startTimePrimaryError=$_.Exception.Message
                # Read-only, PID-filtered fallback. Names never establish reuse.
                $item.startTimeMethod='Win32_Process.CreationDate'
                $cim=@(Get-CimInstance -ClassName Win32_Process -Filter ('ProcessId='+$ownedPid) -OperationTimeoutSec 3 -ErrorAction Stop)
                if($cim.Count -ne 1 -or [int]$cim[0].ProcessId -ne $ownedPid){throw 'CIM did not return exactly the requested process'}
                $created=$cim[0].CreationDate
                if($null -eq $created){throw 'CIM CreationDate is unavailable'}
                if($created -is [DateTimeOffset]) {
                    $started=$created.UtcDateTime
                } elseif($created -is [DateTime] -and $created.Kind -ne [DateTimeKind]::Unspecified) {
                    $started=$created.ToUniversalTime()
                } else {
                    throw 'CIM CreationDate has no explicit UTC/local time kind; refusing to guess'
                }
            }
            $item.startedUtc=$started.ToString('o')
            $item.verifiedReused=($started.Ticks -gt $finished.Ticks)
            $item.state=if($item.verifiedReused){'verifiedReused'}else{'possiblyAlive'}
        }
    } catch {
        $item.state='unverified'
        $item.error=$_.Exception.Message
    } finally {
        if($process){$process.Dispose()}
    }
    $item
})
$unverified=@($observations|Where-Object {$_.state -ne 'absent' -and $_.state -ne 'verifiedReused'})
$report=[ordered]@{
    status=if($unverified.Count){'failed'}else{'passed_read_only_process_cleanup'}
    checkedUtc=[DateTime]::UtcNow.ToString('o')
    generatedUtc=$manifest.generatedUtc
    scope=$manifest.scope
    limitations=$manifest.limitations
    readOnly=$true
    includedSources=$manifest.includedSources
    excludedRunnerSources=$manifest.excludedRunnerSources
    ignoredWithoutProcessIds=$manifest.ignoredWithoutProcessIds
    checkedPidCount=$observations.Count
    observations=$observations
}
$json=($report|ConvertTo-Json -Depth 10).Replace("`r`n","`n")+"`n"
[IO.File]::WriteAllText($output,$json,(New-Object Text.UTF8Encoding($false)))
Write-Output $json
if($unverified.Count){throw 'Recorded processes are possibly alive or could not be identified; no process was stopped'}
'''.replace("__MANIFEST__", payload)
    target = dist / "editor-search-cleanup.ps1"
    target.write_text(script, encoding="utf-8-sig", newline="\n")
    print(f"Generated {target}: {len(owned)} PIDs from {len(included)} sources; not executed")


if __name__ == "__main__":
    main()
