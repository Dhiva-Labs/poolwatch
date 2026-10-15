# Asserts a Windows executable's PE/COFF machine type, so an arm64 job cannot
# silently ship an x64 binary (or the reverse).
#   -Machine 0x8664  x86-64
#   -Machine 0xAA64  ARM64
param(
  [Parameter(Mandatory = $true)][string]$Path,
  [Parameter(Mandatory = $true)][string]$Machine
)
$ErrorActionPreference = 'Stop'
$bytes = [IO.File]::ReadAllBytes((Resolve-Path $Path))
$peOffset = [BitConverter]::ToInt32($bytes, 0x3C)
$signature = [Text.Encoding]::ASCII.GetString($bytes, $peOffset, 2)
if ($signature -ne 'PE') { throw "$Path is not a PE file" }
$actual = [BitConverter]::ToUInt16($bytes, $peOffset + 4)
"{0}: machine type 0x{1:X4} (expected {2})" -f $Path, $actual, $Machine
if ($actual -ne [Convert]::ToUInt16($Machine, 16)) {
  throw ("{0} has machine type 0x{1:X4}, expected {2}" -f $Path, $actual, $Machine)
}
