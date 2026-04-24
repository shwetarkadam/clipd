Set WshShell = CreateObject("WScript.Shell")
WshShell.Run Chr(34) & Replace(WScript.ScriptFullName, "ClipdTray.vbs", "clipd-ui.exe") & Chr(34), 0, False
