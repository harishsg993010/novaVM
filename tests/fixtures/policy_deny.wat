;; Policy that always denies (eval() -> 0).
(module
  (func $eval (export "eval") (result i32)
    i32.const 0
  )
)
