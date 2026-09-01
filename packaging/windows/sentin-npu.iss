; Copyright 2026 Grzegorz Oleksy
; SPDX-License-Identifier: Apache-2.0
;
; Inno Setup script for the Sentin-NPU gateway.
;
; What it produces: one .exe carrying the gateway, the diagnostics, the latency harness, the
; OpenVINO runtime, the quantized model and the Wazuh integration. Nothing is downloaded at install
; time and nothing else has to be present on the machine - no Rust, no Python, no OpenVINO.
;
; The wizard asks the questions whose wrong answers are silent: which port, which address, where
; the audit trail goes, which upstreams, and whether to run as a service. Every answer is written
; into config.yaml as an ABSOLUTE path where a path is involved, because a relative model_dir
; resolves against the service's working directory and drops the gateway to layer 1 without saying
; so - the single trap this project has hit most often.
;
; Build:  iscc /DVersion=0.2.0 /DPayload=..\..\dist\sentin-npu-diag-0.2.0-windows-x64 sentin-npu.iss

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Payload
  #define Payload "..\..\dist\payload"
#endif

#define AppName "Sentin-NPU"
#define Publisher "Grzegorz Oleksy"
#define AppUrl "https://github.com/GrzegorzOle/Sentin-NPU"
#define ServiceName "SentinNPU"

