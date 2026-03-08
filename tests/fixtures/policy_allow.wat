;; Policy that always allows (eval() -> 1).
(module
  (func $eval (export "eval") (result i32)
    i32.const 1
  )
)
