Set WShell = CreateObject("WScript.Shell")
WShell.Run Chr(34) & WScript.Arguments(0) & Chr(34), 0, False
