#ifndef AppVersion
  #error AppVersion must be supplied by scripts/build-installer.ps1
#endif
#ifndef SourceExe
  #error SourceExe must be supplied by scripts/build-installer.ps1
#endif
#ifndef OutputDir
  #error OutputDir must be supplied by scripts/build-installer.ps1
#endif

[Setup]
AppId={{C7F49A12-769A-4EA4-B1B0-56AB5816191A}
AppName=P2P - Voz e tela
AppVersion={#AppVersion}
AppPublisher=BadTonho
DefaultDirName={localappdata}\Programs\P2P-Voz-e-tela
; Always show the destination page, including when upgrading an existing install.
DisableDirPage=no
DefaultGroupName=P2P - Voz e tela
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#OutputDir}
OutputBaseFilename=P2P-Voz-e-tela-Setup
UninstallDisplayIcon={app}\p2p-client.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\crates\p2p-client\assets\p2p-client.ico
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes
RestartApplications=no

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "p2p-client.exe"; Flags: ignoreversion

[Icons]
Name: "{group}\P2P - Voz e tela"; Filename: "{app}\p2p-client.exe"

[Run]
Filename: "{app}\p2p-client.exe"; Description: "Abrir P2P - Voz e tela"; Flags: postinstall nowait skipifsilent

[UninstallDelete]
Type: files; Name: "{localappdata}\P2P-Voz-e-tela\settings.json"
Type: files; Name: "{localappdata}\P2P-Voz-e-tela\settings.json.tmp"
