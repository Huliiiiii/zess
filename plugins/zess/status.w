provide
  . statusline status-span status-mode status-mode-span status-position set-mode-labels!
  . status-modified status-loading status-plugin status-text default-statusline

define : statusline left center right
  hash 'left left 'center center 'right right

define : status-span text style
  hash 'text text 'style style

define : status-get object key fallback
  if
    and (hash? object) : hash-contains? object key
    hash-ref object key
    . fallback

define mode-labels
  hash "normal" "NOR" "insert" "INS" "select" "SEL"
    . "filter" "FLT" "find" "FND" "block" "BLK"
    . "line" "LIN" "range" "RNG"

define : set-mode-labels! labels
  unless (hash? labels)
    error "mode labels must be a hash"
  let loop
    : keys : hash-keys->list labels
      updated mode-labels
    if (null? keys)
      set! mode-labels updated
      let*
        : key : car keys
          mode : if (symbol? key) (symbol->string key) key
          label : hash-ref labels key
        unless (string? mode)
          error "mode label keys must be strings or symbols"
        unless (string? label)
          error "mode labels must be strings"
        loop (cdr keys) : hash-insert updated mode label

define : status-mode context
  let
    : mode : status-get context 'mode "normal"
    if (hash-contains? mode-labels mode)
      hash-ref mode-labels mode
      string-upcase : substring mode 0 : min (string-length mode) 3

define : status-mode-span context
  let*
    : label : status-mode context
      width : status-get context 'width : + (string-length label) 2
      text
        cond
          : >= width : + (string-length label) 2
            string-append " " label " "
          : >= width : string-length label
            . label
          else
            substring label 0 : min width 1
    if (status-get context 'active #t)
      status-span
        . text
        string-append "status." : status-get context 'mode "normal"
      status-span
        make-string (string-length text) #\space
        . "status.inactive"

define : status-position context
  let
    : position : status-get context 'position #f
    if position
      string-append
        to-string : status-get position 'row 1
        . ":"
        to-string : status-get position 'column 1
      . ""

define : status-modified context
  if (status-get context 'modified #f) " [+]" ""

define : status-loading context
  if (status-get context 'loading #f) " …" ""

define : status-plugin context
  let
    : status : status-get context 'plugin_status ""
    cond
      : or (string? status) (vector? status) : list? status
        . status
      : hash? status
        status-get status 'text ""
      else
        . ""

define : status-text context
  let*
    : plugin-status : status-get context 'plugin_status #f
      changes : status-get plugin-status 'git #f
    cond
      : status-get plugin-status 'error #f
        . ""
      : and (hash? changes) : equal? (status-get changes 'state "") "loading"
        . "Loading changes…"
      : and (hash? changes) : equal? (status-get changes 'state "") "failed"
        . "Could not load changes"
      : hash? changes
        let
          : added : status-get changes 'additions #f
            removed : status-get changes 'deletions #f
            uncounted : status-get changes 'unknown_files 0
          if (and (number? added) (number? removed))
            list->vector
              list
                if (equal? (status-get changes 'state "") "refreshing")
                  . "Refreshing…  "
                  . ""
                status-span
                  string-append "+" : to-string added
                  . "git.added"
                . "  "
                status-span
                  string-append "-" : to-string removed
                  . "git.deleted"
                if (> uncounted 0)
                  string-append "  " (to-string uncounted) " uncounted"
                  . ""
            . "Line counts unavailable"
      : status-get context 'position #f
        string-append
          to-string : status-get context 'selection_count 1
          . " sel  "
          status-position context
          . "  "
          status-get context 'encoding "UTF-8"
      else
        string-append
          to-string : status-get context 'selected_count 0
          . " selected / "
          to-string : status-get context 'item_count 0
          . " items"

define : default-statusline context
  statusline
    list->vector
      list
        status-mode-span context
        . " "
        status-get context 'title ""
        status-modified context
        status-loading context
    status-plugin context
    status-text context
