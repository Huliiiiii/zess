(atom) @variable
(line head: (atom) @function.call)
(colon_list . (atom) @function.call)
((atom) @keyword
 (#any-of? @keyword "define" "define-syntax" "lambda" "let" "let*" "letrec"
  "if" "cond" "case" "else" "and" "or" "when" "unless" "begin" "set!"
  "quote" "quasiquote" "unquote" "syntax-rules"))
((atom) @constant (#match? @constant "^#[tf]$"))
(number) @constant
(char) @constant
(string) @string
(comment) @comment
(block_comment) @comment
(shebang) @comment
(line_prefix) @operator
(colon_list ":" @operator)
(continuation "." @operator)
