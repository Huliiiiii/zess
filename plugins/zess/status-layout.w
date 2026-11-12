require-builtin zess/native

provide set-statusline-layout! register-statusline-component

define register-statusline-component %register-statusline-component

struct status-fragment (render components)
  . #:transparent

define : status-visible? spans
  not
    null?
      filter
        lambda : span
          not : equal? (hash-ref span 'text) ""
        . spans

define : status-sequence fragments separator
  status-fragment
    lambda : context
      let loop
        : remaining fragments
          result '()
          visible #f
        if (null? remaining)
          . result
          let
            : spans : (status-fragment-render (car remaining)) context
            if (status-visible? spans)
              loop (cdr remaining)
                append result (if visible separator '()) spans
                . #t
              loop (cdr remaining) result visible
    apply append : map status-fragment-components fragments

define : compile-status-element element
  cond
    : string? element
      let
        : spans : %status-spans element
        status-fragment
          lambda : _
            . spans
          . '()
    : symbol? element
      let
        : name : symbol->string element
        status-fragment
          lambda : context
            %status-spans : (%statusline-component name) context
          list name
    : and (list? element) (pair? element) (equal? (car element) 'join)
      unless (and (pair? (cdr element)) (string? (cadr element)))
        error "statusline join requires a string separator"
      status-sequence
        map compile-status-element : cddr element
        %status-spans : cadr element
    else
      error : string-append "invalid statusline element: " : to-string element

define : status-region compiled name
  if (hash-contains? compiled name)
    let
      : fragment : hash-ref compiled name
      %make-statusline-region (status-fragment-render fragment) : status-fragment-components fragment
    . #f

define : set-statusline-layout! kind regions
  unless (list? regions)
    error "statusline layout regions must be a list"
  let loop
    : remaining regions
      compiled : hash
    if (null? remaining)
      %register-statusline-layout kind
        status-region compiled 'left
        status-region compiled 'center
        status-region compiled 'right
      let*
        : region : car remaining
          name : if (and (list? region) (pair? region)) (car region) #f
        unless (member name '(left center right))
          error : string-append "unknown statusline region: " : to-string name
        when (hash-contains? compiled name)
          error : string-append "duplicate statusline region: " : to-string name
        let
          : fragment : status-sequence (map compile-status-element (cdr region)) '()
          loop (cdr remaining)
            hash-insert compiled name fragment
