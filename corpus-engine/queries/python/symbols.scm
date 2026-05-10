; Python symbol definitions.
; Uniform capture names: @definition + @name.

(function_definition name: (identifier) @name) @definition
(class_definition    name: (identifier) @name) @definition
(decorated_definition
  definition: (function_definition name: (identifier) @name)) @definition
(decorated_definition
  definition: (class_definition    name: (identifier) @name)) @definition
