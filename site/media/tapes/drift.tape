# Stale versus drifted: an input edit regenerates quietly, a hand-edit is refused (exit 3).
Source tapes/_settings.tape
Source tapes/_prelude.tape

Type "knixl generate && knixl check"
Sleep 300ms
Enter
Sleep 2s

Type "sed -i 's/3000/8080/' hosts/web.kdl"
Sleep 300ms
Enter
Sleep 800ms
Type "knixl plan"
Sleep 300ms
Enter
Sleep 2.2s
Type "knixl generate && knixl check"
Sleep 300ms
Enter
Sleep 2.2s

Type "sed -i 's/512M/1G/' generated/hosts/web.nix"
Sleep 300ms
Enter
Sleep 800ms
Type "knixl plan"
Sleep 300ms
Enter
Sleep 2.2s
Type "knixl generate"
Sleep 300ms
Enter
Sleep 1.5s
Type "echo $?"
Sleep 300ms
Enter
Sleep 4s
Screenshot poster.png
Sleep 1s
