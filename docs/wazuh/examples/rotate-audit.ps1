# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# The Polish text in this file is written without diacritics, unlike the rest of the documentation,
# and deliberately: Windows PowerShell 5.1 reads a .ps1 without a byte order mark as ANSI, so a
# UTF-8 file with Polish letters in it arrives mangled on exactly the machines this script is for.
# pwsh 7 assumes UTF-8 and would be fine; 5.1 is what a scheduled task tends to get.
#
# Tekst polski w tym pliku jest bez znakow diakrytycznych, inaczej niz reszta dokumentacji, i jest
# to swiadome: Windows PowerShell 5.1 czyta plik .ps1 bez znacznika BOM jako ANSI, wiec plik UTF-8 z
# polskimi literami dociera znieksztalcony na te maszyny, dla ktorych ten skrypt powstal.

<#
.SYNOPSIS
    EN - Rotates the Sentin-NPU audit trail on Windows by copy and truncate.
    PL - Rotuje slad audytowy Sentin-NPU na Windows metoda kopiuj i obcinaj.

.DESCRIPTION
    EN - The gateway appends to audit.jsonl and never rotates it, so on a busy machine the file
    grows without bound. Wazuh's log collector follows a truncation in place, which is why this
    copies the file aside and then sets its length to zero rather than renaming it. A rename and
    recreate also works, but the events written between the two are lost, and they are lost
    silently.

    The service is never stopped: the gateway holds the file open for append with sharing, so the
    truncation takes effect on a running gateway and the next event lands at offset zero.

    PL - Brama dopisuje do audit.jsonl i nigdy go nie rotuje, wiec na obciazonej maszynie plik
    rosnie bez ograniczen. Kolektor Wazuha sledzi obciecie pliku w miejscu i dlatego ten skrypt
    kopiuje plik obok, a nastepnie zeruje jego dlugosc, zamiast go przenosic. Przeniesienie i
    utworzenie na nowo takze dziala, ale zdarzenia zapisane pomiedzy jednym a drugim przepadaja -
    i przepadaja po cichu.

    Usluga nie jest zatrzymywana: brama trzyma plik otwarty do dopisywania w trybie wspoldzielenia,
    wiec obciecie dziala na pracujacej bramie, a kolejne zdarzenie trafia na pozycje zero.

    EN - measured, not assumed, because the distinction is invisible until it bites. The gateway
    opens the trail with Rust's append mode, which on Windows is FILE_APPEND_DATA: every write goes
    to the current end of file, so after this script truncates, the next event lands at offset zero
    and the file is 15 bytes, not 2015. A writer that merely SEEKS to the end when it opens - .NET's
    FileMode::Append is one - would instead write at its old position and leave a hole of NUL bytes
    the size of the old file, which the log collector reads as garbage. If you adapt this script for
    some other program's log, check which kind of writer it is first.

    PL - zmierzone, nie zalozone, bo ta roznica jest niewidoczna, dopoki nie zaboli. Brama otwiera
    slad w trybie dopisywania Rusta, czyli na Windows FILE_APPEND_DATA: kazdy zapis trafia na
    biezacy koniec pliku, wiec po obcieciu kolejne zdarzenie laduje na pozycji zero, a plik ma 15
    bajtow, nie 2015. Pisarz, ktory przy otwarciu jedynie PRZESKAKUJE na koniec - takim jest
    FileMode::Append z .NET - zapisze na starej pozycji i zostawi dziure z bajtow NUL wielkosci
    starego pliku, ktora kolektor przeczyta jako smieci. Adaptujac ten skrypt do logu innego
    programu, sprawdz najpierw, ktory to rodzaj pisarza.

.PARAMETER Path
    EN - The audit trail. Must match audit.jsonl.path in the gateway's config.yaml.
    PL - Slad audytowy. Musi zgadzac sie z audit.jsonl.path w config.yaml bramy.

.PARAMETER RetainDays
    EN - How long to keep rotated copies. Wazuh has already received the events; these copies are
    for the machine's own forensics, so match this to your local retention policy, not to the
    manager's.
    PL - Jak dlugo trzymac kopie. Wazuh juz otrzymal te zdarzenia, a kopie sluza dochodzeniu na
    samej maszynie - dopasuj ten czas do lokalnej polityki retencji, nie do polityki managera.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File rotate-audit.ps1

.EXAMPLE
    EN - register it as a daily task, running as SYSTEM so it can write in ProgramData:
    PL - rejestracja jako zadanie codzienne, uruchamiane jako SYSTEM, zeby moglo pisac w ProgramData:

    $action  = New-ScheduledTaskAction -Execute 'powershell.exe' `
        -Argument '-NoProfile -ExecutionPolicy Bypass -File "C:\ProgramData\Sentin-NPU\rotate-audit.ps1"'
    $trigger = New-ScheduledTaskTrigger -Daily -At 3am
    Register-ScheduledTask -TaskName 'Sentin-NPU audit rotation' -Action $action -Trigger $trigger `
        -User 'SYSTEM' -RunLevel Highest
#>

[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string] $Path = 'C:\ProgramData\Sentin-NPU\audit.jsonl',
    [int]    $MinimumBytes = 10MB,
    [int]    $RetainDays = 30
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Path)) {
    # EN - not an error worth failing a scheduled task over: a gateway that has found nothing has
    # written nothing. It is worth saying, though, because it is also what a wrong path looks like.
    # PL - to nie blad, ktory powinien wywracac zadanie: brama, ktora nic nie znalazla, nic nie
    # zapisala. Warto to jednak powiedziec, bo tak samo wyglada bledna sciezka.
    Write-Warning "No audit trail at $Path - nothing to rotate."
    return
}

$file = Get-Item -LiteralPath $Path
if ($file.Length -lt $MinimumBytes) {
    Write-Verbose "$Path is $($file.Length) bytes, below the $MinimumBytes threshold - leaving it."
    return
}

$stamp  = Get-Date -Format 'yyyyMMdd-HHmmss'
$copy   = Join-Path $file.DirectoryName ("{0}.{1}" -f $file.Name, $stamp)

if ($PSCmdlet.ShouldProcess($Path, "copy to $copy and truncate")) {
    Copy-Item -LiteralPath $Path -Destination $copy

    # EN - FileShare::ReadWrite is the whole point. Opening without it fails while the gateway holds
    # the file, and the failure reads as a permissions problem rather than as a sharing one.
    # PL - FileShare::ReadWrite jest tu sednem. Otwarcie bez tego nie powiedzie sie, gdy plik trzyma
    # brama, a komunikat bedzie wygladal na problem z uprawnieniami, nie ze wspoldzieleniem.
    $stream = [System.IO.File]::Open(
        $Path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::ReadWrite)
    try { $stream.SetLength(0) } finally { $stream.Dispose() }

    Write-Output "Rotated $Path to $copy ($($file.Length) bytes)."
}

Get-ChildItem -LiteralPath $file.DirectoryName -Filter "$($file.Name).*" |
    Where-Object { $_.LastWriteTime -lt (Get-Date).AddDays(-$RetainDays) } |
    ForEach-Object {
        if ($PSCmdlet.ShouldProcess($_.FullName, 'delete')) {
            Remove-Item -LiteralPath $_.FullName
            Write-Output "Removed $($_.Name), older than $RetainDays days."
        }
    }
