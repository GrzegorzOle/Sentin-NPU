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
; Build:  iscc /DVersion=0.0.0.10 /DPayload=..\..\dist\sentin-npu-diag-0.0.0.10-windows-x64 sentin-npu.iss

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
; ~280 MB of payload; saying so up front is politer than a progress bar that stalls.
DiskSpaceCalculation=yes

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

[Files]
Source: "{#Payload}\sentin-gateway.exe"; DestDir: "{app}"; Components: gateway; Flags: ignoreversion
Source: "{#Payload}\lib\*"; DestDir: "{app}\lib"; Components: gateway; Flags: ignoreversion recursesubdirs
Source: "{#Payload}\models\*"; DestDir: "{app}\models"; Components: gateway; Flags: ignoreversion recursesubdirs
Source: "{#Payload}\sentin-doctor.exe"; DestDir: "{app}"; Components: tools; Flags: ignoreversion
Source: "{#Payload}\sentin-bench.exe"; DestDir: "{app}"; Components: tools; Flags: ignoreversion
Source: "{#Payload}\wazuh\*"; DestDir: "{app}\wazuh"; Components: wazuh; Flags: ignoreversion recursesubdirs
Source: "{#Payload}\README.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme

[Dirs]
; Configuration and the audit trail live outside Program Files: they are data, they are rewritten
; at run time, and an uninstall must not take an operator's audit history with it.
Name: "{commonappdata}\{#AppName}"; Permissions: users-modify

[Icons]
Name: "{group}\Sentin-NPU configuration"; Filename: "notepad.exe"; Parameters: """{commonappdata}\{#AppName}\config.yaml"""
Name: "{group}\Device report (sentin-doctor)"; Filename: "{app}\sentin-doctor.exe"; Components: tools
Name: "{group}\Deployment guide"; Filename: "{app}\wazuh\README.md"; Components: wazuh

[Run]
Filename: "{app}\sentin-gateway.exe"; Parameters: "--install-service ""{commonappdata}\{#AppName}\config.yaml"""; StatusMsg: "Registering the Windows service..."; Flags: runhidden; Check: WantsService
Filename: "{sys}\sc.exe"; Parameters: "start {#ServiceName}"; StatusMsg: "Starting the gateway..."; Flags: runhidden; Check: WantsService and WantsStartNow
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
  Result := ServicePage.Values[1];
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

procedure WriteConfig;
var
  Lines: TArrayOfString;
  Path: String;
begin
  Path := ExpandConstant('{commonappdata}\{#AppName}\config.yaml');

  SetArrayLength(Lines, 44);
  Lines[0]  := '# Sentin-NPU gateway configuration, written by the installer.';
  Lines[1]  := '# Edit freely; the service reads this file when it starts.';
  Lines[2]  := '# Field reference: https://github.com/GrzegorzOle/Sentin-NPU';
  Lines[3]  := '';
  Lines[4]  := 'listen:';
  Lines[5]  := '  host: ' + ConfigPage.Values[1];
  Lines[6]  := '  port: ' + ConfigPage.Values[0];
  Lines[7]  := '';
  Lines[8]  := 'providers:';
  Lines[9]  := '  anthropic:';
  Lines[10] := '    prefix: /anthropic';
  Lines[11] := '    upstream: ' + UpstreamPage.Values[0];
  Lines[12] := '  openai:';
  Lines[13] := '    prefix: /openai';
  Lines[14] := '    upstream: ' + UpstreamPage.Values[1];
  Lines[15] := '  google:';
  Lines[16] := '    prefix: /google';
  Lines[17] := '    upstream: ' + UpstreamPage.Values[2];
  Lines[18] := '';
  Lines[19] := 'inference:';
  Lines[20] := '  # AUTO times every device OpenVINO enumerates and prefers the cheapest that';
  Lines[21] := '  # holds the budget. Pin NPU, GPU or CPU here to skip the probe.';
  Lines[22] := '  device: AUTO';
  Lines[23] := '  select: cost';
  Lines[24] := '  max_inference_ms: 80';
  Lines[25] := '  # Absolute, and that matters: a relative path resolves against the service''s';
  Lines[26] := '  # working directory, and layer 2 would go missing with only a warning.';
  Lines[27] := '  model_dir: ' + YamlQuote(ExpandConstant('{app}\models\seq128'));
  Lines[28] := '  timeout_ms: 250';
  Lines[29] := '  timeout_policy: fail_open';
  Lines[30] := '';
  Lines[31] := 'detectors:';
  Lines[32] := '  pesel:        { layer: deterministic, mode: block }';
  Lines[33] := '  nip:          { layer: deterministic, mode: mask }';
  Lines[34] := '  regon:        { layer: deterministic, mode: mask }';
  Lines[35] := '  iban:         { layer: deterministic, mode: block }';
  Lines[36] := '  payment_card: { layer: deterministic, mode: block }';
  Lines[37] := '  email:        { layer: deterministic, mode: advise }';
  Lines[38] := '  phone_pl:     { layer: deterministic, mode: advise }';
  Lines[39] := '  person:       { layer: ner, mode: advise }';
  Lines[40] := '  organization: { layer: ner, mode: advise }';
  Lines[41] := '  location:     { layer: ner, mode: observe }';
  Lines[42] := '';
  Lines[43] := 'audit:' + #13#10 + '  jsonl:' + #13#10 + '    enabled: true' + #13#10
             + '    path: ' + YamlQuote(ConfigPage.Values[2]) + #13#10
             + '  syslog_cef:' + #13#10 + '    enabled: false' + #13#10
             + '    address: 127.0.0.1:514' + #13#10
             + '  otlp:' + #13#10 + '    enabled: false' + #13#10
             + '    # OTLP over HTTP with JSON encoding: the collector''s HTTP port, not gRPC.'
             + #13#10 + '    endpoint: http://localhost:4318';

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

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    WriteConfig;
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
end;
