;; Conditional policy: eval(input) -> (input > 0 ? 1 : 0).
(module
  (func $eval (export "eval") (param i32) (result i32)
    local.get 0
    i32.const 0
    i32.gt_s
  )
)
