provide map-modes map-modal define-mode register-key-scope

define : register-key-scope scope label
  %register-key-scope scope label

define : map-modes scope modes keys command
  for-each
    lambda : mode
      map-key scope mode keys command
    . modes

define : map-modal scope keys command
  map-modes scope (list "normal" "select") keys command

define-syntax define-mode
  syntax-rules ()
    : _ kind mode (keys command) ...
      begin
        register-mode kind mode
        . (map-key kind mode keys command) ...
