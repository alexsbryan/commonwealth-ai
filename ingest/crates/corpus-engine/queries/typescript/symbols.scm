; TypeScript symbol definitions.
; Uniform capture names: @definition + @name.

(function_declaration   name: (identifier)          @name) @definition
(class_declaration      name: (type_identifier)     @name) @definition
(method_definition      name: (property_identifier) @name) @definition
(interface_declaration  name: (type_identifier)     @name) @definition
(type_alias_declaration name: (type_identifier)     @name) @definition
(enum_declaration       name: (identifier)          @name) @definition

; const foo = () => { ... }  and  const Foo = function() { ... }
(lexical_declaration
  (variable_declarator
    name:  (identifier)     @name
    value: (arrow_function)) ) @definition
(lexical_declaration
  (variable_declarator
    name:  (identifier)     @name
    value: (function_expression)) ) @definition
