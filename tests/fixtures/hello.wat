;; Minimal WASI module that prints "Hello from Wasm\n" to stdout.
(module
  ;; Import fd_write from wasi_snapshot_preview1
  (import "wasi_snapshot_preview1" "fd_write"
    (func $fd_write (param i32 i32 i32 i32) (result i32)))

  (memory (export "memory") 1)

  ;; The string "Hello from Wasm\n" at offset 32
  (data (i32.const 32) "Hello from Wasm\n")

  ;; iov at offset 0: { buf_ptr=32, buf_len=16 }
  (data (i32.const 0) "\20\00\00\00")  ;; pointer to string (32)
  (data (i32.const 4) "\10\00\00\00")  ;; length of string (16)

  (func (export "_start")
    ;; fd_write(fd=1, iovs=0, iovs_len=1, nwritten=16)
    (drop
      (call $fd_write
        (i32.const 1)   ;; stdout
        (i32.const 0)   ;; iovs pointer
        (i32.const 1)   ;; iovs count
        (i32.const 16)  ;; nwritten pointer
      )
    )
  )
)
