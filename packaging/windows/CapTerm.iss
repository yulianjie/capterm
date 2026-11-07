[Setup]
AppId={{A59EEB91-7DC4-4938-BE60-79C23BA37FAE}
AppName=CapTerm
AppVersion={#ReleaseVersion}
VersionInfoVersion={#FileVersion}
VersionInfoTextVersion={#ReleaseVersion}
DefaultDirName={localappdata}\Programs\CapTerm
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0.19041
SetupIconFile=..\..\design\icon\capterm.ico
UninstallDisplayIcon={app}\capterm.exe
CloseApplications=yes
CloseApplicationsFilter=capterm.exe
RestartApplications=no
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Files]
Source: "{#PayloadDirectory}\capterm.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDirectory}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\CapTerm"; Filename: "{app}\capterm.exe"
