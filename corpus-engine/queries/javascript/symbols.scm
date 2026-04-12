; JavaScript symbol definitions.
; Uniform capture names: @definition + @name.

(function_declaration name: (identifier)          @name) @definition
(class_declaration    name: (identifier)          @name) @definition
(method_definition    name: (property_identifier) @name) @definition

(lexical_declaration
  (variable_declarator
    name:  (identifier)     @name
    value: (arrow_function)) ) @definition
(lexical_declaration
  (variable_declarator
    name:  (identifier)     @name
    value: (function_expression)) ) @definition
