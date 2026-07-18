(module
  (import "aos_realm_v0" "env-count" (func $env-count (result i32)))
  (import "aos_realm_v0" "env-len" (func $env-len (param i32) (result i32)))
  (import "aos_realm_v0" "env-read"
    (func $env-read (param i32 i32 i32) (result i32)))
  (import "aos_realm_v0" "write"
    (func $write (param i32 i32 i32) (result i32)))
  (import "aos_realm_v0" "exit" (func $exit (param i32)))

  (memory (export "memory") 1 1)
  (data (i32.const 32768) "\n")

  (func $write-all (param $pointer i32) (param $length i32)
    (local $offset i32)
    (local $written i32)
    (block $done
      (loop $again
        local.get $offset
        local.get $length
        i32.ge_u
        br_if $done
        i32.const 1
        local.get $pointer
        local.get $offset
        i32.add
        local.get $length
        local.get $offset
        i32.sub
        call $write
        local.tee $written
        i32.eqz
        if
          i32.const 20
          call $exit
        end
        local.get $offset
        local.get $written
        i32.add
        local.set $offset
        br $again)))

  (func (export "_start")
    (local $index i32)
    (local $count i32)
    (local $length i32)

    call $env-count
    local.set $count
    (block $done
      (loop $next
        local.get $index
        local.get $count
        i32.ge_u
        br_if $done

        local.get $index
        call $env-len
        local.tee $length
        i32.const 32768
        i32.gt_u
        if
          i32.const 21
          call $exit
        end
        local.get $index
        i32.const 0
        local.get $length
        call $env-read
        local.get $length
        i32.ne
        if
          i32.const 22
          call $exit
        end
        i32.const 0
        local.get $length
        call $write-all
        i32.const 32768
        i32.const 1
        call $write-all

        local.get $index
        i32.const 1
        i32.add
        local.set $index
        br $next))
    i32.const 0
    call $exit
    unreachable))
