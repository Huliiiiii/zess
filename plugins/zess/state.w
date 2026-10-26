provide state-set

define : state-set state . entries
  let loop
    : result state
      rest entries
    cond
      : null? rest
        . result
      : null? : cdr rest
        error "state-set requires key/value pairs"
      else
        loop
          hash-insert result (car rest) : cadr rest
          cddr rest
