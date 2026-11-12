require-builtin zess/native

provide
  . buffer-input buffer-input-state buffer-input-view buffer-input-memory
  . input-event input-argument input-query input-options input-context input-mode input-selection
  . input-buffer-id input-view-id input-generation input-register
  . buffer-result unpack-buffer-result
  . buffer-output buffer-display with-status-data display-row row-id row-text row-depth row-style
  . display-cell cell-append-span row-cells with-row-cells
  . item-selection selection-primary selection-selected selection-anchor
  . output-watches output-modified watch-query
  . preview-text preview-diff preview-directory preview-unavailable
  . input-text text-context-id text-context-path text-context-version text-context-symbols
  . symbol-id symbol-name symbol-kind symbol-line symbol-column symbol-depth
  . context-workspace context-text context-filesystem-revision

struct buffer-input (host state view memory)
     . #:transparent
struct buffer-result (state view memory output)
     . #:transparent

define : unpack-buffer-result result
  list
    buffer-result-state result
    buffer-result-view result
    buffer-result-memory result
    buffer-result-output result

define : input-event input
  %input-event : buffer-input-host input
define : input-text input
  %input-text : buffer-input-host input
define : input-argument input
  %input-argument : buffer-input-host input

define : input-query input
  %input-query : buffer-input-host input

define : input-options input
  %input-options : buffer-input-host input
define : input-context input
  %input-context : buffer-input-host input
define : input-mode input
  %input-mode : buffer-input-host input
define : input-selection input
  %input-selection : buffer-input-host input
define : input-buffer-id input
  %input-buffer-id : buffer-input-host input
define : input-view-id input
  %input-view-id : buffer-input-host input
define : input-generation input
  %input-generation : buffer-input-host input
define : input-register input
  %input-register : buffer-input-host input
