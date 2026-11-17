provide
  . outline-view outline-view? outline-view-collapsed outline-view-filter struct:outline-view ___outline-view-options___
  . outline-state outline-state? outline-state-source outline-state-version struct:outline-state ___outline-state-options___
  . outline-tail outline-children? outline-project outline-update outline-handler

require-builtin zess/filter
require "zess/keys"
require "zess/buffer"
require "zess/ui"

register-key-scope "outline" "Outline"

struct outline-view (collapsed filter) #:transparent
struct outline-state (source version) #:transparent

define : outline-tail id symbols
  cond
    : null? symbols
      . '()
    : equal? id : symbol-id : car symbols
      . symbols
    else
      outline-tail id : cdr symbols

define : outline-children? symbols
  and
    pair? symbols
    pair? : cdr symbols
    > (symbol-depth (cadr symbols)) : symbol-depth : car symbols

define : outline-project symbols collapsed compiled-filter
  let loop
    : remaining symbols
      hidden-depth #f
      rows '()
    if (null? remaining)
      reverse rows
      let*
        : symbol : car remaining
          depth : symbol-depth symbol
          id : symbol-id symbol
        if (and hidden-depth (> depth hidden-depth))
          loop (cdr remaining) hidden-depth rows
          let*
            : children : outline-children? remaining
              folded : and children : member id collapsed
              label
                string-append
                  if children (if folded "▸ " "▾ ") "  "
                  symbol-kind symbol
                  . " "
                  symbol-name symbol
              row : display-row id label "symbol" depth
            loop
              cdr remaining
              if folded depth #f
              if (filter-matches? compiled-filter (symbol-name symbol))
                cons row rows
                . rows

define : outline-update input
  let*
    : previous : buffer-input-view input
      view : if (outline-view? previous) previous : outline-view '() : filter-compile : query "plain" ""
      text : input-text input
      symbols : text-context-symbols text
      selection : input-selection input
      primary : selection-primary selection
      targets : filter (lambda (symbol) (equal? (symbol-id symbol) primary)) symbols
      tail : outline-tail primary symbols
      collapsed : outline-view-collapsed view
      compiled-filter : outline-view-filter view
      event : input-event input
      requests '()
    case event
      : "open"
        when (pair? targets)
          set! requests : list : request-focus-text (text-context-id text) : symbol-line : car targets
      : "toggle" "fold" "unfold"
        when (and tail (outline-children? tail))
          set! collapsed
            if
              or (equal? event "unfold") : and (equal? event "toggle") : member primary collapsed
              filter (lambda (id) (not (equal? id primary))) collapsed
              if (member primary collapsed) collapsed : cons primary collapsed
      : "filter_changed"
        set! compiled-filter : filter-compile : input-query input
    let*
      : rows : outline-project symbols collapsed compiled-filter
        filter-text : filter-source compiled-filter
        first : if (pair? rows) (row-id (car rows)) #f
        selection
          if (equal? event "filter_changed")
            item-selection first (if first (list first) '()) first
            . #f
      buffer-result
        outline-state (text-context-id text) : text-context-version text
        outline-view collapsed compiled-filter
        buffer-input-memory input
        buffer-output
          buffer-display
            string-append "Outline · " : or (text-context-path text) "[scratch]"
            if
              or (equal? filter-text "") : equal? (input-mode input) "filter"
              . ""
              string-append "/" filter-text
            . rows #f ""
          . selection requests

define : outline-handler input
  if (member (input-event input) '("select" "view_leaving"))
    buffer-result (buffer-input-state input) (buffer-input-view input) (buffer-input-memory input)
      buffer-output #f #f '()
    outline-update input

register-buffer "outline" outline-handler
register-context "outline" "text.symbols"
  array "init" "refresh" "render" "open" "view_cloned" "mode_changed" "toggle" "fold" "unfold" "filter_changed"
define-mode "outline" "filter"

register-command "outline" "Open outline buffer"
  lambda : argument
    request-open-buffer "outline" argument

for-each
  lambda : entry
    match entry
      : list event description keys
        let
          : name : string-append "outline." event
          register-buffer-command name description "outline" event
          map-modal "outline" keys name
  list
    list "open" "Go to symbol" "Enter"
    list "toggle" "Toggle children" "Tab"
    list "fold" "Fold children" "h"
    list "unfold" "Unfold children" "l"
