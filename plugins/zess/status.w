require-builtin zess/native
require "zess/config"
require "zess/status-layout"

provide
  . statusline status-span status-mode status-mode-span status-position status-source-position set-mode-labels!
  . status-modified status-loading status-plugin status-text status-format set-status-formats! default-statusline
  . statusline-layout set-statusline-layout! register-statusline-component

define default-status-formats : %status-format-defaults
define status-formats : hash

define-syntax statusline-layout
  syntax-rules ()
    : _ kind region ...
      set-statusline-layout! 'kind '(region ...)

define : set-status-formats! . entries
  let
    : formats : %validate-status-formats : configuration-map entries
    let loop
      : keys : hash-keys->list formats
        updated status-formats
      if (null? keys)
        set! status-formats updated
        let
          : key : car keys
          loop (cdr keys) : hash-insert updated key : hash-ref formats key

define : statusline left center right
  hash 'left left 'center center 'right right

define : status-span text style
  hash 'text text 'style style

define : status-get object key fallback
  if
    and (hash? object) : hash-contains? object key
    hash-ref object key
    . fallback

define status-format
  case-lambda
    : context name
      status-format context name : hash
    : context name fields
      let*
        : options : status-get context 'options : hash
          formats : status-get options 'status-formats : hash
          template
            status-get formats name
              status-get status-formats name : hash-ref default-status-formats name
        let loop
          : keys : hash-keys->list fields
            text template
          if (null? keys)
            . text
            let
              : key : car keys
              loop (cdr keys)
                string-replace text
                  string-append "{" (symbol->string key) "}"
                  to-string : hash-ref fields key

define mode-labels
  hash "normal" "NOR" "insert" "INS" "select" "SEL"
    . "filter" "FLT" "find" "FND" "block" "BLK"
    . "line" "LIN" "range" "RNG"

define : set-mode-labels! . entries
  let
    : labels : configuration-map entries
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

define : status-source-position context
  let*
    : position : status-get context 'source_position : hash
      new-line : status-get position 'new_line #f
      line : if (number? new-line) new-line : status-get position 'old_line #f
    if (number? line) (to-string line) ""

define : status-modified context
  if (status-get context 'modified #f) (status-format context 'modified) ""

define : status-loading context
  if (status-get context 'loading #f) (status-format context 'loading) ""

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

define : status-git context
  let*
    : plugin-status : status-get context 'plugin_status #f
      changes : status-get plugin-status 'git #f
    cond
      : status-get plugin-status 'error #f
        . ""
      : and (hash? changes) : equal? (status-get changes 'state "") "loading"
        status-format context 'git-loading
      : and (hash? changes) : equal? (status-get changes 'state "") "failed"
        status-format context 'git-failed
      : hash? changes
        let
          : added : status-get changes 'additions #f
            removed : status-get changes 'deletions #f
            uncounted : status-get changes 'unknown_files 0
          if (and (number? added) (number? removed))
            list->vector
              list
                if (equal? (status-get changes 'state "") "refreshing")
                  status-format context 'git-refreshing
                  . ""
                status-span
                  status-format context 'git-added : hash 'count added
                  . "git.added"
                . "  "
                status-span
                  status-format context 'git-deleted : hash 'count removed
                  . "git.deleted"
                if (> uncounted 0)
                  status-format context 'git-uncounted : hash 'count uncounted
                  . ""
            status-format context 'git-unavailable
      else
        . ""

define : status-pending context
  let*
    : data : status-get context 'plugin_status_data #f
      files : status-get data 'files #f
      pending : status-get files 'pending_count 0
    if (> pending 0)
      status-format context 'pending : hash 'count pending
      . ""

define : status-items context
  status-format context 'items
    hash
      . 'selected : status-get context 'selected_count 0
      . 'items : status-get context 'item_count 0

define : status-text-position context
  if (status-get context 'position #f)
    status-format context 'text
      hash
        . 'selections : status-get context 'selection_count 1
        . 'position : status-position context
        . 'encoding : status-get context 'encoding "UTF-8"
    . ""

define : status-text context
  let*
    : plugin-status : status-get context 'plugin_status #f
      changes : status-get plugin-status 'git #f
    cond
      : or (hash? changes) : status-get plugin-status 'error #f
        status-git context
      : status-get context 'position #f
        status-text-position context
      else
        string-join
          filter
            lambda : text
              not : equal? text ""
            list
              status-pending context
              status-items context
          . " · "

define : default-statusline context
  statusline
    list->vector
      list
        status-mode-span context
        . " "
        status-get context 'title ""
        status-modified context
        status-loading context
    let*
      : data : status-get context 'plugin_status_data #f
        files : status-get data 'files #f
      if (hash? files)
        . ""
        status-plugin context
    status-text context

register-statusline-component 'mode status-mode-span
register-statusline-component 'title
  lambda : context
    status-get context 'title ""
register-statusline-component 'modified status-modified
register-statusline-component 'loading status-loading
register-statusline-component 'pending status-pending
register-statusline-component 'items
  lambda : context
    if (hash-contains? context 'item_count) (status-items context) ""
register-statusline-component 'text status-text-position
register-statusline-component 'git status-git
register-statusline-component 'plugin status-plugin
register-statusline-component 'position status-position
register-statusline-component 'encoding
  lambda : context
    status-get context 'encoding ""
register-statusline-component 'source-position status-source-position
