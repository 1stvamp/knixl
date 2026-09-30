# Pin a package version: the pin lands in the KDL, the lock and the generated Nix.
Require bat
Source tapes/_settings.tape
Source tapes/_prelude.tape

Type "knixl install curl@8.4.0 --host web --yes --no-abi-check"
Sleep 300ms
Enter
Sleep 3s

Type "grep curl hosts/web.kdl"
Sleep 300ms
Enter
Sleep 1.5s
Type `grep -A1 'host "web"' knixl.lock.kdl`
Sleep 300ms
Enter
Sleep 3s

Type "clear"
Enter
Type "grep -A11 systemPackages generated/hosts/web.nix | bat -pp -l nix"
Sleep 300ms
Enter
Sleep 3.5s

Type "knixl check"
Sleep 300ms
Enter
Sleep 4s
Screenshot poster.png
Sleep 1s
