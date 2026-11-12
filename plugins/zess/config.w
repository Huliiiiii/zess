provide configure-editor configure-views configuration-map

define : configuration-map entries
  if (and (pair? entries) (null? (cdr entries)) (hash? (car entries)))
    car entries
    let loop
      : rest entries
        result : hash
      cond
        : null? rest
          . result
        : null? : cdr rest
          error "configuration requires key/value pairs"
        else
          let*
            : key : car rest
              key
                cond
                  : symbol? key
                    . key
                  : string? key
                    string->symbol key
                  else
                    error "configuration keys must be strings or symbols"
            loop (cddr rest) : hash-insert result key : cadr rest

define-syntax configure-editor
  syntax-rules ()
    : _ (key value) ...
      set-editor-options!
        apply hash
          append (list 'key value) ...

define-syntax configure-views
  syntax-rules ()
    : _ kinds (key value) ...
      set-view-options! kinds
        apply hash
          append (list 'key value) ...
