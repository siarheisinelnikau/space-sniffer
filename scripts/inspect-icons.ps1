[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ExecutablePath,
    [string]$ResourceName = 'IDI_ICON',
    [int]$ResourceId = 1,
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../target/icon-proof')
)
# Loads the executable as resource data only. Never starts the application or captures the desktop.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
if (-not ('ArtifactIconResources' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ArtifactIconResources {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern IntPtr LoadLibraryExW(string file, IntPtr reserved, uint flags);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool FreeLibrary(IntPtr module);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr FindResourceW(IntPtr module, IntPtr name, IntPtr type);
    [DllImport("kernel32.dll")] public static extern uint SizeofResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll")] public static extern IntPtr LoadResource(IntPtr module, IntPtr resource);
    [DllImport("kernel32.dll")] public static extern IntPtr LockResource(IntPtr resource);
    [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr LoadImageW(IntPtr module, IntPtr name, uint type, int width, int height, uint flags);
    [DllImport("user32.dll")] public static extern bool DestroyIcon(IntPtr icon);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool DrawIconEx(IntPtr dc,int x,int y,IntPtr icon,int width,int height,uint frame,IntPtr brush,uint flags);
}
'@
}
$taskExe = (Resolve-Path -LiteralPath $ExecutablePath).Path
$taskOutput = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $taskOutput | Out-Null
$taskModule = [ArtifactIconResources]::LoadLibraryExW($taskExe,[IntPtr]::Zero,0x22)
if ($taskModule -eq [IntPtr]::Zero) { throw 'Cannot load packaged resources.' }
$taskName = if ($ResourceId -gt 0) { [IntPtr]$ResourceId } else { [Runtime.InteropServices.Marshal]::StringToHGlobalUni($ResourceName) }
try {
    $taskResource = [ArtifactIconResources]::FindResourceW($taskModule,$taskName,[IntPtr]14)
    if ($taskResource -eq [IntPtr]::Zero) { throw 'Application icon group missing.' }
    $taskBytes = [byte[]]::new([ArtifactIconResources]::SizeofResource($taskModule,$taskResource))
    [Runtime.InteropServices.Marshal]::Copy([ArtifactIconResources]::LockResource([ArtifactIconResources]::LoadResource($taskModule,$taskResource)),$taskBytes,0,$taskBytes.Length)
    $taskCount = [BitConverter]::ToUInt16($taskBytes,4)
    if ($taskBytes.Length -ne 6 + 14*$taskCount) { throw 'Invalid icon group.' }
    $taskSizes = @()
    for ($taskIndex=0; $taskIndex -lt $taskCount; $taskIndex++) {
        $taskWidth = [int]$taskBytes[6+14*$taskIndex]; if ($taskWidth -eq 0) { $taskWidth=256 }
        $taskHeight = [int]$taskBytes[7+14*$taskIndex]; if ($taskHeight -eq 0) { $taskHeight=256 }
        if ($taskWidth -ne $taskHeight) { throw 'Non-square application icon.' }
        $taskSizes += $taskWidth
    }
    foreach ($taskRequired in @(16,20,24,32,48,64,256)) { if ($taskRequired -notin $taskSizes) { throw "Missing native icon size $taskRequired." } }
    $taskWidth = ($taskSizes | ForEach-Object { [Math]::Max(72,$_+20) } | Measure-Object -Sum).Sum
    $taskCanvas = [Drawing.Bitmap]::new([int]$taskWidth,600)
    $taskGraphics = [Drawing.Graphics]::FromImage($taskCanvas)
    $taskFont = [Drawing.Font]::new('Segoe UI',9)
    $taskGraphics.Clear([Drawing.Color]::White)
    $taskGraphics.FillRectangle([Drawing.Brushes]::DimGray,0,300,[int]$taskWidth,300)
    $taskX=0; $taskReport=@()
    try {
        foreach ($taskSize in $taskSizes) {
            $taskIcon = [ArtifactIconResources]::LoadImageW($taskModule,$taskName,1,$taskSize,$taskSize,0)
            if ($taskIcon -eq [IntPtr]::Zero) { throw "Cannot load icon at $taskSize pixels." }
            try {
                $taskManaged = [Drawing.Icon]::FromHandle($taskIcon)
                $taskBitmap = $taskManaged.ToBitmap()
                try {
                    $taskMinX=$taskSize; $taskMinY=$taskSize; $taskMaxX=-1; $taskMaxY=-1; $taskVisible=0
                    for ($taskY=0; $taskY -lt $taskBitmap.Height; $taskY++) {
                        for ($taskPixelX=0; $taskPixelX -lt $taskBitmap.Width; $taskPixelX++) {
                            if ($taskBitmap.GetPixel($taskPixelX,$taskY).A -gt 0) {
                                $taskVisible++; $taskMinX=[Math]::Min($taskMinX,$taskPixelX); $taskMinY=[Math]::Min($taskMinY,$taskY)
                                $taskMaxX=[Math]::Max($taskMaxX,$taskPixelX); $taskMaxY=[Math]::Max($taskMaxY,$taskY)
                            }
                        }
                    }
                    if ($taskVisible -eq 0 -or $taskBitmap.Width -ne $taskSize -or $taskBitmap.Height -ne $taskSize) { throw 'Blank or incorrectly sized native icon.' }
                    $taskReport += [pscustomobject]@{size=$taskSize; visiblePixels=$taskVisible; alphaBounds=@($taskMinX,$taskMinY,$taskMaxX,$taskMaxY)}
                } finally { $taskBitmap.Dispose(); $taskManaged.Dispose() }
                $taskCell=[Math]::Max(72,$taskSize+20); $taskLeft=$taskX+[int](($taskCell-$taskSize)/2)
                $taskGraphics.DrawString("$taskSize px",$taskFont,[Drawing.Brushes]::Black,[single]$taskX,10)
                $taskGraphics.DrawString("$taskSize px",$taskFont,[Drawing.Brushes]::White,[single]$taskX,310)
                $taskDc=$taskGraphics.GetHdc()
                try {
                    foreach ($taskTop in @(40,340)) {
                        if (-not [ArtifactIconResources]::DrawIconEx($taskDc,$taskLeft,$taskTop,$taskIcon,$taskSize,$taskSize,0,[IntPtr]::Zero,3)) { throw 'Native icon rendering failed.' }
                    }
                } finally { $taskGraphics.ReleaseHdc($taskDc) }
                $taskX += $taskCell
            } finally { [void][ArtifactIconResources]::DestroyIcon($taskIcon) }
        }
        $taskCanvas.Save((Join-Path $taskOutput 'icons.png'),[Drawing.Imaging.ImageFormat]::Png)
    } finally { $taskFont.Dispose(); $taskGraphics.Dispose(); $taskCanvas.Dispose() }
    $taskResult=[ordered]@{ executable=$taskExe; sha256=(Get-FileHash -LiteralPath $taskExe -Algorithm SHA256).Hash.ToLowerInvariant(); scope='Resource-only LoadImageW / DrawIconEx proof; application not launched; OS tray/window integration unverified'; icons=$taskReport }
    $taskResult | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $taskOutput 'icons.json')
    $taskResult | ConvertTo-Json -Depth 5
} finally {
    if ($ResourceId -eq 0) { [Runtime.InteropServices.Marshal]::FreeHGlobal($taskName) }
    [void][ArtifactIconResources]::FreeLibrary($taskModule)
}

