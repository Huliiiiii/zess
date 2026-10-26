require-builtin zess/native

provide
  . request-action request-open-file request-open-buffer request-focus-text
  . request-layer request-file-layer request-mini-buffer request-mode request-message request-schedule
  . request-preview request-close request-yank configure-view
  . preview-surface preview-pane preview-pane-header preview-pane-scope preview-pane-loading
  . preview-layout-rows preview-layout-columns preview-layout-responsive request-preview-group

define configure-view %configure-view
define request-action %request-action
define request-open-file %request-open-file
define request-open-buffer %request-open-buffer
define request-focus-text %request-focus-text
define request-layer %request-layer
define request-file-layer %request-file-layer
define request-mini-buffer
  case-lambda
    : purpose text
      %request-mini-buffer purpose text #f
    : purpose text key-scope
      %request-mini-buffer purpose text key-scope
define request-mode %request-mode
define request-message %request-message
define request-yank
  case-lambda
    : values
      %request-yank values #f
    : values register
      %request-yank values register
define request-schedule %request-schedule
define request-preview %request-preview
define preview-surface %preview-surface
define preview-pane %preview-pane
define preview-pane-header %preview-pane-header
define preview-pane-scope %preview-pane-scope
define preview-pane-loading %preview-pane-loading
define preview-layout-rows %preview-layout-rows
define preview-layout-columns %preview-layout-columns
define preview-layout-responsive %preview-layout-responsive
define request-preview-group %request-preview-group
define request-close %request-close
