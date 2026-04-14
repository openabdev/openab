Set WShell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
scriptDir = fso.GetParentFolderName(WScript.ScriptFullName)
batPath = WScript.Arguments(0)
If Not fso.FileExists(batPath) Then
    batPath = fso.BuildPath(scriptDir, batPath)
End If
WShell.CurrentDirectory = scriptDir
WShell.Run Chr(34) & batPath & Chr(34), 0, False