[Setup]
AppId={{7C4B2E19-2F6D-4E2A-9E71-3C8A5D1B0F42}
AppName={#AppName}
AppVersion={#Version}
AppPublisher={#Publisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
LicenseFile={#Payload}\..\..\LICENSE
OutputDir=.\out
OutputBaseFilename=sentin-npu-setup-{#Version}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; The service must be installed by an administrator, and so must a program in Program Files.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\sentin-gateway.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "polish"; MessagesFile: "compiler:Languages\Polish.isl"

[Types]
Name: "full"; Description: "Gateway, diagnostics and the SIEM integration"
Name: "gateway"; Description: "Gateway only"
Name: "custom"; Description: "Custom"; Flags: iscustom

[Components]
Name: "gateway"; Description: "Gateway, OpenVINO runtime and the NER model"; Types: full gateway custom; Flags: fixed
Name: "tools"; Description: "Diagnostics (sentin-doctor) and latency harness (sentin-bench)"; Types: full
Name: "wazuh"; Description: "Wazuh rules, dashboard and deployment guide"; Types: full
Name: "docs"; Description: "Documentation: installation, configuration, the audit event schema"; Types: full gateway custom; Flags: fixed

[Files]
Source: "{#Payload}\sentin-gateway.exe"; DestDir: "{app}"; Components: gateway; Flags: ignoreversion
; The OpenVINO runtime goes in lib\, and nothing here puts it on any search path: the binaries add
; their own lib\ directory themselves at startup. That is deliberate. Windows searches the
; executable's directory and PATH, neither of which is lib\, so the first release of this installer
; produced a service that ran with layer 2 silently missing - and the alternative fix, editing the
; machine's PATH, changes the whole system to solve one program's problem and has to be undone on
; uninstall to avoid leaving a dangling entry.
Source: "{#Payload}\lib\*"; DestDir: "{app}\lib"; Components: gateway; Flags: ignoreversion recursesubdirs
Source: "{#Payload}\models\*"; DestDir: "{app}\models"; Components: gateway; Flags: ignoreversion recursesubdirs
Source: "{#Payload}\sentin-doctor.exe"; DestDir: "{app}"; Components: tools; Flags: ignoreversion
Source: "{#Payload}\sentin-bench.exe"; DestDir: "{app}"; Components: tools; Flags: ignoreversion
Source: "{#Payload}\wazuh\*"; DestDir: "{app}\wazuh"; Components: wazuh; Flags: ignoreversion recursesubdirs
Source: "{#Payload}\docs\*"; DestDir: "{app}\docs"; Components: docs; Flags: ignoreversion recursesubdirs
Source: "{#Payload}\README.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme

[Dirs]
; Configuration and the audit trail live outside Program Files: they are data, they are rewritten
; at run time, and an uninstall must not take an operator's audit history with it.
Name: "{commonappdata}\{#AppName}"; Permissions: users-modify

[Icons]
Name: "{group}\Sentin-NPU configuration"; Filename: "notepad.exe"; Parameters: """{commonappdata}\{#AppName}\config.yaml"""
; The service has no console, so this file is the only place its startup is visible - and the one
; line worth finding in it is whether layer 2 loaded.
Name: "{group}\Service log"; Filename: "notepad.exe"; Parameters: """{commonappdata}\{#AppName}\sentin-gateway.log"""
Name: "{group}\Device report (sentin-doctor)"; Filename: "{app}\sentin-doctor.exe"; Components: tools
Name: "{group}\Deployment guide"; Filename: "{app}\wazuh\README.md"; Components: wazuh
Name: "{group}\Installation and configuration"; Filename: "{app}\docs\install-windows.md"; Components: docs
Name: "{group}\Audit event schema"; Filename: "{app}\docs\events.md"; Components: docs
Name: "{group}\Documentation"; Filename: "{app}\docs"; Components: docs

; Registering and starting the service happens in [Code] rather than here. A [Run] entry cannot
; check what it did, and an installer that reports success while the service it just registered
; failed to start is the exact failure this project exists to complain about.
[Run]
Filename: "{app}\sentin-doctor.exe"; Description: "Show what this machine can run the model on"; Flags: postinstall skipifsilent nowait; Components: tools

[UninstallRun]
Filename: "{app}\sentin-gateway.exe"; Parameters: "--uninstall-service"; Flags: runhidden; RunOnceId: "RemoveService"

[Code]
var
  ConfigPage: TInputQueryWizardPage;
  ServicePage: TInputOptionWizardPage;
  UpstreamPage: TInputQueryWizardPage;

function WantsService: Boolean;
begin
  Result := ServicePage.Values[0];
end;

function WantsStartNow: Boolean;
begin
  { Both, in one function: a Check parameter takes a single call rather than an expression, and
    starting a service that was never registered would fail in a way nobody could act on. }
  Result := ServicePage.Values[0] and ServicePage.Values[1];
end;

procedure InitializeWizard;
begin
  ConfigPage := CreateInputQueryPage(wpSelectComponents,
    'Gateway configuration',
    'Where the gateway listens, and where its audit trail goes.',
    'These become config.yaml. You can edit that file afterwards; the service reads it at start.');
  ConfigPage.Add('Listen port:', False);
  ConfigPage.Add('Bind address:', False);
  ConfigPage.Add('Audit file:', False);
  { 4141 rather than the more obvious 4000: a model router such as LiteLLM commonly holds 4000,
    and a gateway that will not start because its port is taken is a poor first impression. }
  ConfigPage.Values[0] := '4141';
  { Loopback by default. Anything else exposes an inspection point to the network, which is a
    decision an operator should have to make on purpose - but containers on this machine reach the
    host by its LAN address, so the field is here rather than hidden. }
  ConfigPage.Values[1] := '127.0.0.1';
  ConfigPage.Values[2] := ExpandConstant('{commonappdata}\{#AppName}\audit.jsonl');

  UpstreamPage := CreateInputQueryPage(ConfigPage.ID,
    'Where requests go',
    'The upstream for each provider prefix.',
    'Callers point at http://<this machine>:<port>/anthropic, /openai or /google. Leave the '
    + 'defaults unless you route through a local model router.');
  UpstreamPage.Add('/anthropic ->', False);
  UpstreamPage.Add('/openai ->', False);
  UpstreamPage.Add('/google ->', False);
  UpstreamPage.Values[0] := 'https://api.anthropic.com';
  UpstreamPage.Values[1] := 'http://localhost:4000';
  UpstreamPage.Values[2] := 'https://generativelanguage.googleapis.com';

  ServicePage := CreateInputOptionPage(UpstreamPage.ID,
    'Windows service',
    'Run the gateway without anyone having to remember to start it.',
    'A gateway that is started by hand is a gateway that is sometimes not running, and the traffic '
    + 'it misses is exactly the traffic nobody inspected.', False, False);
  ServicePage.Add('Install as a Windows service, started automatically at boot');
  ServicePage.Add('Start it when this installer finishes');
  ServicePage.Values[0] := True;
  ServicePage.Values[1] := True;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Port: Integer;
begin
  Result := True;
  if CurPageID = ConfigPage.ID then
  begin
    Port := StrToIntDef(ConfigPage.Values[0], -1);
    if (Port < 1) or (Port > 65535) then
    begin
      MsgBox('The listen port must be a number between 1 and 65535.', mbError, MB_OK);
      Result := False;
      Exit;
    end;
    if Trim(ConfigPage.Values[1]) = '' then
    begin
      MsgBox('The bind address cannot be empty. Use 127.0.0.1 for this machine only, or 0.0.0.0 '
        + 'to accept connections from the network.', mbError, MB_OK);
      Result := False;
      Exit;
    end;
    if Trim(ConfigPage.Values[2]) = '' then
    begin
      MsgBox('The audit file path cannot be empty.', mbError, MB_OK);
      Result := False;
    end;
  end;
end;

{ Escape a Windows path for a double-quoted YAML scalar: backslashes double, quotes escape.
  Writing C:\ProgramData\... unquoted into YAML is valid until a path contains a colon or a
  leading character YAML treats specially, and then the gateway fails to start with a parse error
  nobody connects to the installer. }
function YamlQuote(const Value: String): String;
var
  Escaped: String;
begin
  Escaped := Value;
  StringChangeEx(Escaped, '\', '\\', True);
  StringChangeEx(Escaped, '"', '\"', True);
  Result := '"' + Escaped + '"';
end;

{ Append one line, growing the array as needed.

  This replaced a fixed SetArrayLength and hand-written indices, which is not a matter of taste: a
  detector added to config/default.yaml and forgotten here produced an installed configuration
  missing it, and a detector nobody lists only observes - so the gateway found the identifier and
  forwarded it. That happened with vat_eu in 0.2.0. Renumbering ten lines by hand to insert one is
  the kind of edit that gets skipped, so the numbering is gone. }
procedure AddLine(var Lines: TArrayOfString; var Count: Integer; const Value: String);
begin
  if Count >= GetArrayLength(Lines) then
    SetArrayLength(Lines, Count + 16);
  Lines[Count] := Value;
  Count := Count + 1;
end;

procedure WriteConfig;
var
  Lines: TArrayOfString;
  Path: String;
  N: Integer;
begin
  Path := ExpandConstant('{commonappdata}\{#AppName}\config.yaml');
  N := 0;

  AddLine(Lines, N, '# Sentin-NPU gateway configuration, written by the installer.');
  AddLine(Lines, N, '# Edit freely; the service reads this file when it starts.');
  AddLine(Lines, N, '# Field reference: https://github.com/GrzegorzOle/Sentin-NPU');
  AddLine(Lines, N, '');
  AddLine(Lines, N, 'listen:');
  AddLine(Lines, N, '  host: ' + ConfigPage.Values[1]);
  AddLine(Lines, N, '  port: ' + ConfigPage.Values[0]);
  AddLine(Lines, N, '');
  AddLine(Lines, N, 'providers:');
  AddLine(Lines, N, '  anthropic:');
  AddLine(Lines, N, '    prefix: /anthropic');
  AddLine(Lines, N, '    upstream: ' + UpstreamPage.Values[0]);
  AddLine(Lines, N, '  openai:');
  AddLine(Lines, N, '    prefix: /openai');
  AddLine(Lines, N, '    upstream: ' + UpstreamPage.Values[1]);
  AddLine(Lines, N, '  google:');
  AddLine(Lines, N, '    prefix: /google');
  AddLine(Lines, N, '    upstream: ' + UpstreamPage.Values[2]);
  AddLine(Lines, N, '');
  AddLine(Lines, N, 'inference:');
  AddLine(Lines, N, '  # AUTO times every device OpenVINO enumerates and prefers the cheapest that');
  AddLine(Lines, N, '  # holds the budget. Pin NPU, GPU or CPU here to skip the probe.');
  AddLine(Lines, N, '  device: AUTO');
  AddLine(Lines, N, '  select: cost');
  AddLine(Lines, N, '  max_inference_ms: 80');
  AddLine(Lines, N, '  # Absolute, and that matters: a relative path resolves against the service''s');
  AddLine(Lines, N, '  # working directory, and layer 2 would go missing with only a warning.');
  AddLine(Lines, N, '  model_dir: ' + YamlQuote(ExpandConstant('{app}\models\seq128')));
  AddLine(Lines, N, '  timeout_ms: 250');
  AddLine(Lines, N, '  timeout_policy: fail_open');
  AddLine(Lines, N, '');

  { Keep this list in step with config/default.yaml. A detector present in the code and absent from
    the configuration defaults to observing, which is deliberate - code must not start rewriting
    traffic on its own - and which means an omission here is invisible: the identifier is detected,
    reported, and forwarded anyway. }
  AddLine(Lines, N, 'detectors:');
  AddLine(Lines, N, '  pesel:        { layer: deterministic, mode: block }');
  AddLine(Lines, N, '  nip:          { layer: deterministic, mode: mask }');
  AddLine(Lines, N, '  vat_eu:       { layer: deterministic, mode: mask }');
  AddLine(Lines, N, '  regon:        { layer: deterministic, mode: mask }');
  AddLine(Lines, N, '  iban:         { layer: deterministic, mode: block }');
  AddLine(Lines, N, '  payment_card: { layer: deterministic, mode: block }');
  AddLine(Lines, N, '  email:        { layer: deterministic, mode: advise }');
  AddLine(Lines, N, '  phone_pl:     { layer: deterministic, mode: advise }');
  AddLine(Lines, N, '  person:       { layer: ner, mode: advise }');
  AddLine(Lines, N, '  organization: { layer: ner, mode: advise }');
  AddLine(Lines, N, '  location:     { layer: ner, mode: observe }');
  AddLine(Lines, N, '');

  AddLine(Lines, N, 'audit:' + #13#10 + '  jsonl:' + #13#10 + '    enabled: true' + #13#10
             + '    path: ' + YamlQuote(ConfigPage.Values[2]) + #13#10
             + '  syslog_cef:' + #13#10 + '    enabled: false' + #13#10
             + '    address: 127.0.0.1:514' + #13#10
             + '  otlp:' + #13#10 + '    enabled: false' + #13#10
             + '    # OTLP over HTTP with JSON encoding: the collector''s HTTP port, not gRPC.'
             + #13#10 + '    endpoint: http://localhost:4318');

  { Trim the slack the last growth left, or the file ends with a run of blank lines. }
  SetArrayLength(Lines, N);

  { An existing configuration is never overwritten. An upgrade must not silently discard the
    detector policy a site tuned, and a config.yaml is the one file here that is theirs. }
  if FileExists(Path) then
  begin
    SaveStringsToFile(Path + '.new', Lines, False);
    MsgBox('A configuration already exists and was kept.' + #13#10 + #13#10
      + 'The answers from this wizard were written to:' + #13#10
      + Path + '.new', mbInformation, MB_OK);
  end
  else
    SaveStringsToFile(Path, Lines, False);
end;

{ Is anything already listening on that port? findstr exits 0 when it matches, which is the only
  answer needed. This exists because the port is the one thing an installer cannot fix for you:
  the gateway may already be running from a source build or an unpacked bundle, and a service that
  cannot bind fails in a way nobody sees. }
function PortIsBusy(const Port: String): Boolean;
var
  ResultCode: Integer;
begin
  Result := Exec(ExpandConstant('{cmd}'),
                 '/c netstat -ano -p tcp | findstr LISTENING | findstr /R /C:":' + Port + ' "',
                 '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
end;

function ServiceIsRunning: Boolean;
var
  ResultCode: Integer;
begin
  Result := Exec(ExpandConstant('{cmd}'),
                 '/c sc query {#ServiceName} | findstr RUNNING',
                 '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
end;

function LogPath: String;
begin
  Result := ExpandConstant('{commonappdata}\{#AppName}\sentin-gateway.log');
end;

{ A service that is running is not the same thing as a gateway that is inspecting.

  Layer 2 needs the OpenVINO runtime and the NER model, and when it cannot have them the gateway
  keeps serving layer 1 and records that in its log - a design decision, because a gateway in front
  of somebody's real work should degrade rather than refuse. The cost is that the failure is
  invisible: the service is RUNNING, the port answers, requests succeed, and only the entity
  detection is missing. This installer's first release shipped exactly that, and it was found days
  later by noticing a field absent from an audit event.

  So the last thing the installer does is read the log the gateway has just written and say which
  of the two it got. }
{ Does the log carry this line yet?

  findstr rather than Inno's own LoadStringFromFile, which opens a file without sharing and so
  fails for as long as anything holds it open for writing - which, by the time this runs, is the
  service that has just been started. The first version of this check used it and reported "the
  gateway did not say" on a machine whose log said "layer 2 ready" in plain text. }
function LogContains(const Needle: String): Boolean;
var
  ResultCode: Integer;
begin
  Result := Exec(ExpandConstant('{cmd}'),
                 '/c findstr /c:"' + Needle + '" "' + LogPath() + '"',
                 '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
end;

procedure CheckSecondLayer;
var
  Waited: Integer;
  Ready, Unavailable: Boolean;
begin
  Ready := False;
  Unavailable := False;
  { Up to twenty seconds: on a first start the device probe compiles the model on every device
    OpenVINO enumerates, which on a machine with a discrete GPU is genuinely slow. }
  Waited := 0;
  while Waited < 40 do
  begin
    Ready := LogContains('layer 2 ready');
    Unavailable := LogContains('layer 2 unavailable');
    if Ready or Unavailable then
      Break;
    Sleep(500);
    Waited := Waited + 1;
  end;

  if Unavailable then
    MsgBox('The gateway is running, but only its first layer is.' + #13#10 + #13#10
      + 'Checksum detection - PESEL, NIP, REGON, IBAN, payment cards - works. Named entity '
      + 'detection does not, so people, organisations and places will not be found.' + #13#10 + #13#10
      + 'The reason is in the log:' + #13#10 + LogPath(), mbError, MB_OK)
  else if not Ready then
    MsgBox('The gateway is running, but it did not report whether its second layer loaded.'
      + #13#10 + #13#10
      + 'Look for "layer 2 ready" or "layer 2 unavailable" in:' + #13#10 + LogPath(),
      mbInformation, MB_OK);
end;

procedure SetUpService;
var
  ResultCode, Waited: Integer;
begin
  if not Exec(ExpandConstant('{app}\sentin-gateway.exe'),
              '--install-service "' + ExpandConstant('{commonappdata}\{#AppName}\config.yaml') + '"',
              '', SW_HIDE, ewWaitUntilTerminated, ResultCode) or (ResultCode <> 0) then
  begin
    MsgBox('The Windows service could not be registered.' + #13#10 + #13#10
      + 'Everything else is installed. You can register it later from an elevated prompt:'
      + #13#10 + #13#10
      + '  "' + ExpandConstant('{app}\sentin-gateway.exe') + '" --install-service '
      + '"' + ExpandConstant('{commonappdata}\{#AppName}\config.yaml') + '"',
      mbError, MB_OK);
    Exit;
  end;

  if not WantsStartNow then
    Exit;

  { Move the previous log aside before starting, so that what CheckSecondLayer reads describes this
    start and no other. The gateway appends across restarts, so an upgrade over a working
    installation would otherwise find last week's "layer 2 ready" and report success for a start
    that had just failed - a false pass, which is worse than the false warning it replaces. The
    service is stopped at this point, so nothing holds the file. }
  DeleteFile(LogPath() + '.1');
  RenameFile(LogPath(), LogPath() + '.1');

  Exec(ExpandConstant('{sys}\sc.exe'), 'start {#ServiceName}', '', SW_HIDE,
       ewWaitUntilTerminated, ResultCode);

  { A service that fails to bind exits almost at once, so a short wait tells the truth. }
  Waited := 0;
  while (Waited < 10) and (not ServiceIsRunning) do
  begin
    Sleep(700);
    Waited := Waited + 1;
  end;

  if not ServiceIsRunning then
  begin
    MsgBox('The service was registered but is not running.' + #13#10 + #13#10
      + 'The usual cause is that something else already holds port '
      + ConfigPage.Values[0] + ' - often the gateway itself, started by hand or from an '
      + 'unpacked bundle. Stop it and then:' + #13#10 + #13#10
      + '  sc start {#ServiceName}' + #13#10 + #13#10
      + 'The configuration it uses is ' + ExpandConstant('{commonappdata}\{#AppName}\config.yaml')
      + #13#10 + 'and it logs to ' + LogPath(),
      mbError, MB_OK);
    Exit;
  end;

  CheckSecondLayer;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    WriteConfig;
    if WantsService then
      SetUpService;
  end;
end;

{ The service holds the executable open, so an upgrade over a running service fails on a file in
  use. Stopping it first is the difference between an upgrade and a support call. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Result := '';
  Exec(ExpandConstant('{sys}\sc.exe'), 'stop {#ServiceName}', '', SW_HIDE,
       ewWaitUntilTerminated, ResultCode);
  Sleep(1500);

  { That stops the service. It does nothing about a gateway someone started by hand from a source
    build or an unpacked bundle, which is a perfectly normal thing to have running while installing
    - and the new service would then be unable to bind. Killing a process this installer did not
    start would be presumptuous, so it says so and lets the operator decide. }
  if WantsService and PortIsBusy(ConfigPage.Values[0]) then
  begin
    if MsgBox('Something is already listening on port ' + ConfigPage.Values[0] + '.'
      + #13#10 + #13#10
      + 'If that is the gateway running from a source build or an unpacked bundle, stop it first '
      + 'or the service will not be able to start.' + #13#10 + #13#10
      + 'Continue anyway?', mbConfirmation, MB_YESNO) = IDNO then
      Result := 'Installation cancelled: port ' + ConfigPage.Values[0] + ' is in use.';
  end;
end;
