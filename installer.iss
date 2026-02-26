[Setup]
AppId={{1490D457-CF8A-4229-8ED4-BCEB2FCC1980}
AppName=Condor3 Revive Helper
AppVersion=0.1.0
;AppVerName=Condor3 Revive Helper 0.1.0
AppPublisher=TheGreatCabbage
DefaultDirName={autopf}\Condor3 Revive Helper
DefaultGroupName=Condor3 Revive Helper
; Uncomment the following line to run in non administrative install mode (install for current user only.)
;PrivilegesRequired=lowest
OutputBaseFilename=Condor3ReviveHelper_Setup
Compression=lzma
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Registry]
Root: HKLM; Subkey: "Software\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Condor.exe"; Flags: uninsdeletekey

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "target\opt\gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "target\opt\Condor-VR-Configurer.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "target\opt\CondorVR.exe"; DestDir: "{app}"; Flags: ignoreversion
; NOTE: Don't use "Flags: ignoreversion" on any shared system files

[Icons]
Name: "{group}\Condor3 Revive Helper"; Filename: "{app}\gui.exe"
Name: "{autodesktop}\Condor3 Revive Helper"; Filename: "{app}\gui.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\gui.exe"; Description: "{cm:LaunchProgram,Condor3 Revive Helper}"; Flags: nowait postinstall skipifsilent
