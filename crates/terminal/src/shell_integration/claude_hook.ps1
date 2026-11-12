# Solo: a Claude Code hook that tells the terminal what Claude is doing, with
# OSC 6973 ; agent ; nonce=<the terminal's nonce> ; <kind> ; <ms since the epoch the hook started at> ;
# <session id>. The terminal ignores reports without its nonce (SOLO_SHELL_NONCE).
#
# Claude Code may start hooks in a hidden console of their own, so this attaches
# to the console of the shell Claude was started from (SOLO_AGENT_CONSOLE_PID),
# or else Claude's own, before writing.

$ErrorActionPreference = 'Stop'
try {
    $kind = "$($args[0])"
    # Hooks run in the background and may finish out of order; the terminal
    # orders them by when they started.
    $startedAt = [DateTimeOffset]::new((Get-Process -Id $PID).StartTime).ToUnixTimeMilliseconds()
    $sessionId = ''
    try {
        $candidate = "$(([Console]::In.ReadToEnd() | ConvertFrom-Json).session_id)"
        if ($candidate -match '^[0-9A-Fa-f-]+$') { $sessionId = $candidate }
    } catch {}

    $assembly = [AppDomain]::CurrentDomain.DefineDynamicAssembly([Reflection.AssemblyName]::new('SoloClaudeHook'), 'Run')
    $type = $assembly.DefineDynamicModule('SoloClaudeHook').DefineType('SoloConsole', 'Public, Class')
    foreach ($signature in @(
        @('FreeConsole', [bool], [Type[]]@()),
        @('AttachConsole', [bool], [Type[]]@([uint32])),
        @('CreateFileW', [IntPtr], [Type[]]@([string], [uint32], [uint32], [IntPtr], [uint32], [uint32], [IntPtr]))
    )) {
        $method = $type.DefinePInvokeMethod($signature[0], 'kernel32.dll', 'Public, Static, PinvokeImpl', 'Standard', $signature[1], $signature[2], 'Winapi', 'Unicode')
        $method.SetImplementationFlags('PreserveSig')
    }
    $console = $type.CreateType()

    $attachParent = [uint32]::MaxValue
    $shell = [uint32]0
    [void]$console::FreeConsole()
    if (-not ([uint32]::TryParse("$env:SOLO_AGENT_CONSOLE_PID", [ref]$shell) -and $console::AttachConsole($shell))) {
        if (-not $console::AttachConsole($attachParent)) { exit 0 }
    }
    # GENERIC_WRITE, shared for reading and writing, OPEN_EXISTING.
    $handle = $console::CreateFileW('CONOUT$', 0x40000000, 3, [IntPtr]::Zero, 3, 0, [IntPtr]::Zero)
    if ($handle -eq [IntPtr]::Zero -or $handle -eq [IntPtr]-1) { exit 0 }
    $output = [IO.FileStream]::new([Microsoft.Win32.SafeHandles.SafeFileHandle]::new($handle, $true), 'Write')
    $nonce = "$env:SOLO_SHELL_NONCE"
    if ($nonce -notmatch '^[0-9A-Za-z]*$') { $nonce = '' }
    $bytes = [Text.Encoding]::ASCII.GetBytes("$([char]27)]6973;agent;nonce=$nonce;$kind;$startedAt;$sessionId$([char]7)")
    $output.Write($bytes, 0, $bytes.Length)
    $output.Dispose()
} catch {
    # A status the terminal misses isn't worth bothering Claude Code about.
}
exit 0
