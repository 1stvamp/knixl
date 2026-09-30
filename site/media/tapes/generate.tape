# knixl generate, check, and doc on a fresh project holding two example hosts.
Source tapes/_settings.tape
Source tapes/_prelude.tape

Type "cat hosts/web.kdl"
Sleep 300ms
Enter
Sleep 3s

Type "knixl plan"
Sleep 300ms
Enter
Sleep 2s

Type "knixl generate"
Sleep 300ms
Enter
Sleep 1s

Type "knixl check"
Sleep 300ms
Enter
Sleep 2.5s

Type "clear"
Enter
Type "knixl doc web-service"
Sleep 300ms
Enter
Sleep 4s

Screenshot poster.png
Sleep 1s
