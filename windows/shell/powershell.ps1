# SPDX-License-Identifier: GPL-3.0-or-later
# Session-local only. Never edits a profile, execution policy or PSReadLine keys.
if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') { return }
if ($null -eq $function:prompt) { return }
if ((Test-Path variable:global:__FlowmuxPrompt) -and
    [object]::ReferenceEquals($function:prompt, $global:__FlowmuxPrompt)) { return }

try {
    if (-not (Test-Path variable:global:__FlowmuxOriginalPrompts)) { $global:__FlowmuxOriginalPrompts = @{} }
    $flowmuxPromptId = [guid]::NewGuid().ToString('N')
    $global:__FlowmuxOriginalPrompts[$flowmuxPromptId] = $function:prompt
    # A closure would capture automatic variables such as $pwd. A fixed template
    # and a generated identifier keep the original prompt in its normal scope.
    $flowmuxTemplate = @'

        # Run the original first, so it sees the prior command's $? and LASTEXITCODE.
        # Do not intercept PSConsoleHostReadLine or composition/key handling.
        $flowmuxText = & $global:__FlowmuxOriginalPrompts['%%FLOWMUX_PROMPT_ID%%']
        try {
            $flowmuxLocation = $ExecutionContext.SessionState.Path.CurrentLocation
            if ($flowmuxLocation.Provider.Name -eq 'FileSystem' -and $flowmuxLocation.ProviderPath -match '^[a-zA-Z]:[\\/]') {
                # Console.Write uses the shell's output code page. ASCII URI
                # encoding preserves Jamo/combining marks without changing it.
                $flowmuxUri = 'file:///' + [Uri]::EscapeDataString($flowmuxLocation.ProviderPath).Replace('%3A', ':').Replace('%5C', '/')
                [Console]::Write(([string][char]27) + ']7;' + $flowmuxUri + [char]7)
            }
        } catch { }
        return $flowmuxText
'@
    $flowmuxWrapper = [ScriptBlock]::Create($flowmuxTemplate.Replace('%%FLOWMUX_PROMPT_ID%%', $flowmuxPromptId))
    Set-Item -Path function:global:prompt -Value $flowmuxWrapper -ErrorAction Stop
    $global:__FlowmuxPrompt = $function:prompt
    Remove-Variable flowmuxPromptId, flowmuxTemplate, flowmuxWrapper -ErrorAction SilentlyContinue
} catch { }
