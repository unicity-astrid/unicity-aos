(module
  (import "aos_realm_v0" "arg-count" (func $arg-count (result i32)))
  (import "aos_realm_v0" "arg-len" (func $arg-len (param i32) (result i32)))
  (import "aos_realm_v0" "arg-read"
    (func $arg-read (param i32 i32 i32) (result i32)))
  (import "aos_realm_v0" "pipe" (func $pipe (param i32 i32) (result i32)))
  (import "aos_realm_v0" "spawn-signed-record"
    (func $spawn (param i32 i32) (result i32)))
  (import "aos_realm_v0" "wait" (func $wait (param i32 i32) (result i32)))
  (import "aos_realm_v0" "exit" (func $exit (param i32)))

  (memory (export "memory") 1 1)
  (data (i32.const 4096) "echo")
  (data (i32.const 4100) "cat")
  (data (i32.const 4103) "env")
  (data (i32.const 4106) "|")
  (data (i32.const 4107) "/bin/echo")
  (data (i32.const 4116) "/bin/cat")
  (data (i32.const 4124) "/usr/bin/env")

  ;; Compare one structured process argument with a literal already in memory.
  (func $arg-is
    (param $index i32) (param $literal i32) (param $literal-length i32)
    (result i32)
    (local $offset i32)
    (local $result i32)
    local.get $index
    call $arg-len
    local.get $literal-length
    i32.eq
    if
      local.get $index
      i32.const 1024
      local.get $literal-length
      call $arg-read
      drop
      i32.const 1
      local.set $result
      (block $done
        (loop $next
          local.get $offset
          local.get $literal-length
          i32.ge_u
          br_if $done
          i32.const 1024
          local.get $offset
          i32.add
          i32.load8_u
          local.get $literal
          local.get $offset
          i32.add
          i32.load8_u
          i32.ne
          if
            i32.const 0
            local.set $result
            br $done
          end
          local.get $offset
          i32.const 1
          i32.add
          local.set $offset
          br $next))
    end
    local.get $result)

  (func $copy-arg
    (param $index i32) (param $destination i32) (param $maximum i32)
    (result i32)
    (local $length i32)
    local.get $index
    call $arg-len
    local.tee $length
    local.get $maximum
    i32.gt_u
    if
      i32.const 65
      call $exit
    end
    local.get $index
    local.get $destination
    local.get $length
    call $arg-read
    local.get $length
    i32.ne
    if
      i32.const 66
      call $exit
    end
    local.get $length)

  ;; Encode the fixed 44-byte spawn record at address zero.
  (func $record
    (param $executable i32) (param $executable-length i32)
    (param $argv i32) (param $argc i32)
    (param $environment i32) (param $environment-count i32)
    (param $actions i32) (param $action-count i32)
    (param $handle i32)
    i32.const 0 i32.const 1 i32.store
    i32.const 4 i32.const 0 i32.store
    i32.const 8 local.get $executable i32.store
    i32.const 12 local.get $executable-length i32.store
    i32.const 16 local.get $argv i32.store
    i32.const 20 local.get $argc i32.store
    i32.const 24 local.get $environment i32.store
    i32.const 28 local.get $environment-count i32.store
    i32.const 32 local.get $actions i32.store
    i32.const 36 local.get $action-count i32.store
    i32.const 40 local.get $handle i32.store)

  (func $wait-ok (param $handle i32) (param $status i32) (param $failure i32)
    local.get $handle
    local.get $status
    call $wait
    drop
    local.get $status
    i32.load
    i32.eqz
    local.get $status
    i32.const 4
    i32.add
    i32.load
    i32.eqz
    i32.and
    if
    else
      local.get $failure
      call $exit
    end)

  (func $spawn-echo (param $message-length i32) (param $handle i32)
    ;; argv = ["echo", message]
    i32.const 256 i32.const 4096 i32.store
    i32.const 260 i32.const 4 i32.store
    i32.const 264 i32.const 2048 i32.store
    i32.const 268 local.get $message-length i32.store
    i32.const 4107 i32.const 9
    i32.const 256 i32.const 2
    i32.const 0 i32.const 0
    i32.const 0 i32.const 0
    local.get $handle
    call $record
    i32.const 0 i32.const 44 call $spawn
    drop)

  (func $direct-echo (param $message-length i32)
    local.get $message-length
    i32.const 80
    call $spawn-echo
    i32.const 80 i32.const 112 i32.const 70 call $wait-ok)

  (func $direct-env (param $entry-length i32)
    ;; argv = ["env"], environment = [argv[2]]
    i32.const 256 i32.const 4103 i32.store
    i32.const 260 i32.const 3 i32.store
    i32.const 320 i32.const 2048 i32.store
    i32.const 324 local.get $entry-length i32.store
    i32.const 4124 i32.const 12
    i32.const 256 i32.const 1
    i32.const 320 i32.const 1
    i32.const 0 i32.const 0
    i32.const 80
    call $record
    i32.const 0 i32.const 44 call $spawn
    drop
    i32.const 80 i32.const 112 i32.const 71 call $wait-ok)

  (func $pipeline (param $message-length i32)
    (local $read-fd i32)
    (local $write-fd i32)
    i32.const 4 i32.const 64 call $pipe drop
    i32.const 64 i32.load local.set $read-fd
    i32.const 68 i32.load local.set $write-fd

    ;; Consumer: /bin/cat, dup(read, stdin), close-parent(read).
    i32.const 256 i32.const 4100 i32.store
    i32.const 260 i32.const 3 i32.store
    i32.const 352 i32.const 1 i32.store
    i32.const 356 local.get $read-fd i32.store
    i32.const 360 i32.const 0 i32.store
    i32.const 364 i32.const 2 i32.store
    i32.const 368 local.get $read-fd i32.store
    i32.const 372 i32.const -1 i32.store
    i32.const 4116 i32.const 8
    i32.const 256 i32.const 1
    i32.const 0 i32.const 0
    i32.const 352 i32.const 2
    i32.const 80
    call $record
    i32.const 0 i32.const 44 call $spawn drop

    ;; Producer: /bin/echo, dup(write, stdout), close-parent(write).
    i32.const 256 i32.const 4096 i32.store
    i32.const 260 i32.const 4 i32.store
    i32.const 264 i32.const 2048 i32.store
    i32.const 268 local.get $message-length i32.store
    i32.const 352 i32.const 1 i32.store
    i32.const 356 local.get $write-fd i32.store
    i32.const 360 i32.const 1 i32.store
    i32.const 364 i32.const 2 i32.store
    i32.const 368 local.get $write-fd i32.store
    i32.const 372 i32.const -1 i32.store
    i32.const 4107 i32.const 9
    i32.const 256 i32.const 2
    i32.const 0 i32.const 0
    i32.const 352 i32.const 2
    i32.const 96
    call $record
    i32.const 0 i32.const 44 call $spawn drop

    i32.const 96 i32.const 112 i32.const 72 call $wait-ok
    i32.const 80 i32.const 120 i32.const 73 call $wait-ok)

  (func (export "_start")
    (local $length i32)
    call $arg-count
    i32.const 3
    i32.eq
    if
      i32.const 1 i32.const 4096 i32.const 4 call $arg-is
      if
        i32.const 2 i32.const 2048 i32.const 32768 call $copy-arg
        local.set $length
        local.get $length call $direct-echo
        i32.const 0 call $exit
      end
      i32.const 1 i32.const 4103 i32.const 3 call $arg-is
      if
        i32.const 2 i32.const 2048 i32.const 32768 call $copy-arg
        local.set $length
        local.get $length call $direct-env
        i32.const 0 call $exit
      end
    end

    call $arg-count
    i32.const 5
    i32.eq
    if
      i32.const 1 i32.const 4096 i32.const 4 call $arg-is
      i32.const 3 i32.const 4106 i32.const 1 call $arg-is
      i32.and
      i32.const 4 i32.const 4100 i32.const 3 call $arg-is
      i32.and
      if
        i32.const 2 i32.const 2048 i32.const 32768 call $copy-arg
        local.set $length
        local.get $length call $pipeline
        i32.const 0 call $exit
      end
    end

    i32.const 64
    call $exit
    unreachable))
