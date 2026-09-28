# SPDX-License-Identifier: GPL-3.0-or-later
# Pure evidence projection: no HWND, process, polling or application calls.
function Get-OptionsEvidenceSummary([string]$Name,$Status,[ValidateSet('options','keybindings')][string]$Mode){
    $options=$Status.options;$bindings=$options.keybindings
    $record=[ordered]@{schema='options-observation-v2';name=$Name;revision=$Status.document.revision;config_error=$Status.config_error;pending_writes=$Status.pending_writes;terminal=$Status.document.terminal;default_shell=$Status.document.default_shell}
    $record.options=[ordered]@{window=$options.window;owner=$options.owner;open=$options.open;page=$options.page;viewport=$options.viewport;scroll_offset=$options.scroll_offset;pending=$options.pending;queued=$options.queued;composing=$options.composing;error_or_status=$options.error_or_status}
    $record.acks=@($Status.surfaces|ForEach-Object {
        $ack=[ordered]@{surface=$_.surface;revision=$_.applied.revision;terminal=$_.applied.terminal;background=$_.applied.background;foreground=$_.applied.foreground;colors=$_.applied.colors}
        if($Mode -eq 'keybindings'){$ack.bindings=@($_.applied.bindings|Where-Object {$_.action -ceq 'new-surface'})}
        $ack
    })
    if($Mode -eq 'options'){
        $record.fields=@($options.controls|ForEach-Object {[ordered]@{key=$_.key;input=$_.input;parent=$_.parent;value=$_.value;baseline=$_.baseline;draft_error=$_.draft_error}})
    }else{
        $record.keybindings=[ordered]@{action_count=@($bindings.actions).Count;supported_count=@($bindings.actions|Where-Object {$_.supported}).Count;pending=$bindings.pending;queued=$bindings.queued;error=$bindings.error;action=@($bindings.actions|Where-Object {$_.action -ceq 'new-surface'});editor=$bindings.editor}
    }
    return $record
}
