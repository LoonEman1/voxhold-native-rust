; VoxHold beta installer.
;
; Per-user installation without elevation. The version and staging directory
; are supplied by the release workflow:
;   ISCC.exe /DMyAppVersion=0.2.0 /DStageDir=<payload> /DOutputDir=<dist> installer\voxhold.iss

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif
#ifndef StageDir
  #define StageDir "..\stage"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

[Setup]
AppId={{6C2A9D54-9B1E-4C6B-8F2B-5C7E0B3F1A21}
AppName=VoxHold
AppVersion={#MyAppVersion}
AppPublisher=VoxHold
AppPublisherURL=https://github.com/LoonEman1/voxhold-native-rust
DefaultDirName={localappdata}\Programs\VoxHold
DefaultGroupName=VoxHold
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=VoxHold-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\Voxhold.exe
SetupIconFile=..\assets\voxhold.ico
CloseApplications=yes
RestartApplications=no

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\VoxHold"; Filename: "{app}\Voxhold.exe"
Name: "{group}\Uninstall VoxHold"; Filename: "{uninstallexe}"
Name: "{autodesktop}\VoxHold"; Filename: "{app}\Voxhold.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Run]
Filename: "{app}\Voxhold.exe"; Description: "Launch VoxHold"; Flags: nowait postinstall skipifsilent
