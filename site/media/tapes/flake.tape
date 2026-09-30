# Input mode: pin flake inputs with upgrade, generate a bootable flake, lock it, check it.
Require bat
Require git
Require nix
Source tapes/_settings.tape
Source tapes/_prelude.tape

Type "cat knixl.kdl"
Sleep 300ms
Enter
Sleep 3s

Type "knixl upgrade --yes"
Sleep 300ms
Enter
Sleep 3s
Type "knixl generate"
Sleep 300ms
Enter
Sleep 1s

Type "clear"
Enter
Type "bat -pp -r 4:24 generated/flake.nix"
Sleep 300ms
Enter
Sleep 5s

Type "clear"
Enter
Type "git add -A && nix flake lock ./generated 2>/dev/null"
Sleep 300ms
Enter
Sleep 2s
Type "ls generated/"
Sleep 300ms
Enter
Sleep 1.2s
Type "knixl check"
Sleep 300ms
Enter
Sleep 4s
Screenshot poster.png
Sleep 1s
