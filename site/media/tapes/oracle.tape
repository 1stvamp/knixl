# A misspelt option is refused before anything is written (exit 5), then fixed.
Source tapes/_settings.tape
Source tapes/_prelude.tape

Type "grep -n timezon hosts/nas.kdl"
Sleep 300ms
Enter
Sleep 2s

Type "knixl check"
Sleep 300ms
Enter
Sleep 1.5s
Type "echo $?"
Sleep 300ms
Enter
Sleep 2.5s

Type "sed -i 's/timezon /timezone /' hosts/nas.kdl"
Sleep 300ms
Enter
Sleep 800ms
Type "knixl check"
Sleep 300ms
Enter
Sleep 4s
Screenshot poster.png
Sleep 1s
