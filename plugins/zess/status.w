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
        begin
          set! status-formats updated
          %set-status-formats status-formats
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

%set-status-mode-labels mode-labels

define : set-mode-labels! . entries
  let
    : labels : configuration-map entries
    let loop
      : keys : hash-keys->list labels
        updated mode-labels
      if (null? keys)
        begin
          set! mode-labels updated
          %set-status-mode-labels mode-labels
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
    : active : status-get context 'active #t
      text : %format-mode-label (status-mode context) (status-get context 'width #f) active
    status-span text
      if active
        string-append "status." : status-get context 'mode "normal"
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

define : status-git-pending? context
  let*
    : plugin-status : status-get context 'plugin_status #f
      changes : status-get plugin-status 'git #f
      state : status-get changes 'state ""
    or (number? (status-get changes 'write_epoch #f))
      and (not (status-get plugin-status 'error #f))
        or (equal? state "loading") (equal? state "refreshing")

define : status-git context
  let*
    : plugin-status : status-get context 'plugin_status #f
      changes : status-get plugin-status 'git #f
      state : status-get changes 'state ""
      loading : status-get context 'loading #f
      writing : and loading (number? (status-get changes 'write_epoch #f))
    cond
      : status-get plugin-status 'error #f
        if writing (status-format context 'git-writing) ""
      : equal? state "loading"
        if loading
          status-format context : if writing 'git-writing 'git-loading
          . ""
      : equal? state "failed"
        status-format context : if writing 'git-writing 'git-failed
      : hash? changes
        let
          : added : status-get changes 'additions #f
            removed : status-get changes 'deletions #f
            uncounted : status-get changes 'unknown_files 0
          if (and (number? added) (number? removed))
            list->vector
              list
                cond
                  . (writing (status-format context 'git-writing))
                  : and loading (equal? state "refreshing")
                    status-format context 'git-refreshing
                  else
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
            status-format context : if writing 'git-writing 'git-unavailable
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
  let*
    : kind : status-get context 'kind ""
      history : equal? kind "git-history"
      data : status-get context 'plugin_status_data #f
      files : status-get data 'files #f
    statusline
      list->vector
        list
          status-mode-span context
          . " "
          status-get context 'title ""
          if (and (equal? kind "files") (not (equal? (status-pending context) ""))) "" : status-modified context
          if (and (not history) (status-git-pending? context)) "" : status-loading context
      if (hash? files)
        . ""
        status-plugin context
      if history (status-items context) : status-text context

%register-builtin-statusline-component 'mode status-mode-span
%register-builtin-statusline-component 'title
  lambda : context
    status-get context 'title ""
%register-builtin-statusline-component 'modified status-modified
%register-builtin-statusline-component 'loading status-loading
%register-builtin-statusline-component 'pending status-pending
%register-builtin-statusline-component 'items
  lambda : context
    if (hash-contains? context 'item_count) (status-items context) ""
%register-builtin-statusline-component 'text status-text-position
%register-builtin-statusline-component 'git status-git
%register-builtin-statusline-component 'plugin status-plugin
%register-builtin-statusline-component 'position status-position
%register-builtin-statusline-component 'encoding
  lambda : context
    status-get context 'encoding ""
%register-builtin-statusline-component 'source-position status-source-position
