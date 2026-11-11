@echo off
rem build.cmd <drive letter of the mounted Enterprise WDK>  - builds the virtio display driver (x64, Windows 11) from
rem C:\src\kvm-guest-drivers-windows and test-signs it into C:\winvm\viogpudo-vsync. Run by rebuild.sh.
setlocal
call %1:\BuildEnv\SetupBuildEnv.cmd >nul
@echo off
cd /d C:\src\kvm-guest-drivers-windows
del /q viogpu\viogpudo\objfre_win11_amd64\amd64\viogpudo.sys 2>nul
msbuild VirtIO\VirtioLib.vcxproj /p:Configuration="Win11 Release" /p:Platform=x64 /m /nologo /v:quiet /fl /flp:LogFile=C:\src\build-virtiolib.log
rem the packaging step at the end fails on helper programs that are not built here; the driver itself is what matters
msbuild viogpu\viogpudo\viogpudo.vcxproj /p:Configuration="Win11 Release" /p:Platform=x64 /m /nologo /v:quiet /fl /flp:LogFile=C:\src\build-viogpudo.log >nul
findstr /c:": error C" /c:": error LNK" /c:"warning C" C:\src\build-viogpudo.log
if not exist viogpu\viogpudo\objfre_win11_amd64\amd64\viogpudo\viogpudo.sys (echo BUILD FAILED & exit /b 1)
cd viogpu\viogpudo\objfre_win11_amd64\amd64\viogpudo
signtool sign /q /fd SHA256 /f C:\src\kvm-guest-drivers-windows\build\VirtIOTestCert.pfx viogpudo.sys viogpudo.cat || exit /b 1
if not exist C:\winvm\viogpudo-vsync mkdir C:\winvm\viogpudo-vsync
copy /y * C:\winvm\viogpudo-vsync\ >nul
echo BUILD OK
