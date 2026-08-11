
OpenSBI v1.7
   ____                    _____ ____ _____
  / __ \                  / ____|  _ \_   _|
 | |  | |_ __   ___ _ __ | (___ | |_) || |
 | |  | | '_ \ / _ \ '_ \ \___ \|  _ < | |
 | |__| | |_) |  __/ | | |____) | |_) || |_
  \____/| .__/ \___|_| |_|_____/|____/_____|
        | |
        |_|

Platform Name               : riscv-virtio,qemu
Platform Features           : medeleg
Platform HART Count         : 8
Platform IPI Device         : aclint-mswi
Platform Timer Device       : aclint-mtimer @ 10000000Hz
Platform Console Device     : uart8250
Platform HSM Device         : ---
Platform PMU Device         : ---
Platform Reboot Device      : syscon-reboot
Platform Shutdown Device    : syscon-poweroff
Platform Suspend Device     : ---
Platform CPPC Device        : ---
Firmware Base               : 0x80000000
Firmware Size               : 403 KB
Firmware RW Offset          : 0x40000
Firmware RW Size            : 147 KB
Firmware Heap Offset        : 0x54000
Firmware Heap Size          : 67 KB (total), 4 KB (reserved), 12 KB (used), 50 KB (free)
Firmware Scratch Size       : 4096 B (total), 1400 B (used), 2696 B (free)
Runtime SBI Version         : 3.0
Standard SBI Extensions     : time,rfnc,ipi,base,hsm,srst,pmu,dbcn,fwft,legacy,dbtr,sse
Experimental SBI Extensions : none

Domain0 Name                : root
Domain0 Boot HART           : 4
Domain0 HARTs               : 0*,1*,2*,3*,4*,5*,6*,7*
Domain0 Region00            : 0x0000000000100000-0x0000000000100fff M: (I,R,W) S/U: (R,W)
Domain0 Region01            : 0x0000000010000000-0x0000000010000fff M: (I,R,W) S/U: (R,W)
Domain0 Region02            : 0x0000000002000000-0x000000000200ffff M: (I,R,W) S/U: ()
Domain0 Region03            : 0x0000000080000000-0x000000008003ffff M: (R,X) S/U: ()
Domain0 Region04            : 0x0000000080040000-0x000000008007ffff M: (R,W) S/U: ()
Domain0 Region05            : 0x000000000c400000-0x000000000c5fffff M: (I,R,W) S/U: (R,W)
Domain0 Region06            : 0x000000000c000000-0x000000000c3fffff M: (I,R,W) S/U: (R,W)
Domain0 Region07            : 0x0000000000000000-0xffffffffffffffff M: () S/U: (R,W,X)
Domain0 Next Address        : 0x0000000080200000
Domain0 Next Arg1           : 0x000000047fe00000
Domain0 Next Mode           : S-mode
Domain0 SysReset            : yes
Domain0 SysSuspend          : yes

Boot HART ID                : 4
Boot HART Domain            : root
Boot HART Priv Version      : v1.12
Boot HART Base ISA          : rv64imafdch
Boot HART ISA Extensions    : sstc,zicntr,zihpm,zicboz,zicbom,sdtrig,svadu
Boot HART PMP Count         : 16
Boot HART PMP Granularity   : 2 bits
Boot HART PMP Address Bits  : 54
Boot HART MHPM Info         : 16 (0x0007fff8)
Boot HART Debug Triggers    : 2 triggers
Boot HART MIDELEG           : 0x0000000000001666
Boot HART MEDELEG           : 0x0000000000f4b509
[kernel] main_init hart_id=4
remap_test passed!
/**** APPS ****
.
..
lost+found
root
proc
run
usr
sys
tmp
bin
lib
sbin
etc
boot
dev
mnt
srv
opt
var
media
home
work
glibc
musl
**************/
[timer] STIE enabled
[timer] global SIE enabled
[sbi-debug] start_hart(0) addr=0x80200000 ret=0x0
[sbi-debug] start_hart(1) addr=0x80200000 ret=0x0
[sbi-debug] start_hart(2) addr=0x80200000 ret=0x0
[kernel] Hello from hart 0!
[kernel] Hello from hart 1!
[kernel] Hello from hart 3!
[kernel] Hello from hart 2!
[timer] STIE enabled
[timer] STIE enabled
[sbi-debug] start_hart(3) addr=0x80200000 ret=0x0
[timer] STIE enabled
[timer] STIE enabled
[timer] global SIE enabled
[kernel] Hello from hart 5!
[sbi-debug] start_hart(5) addr=0x80200000 ret=0x0
[timer] global SIE enabled
[timer] global SIE enabled
[timer] global SIE enabled
[sbi-debug] start_hart(6) addr=0x80200000 ret=0x0
[kernel] Hello from hart 6!
[sbi-debug] start_hart(7) addr=0x80200000 ret=0x0
[timer] ST[IEuse r] _start: BeSS nand heaabp leinidtia
lized [(targci=0m)
er] STIE enabled
[time[userr] _s]ta rt:g argv parsedl; oentebriangl mai n
SI[inEit]  runnineg cagnaebntl_etestcoded.s
mh ..a.
in_init done, run tasks...
[timer] global SIE enabled
[kernel] Hello from hart 7!
[timer] STIE enabled
[timer] global SIE enabled
#### OS COMP TEST GROUP START cagent-glibc ####
Simple LLM Server listening on http://127.0.0.1:8080
API endpoint: http://127.0.0.1:8080/v1/chat/completions
Press Ctrl+C to stop

Request: POST /v1/chat/completions HTTP/1.1
  [Neural Inference]
    Template 0 (factorial calculation): score=2.475
    Template 1 (date calculation): score=0.000
    Template 2 (network connections): score=0.000
    Template 3 (cpu cores): score=0.000
    Template 4 (disk usage): score=0.000
    Template 5 (system uptime): score=0.000
    Template 6 (username): score=0.000
    Template 7 (listening ports): score=0.000
    Template 8 (kernel version): score=0.000
    => Selected: factorial calculation (score=2.475)
Request: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/completions HTTP/1.1
  [Neural Inference]
    Template 0 (factorial calculation): score=0.000
    Template 1 (date calculation): score=0.000
    Template 2 (network connections): score=1.768
    Template 3 (cpu cores): score=0.000
    Template 4 (disk usage): score=1.414
    Template 5 (system uptime): score=0.000
    Template 6 (username): score=0.000
    Template 7 (listening ports): score=0.000
    Template 8 (kernel version): score=3.182
    => Selected: kernel version (score=3.182)
Request: POST /v1/chat/completions HTTP/1.1
  [Neural Inference]
    Template 0 (factorial calculation): score=0.000
    Template 1 (date calculation): score=0.000
    Template 2 (network connections): score=3.464
    Template 3 (cpu cores): score=0.000
    Template 4 (disk usage): score=0.000
    Template 5 (system uptime): score=0.000
    Template 6 (username): score=0.000
    Template 7 (listening ports): score=1.155
    Template 8 (kernel version): score=1.155
    => Selected: network connections (score=3.464)
Request: POST /v1/chat/completions HTTP/1.1
  [Neural Inference]
    Template 0 (factorial calculation): score=0.000
    Template 1 (date calculation): score=0.000
    Template 2 (network connections): score=0.000
    Template 3 (cpu cores): score=3.182
    Template 4 (disk usage): score=0.000
    Template 5 (system uptime): score=3.182
    Template 6 (username): score=0.000
    Template 7 (listening ports): score=0.000
    Template 8 (kernel version): score=0.000
    => Selected: cpu cores (score=3.182)
Request: POST /v1/chat/completions HTTP/1.1
  [Neural Inference]
    Template 0 (factorial calculation): score=0.566
    Template 1 (date calculation): score=2.687
    Template 2 (network connections): score=0.000
    Template 3 (cpu cores): score=0.000
    Template 4 (disk usage): score=0.000
    Template 5 (system uptime): score=0.000
    Template 6 (username): score=0.000
    Template 7 (listening ports): score=0.000
    Template 8 (kernel version): score=0.000
    => Selected: date calculation (score=2.687)
Request: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/completions HTTP/1.1
Requetestestcatcsea sce agecnt fagacentotri afsl -create pass 35p3
ass 363
st: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/cteostcase cagent kernel pass 540
mpletions HTTP/1.1
testcase cagent cpu pass 610
Request: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/completions HTTP/1.1
  [Neural Inference]
    Template 0 (factorial calculation): score=0.000
    Template 1 (date calculation): score=0.000
    Template 2 (network connections): score=0.000
    Template 3 (cpu cores): score=0.000
    Template 4 (disk usage): score=2.000
    Template 5 (system uptime): score=0.000
    Template 6 (username): score=0.000
    Template 7 (listening ports): score=0.000
    Template 8 (kernel version): score=2.500
    => Selected: kernel version (score=2.500)
Request: POST /v1/chat/completions HTTP/1.1
testcase cagent date pass 796
testcase cagent network pass 894
Request: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/completions HTTP/1.1
Request: POST /v1/chat/completions HTTP/1.1
testcase cagent fs-usage pass 1159
testcase cagent fs-readwrite pass 1365
testcase cagent fs-directory pass 1342
testcase cagent fs-search pass 1322

Shutting down server...
#### OS COMP TEST GROUP END cagent-glibc ####
[init] cagent_testcode.sh exited with status 0
[init] running buildstorm_testcode.sh ...
#### OS COMP TEST GROUP START buildstorm-glibc ####
[UNIMPLEMENTED SYSCALL] ID: 430 pid=19 tid=19 comm=mount args=[0x40853c40,0x1,0x40853c40,0xafafafafcfc3dfdf,0x2000000000000,0x2]
[UNIMPLEMENTED SYSCALL] ID: 430 pid=19 tid=19 comm=mount args=[0x40853c40,0x1,0x40853c40,0xafafafdfcbdfd5df,0x2000000000000,0x2]
[UNIMPLEMENTED SYSCALL] ID: 430 pid=19 tid=19 comm=mount args=[0x40853c40,0x1,0x40853c40,0x303030303030303,0x2000000000000,0x2]
[K] hart[3] PID19 syscall 291 returned EFAULT
rustc 1.98.0-nightly (57d06900f 2026-05-27)
[K] hart[3] PID19 syscall 291 returned EFAULT
[K] hart[3] PID19 syscall 291 returned EFAULT
cargo 1.98.0-nightly (fbb61be30 2026-05-26)
BUILDSTORM_TOOLCHAIN ok
[K] hart[3] PID19 syscall 291 returned EFAULT
[K] hart[3] PID19 syscall 291 returned EFAULT
[K] hart[3] PID35 syscall 291 returned EFAULT
[K] hart[3] PID19 syscall 291 returned EFAULT
[K] hart[3] PID19 syscall 291 returned EFAULT
BUILDSTORM_MINIBUILD ok
----- pre-build tg-xtask (untimed) -----
[K] hart[3] PID19 syscall 291 returned EFAULT
[1m[92m    Finished[0m `dev` profile [unoptimized + debuginfo] target(s) in 5.32s
----- build arceos-helloworld (timed, arch=riscv64) -----
BUILDSTORM_BEGIN mode=multi
[K] hart[3] PID22 syscall 291 returned EFAULT
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.78s
     Running `target/debug/tg-xtask arceos build -p arceos-helloworld --arch riscv64`
[2026-08-11T01:02:25Z INFO  axbuild::context] Workspace root: /work/tgoskits
Using build config: /work/tgoskits/tmp/axbuild/config/arceos-helloworld/build-riscv64gc-unknown-none-elf.toml
[2026-08-11T01:02:26Z INFO  axbuild::build::config_file] Found build config at /work/tgoskits/tmp/axbuild/config/arceos-helloworld/build-riscv64gc-unknown-none-elf.toml
Using build config: /work/tgoskits/tmp/axbuild/config/arceos-helloworld/build-riscv64gc-unknown-none-elf.toml
[2026-08-11T01:02:27Z INFO  axbuild::build::config_file] Found build config at /work/tgoskits/tmp/axbuild/config/arceos-helloworld/build-riscv64gc-unknown-none-elf.toml
[axbuild] cargo build package=arceos-helloworld target=scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json config=/work/tgoskits/tmp/axbuild/config/arceos-helloworld/build-riscv64gc-unknown-none-elf.toml ...
sh -c /work/tgoskits/tmp/axbuild/std/prebuild/prebuild-riscv64gc-unknown-linux-musl-5d5e8c438d29430d.sh
[K] hart[3] PID24 syscall 291 returned EFAULT
[K] hart[3] PID32 syscall 291 returned EFAULT
[K] hart[3] PID32 syscall 291 returned EFAULT
[K] hart[3] PID32 syscall 291 returned EFAULT
AR_riscv64gc_unknown_linux_musl=riscv64-linux-musl-ar
AX_LOG=warn
AX_TARGET=riscv64gc-unknown-none-elf
CARGO_UNSTABLE_JSON_TARGET_SPEC=true
CC_riscv64gc_unknown_linux_musl=riscv64-linux-musl-cc
CFLAGS_riscv64gc_unknown_linux_musl=-march=rv64gc -mabi=lp64d -mcmodel=medany -fno-stack-protector
CXXFLAGS_riscv64gc_unknown_linux_musl=-march=rv64gc -mabi=lp64d -mcmodel=medany -fno-stack-protector
SMP=1
cargo build --config /work/tgoskits/tmp/axbuild/std/config-riscv64gc-unknown-linux-musl-dynamic.toml -p arceos-helloworld --target scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json -Z unstable-options --target-dir /work/tgoskits/target --features arceos,ax-std/irq,ax-std/paging,ax-std/smp,ax-std/std-compat -Z json-target-spec --release --message-format json-render-diagnostics
[K] hart[3] PID8 syscall 291 returned EFAULT
   Compiling compiler_builtins v0.1.160 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/compiler-builtins/compiler-builtins)
   Compiling core v0.0.0 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/core)
   Compiling libc v0.2.185
   Compiling std v0.0.0 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/std)
   Compiling thiserror v2.0.18
   Compiling heapless v0.9.3
   Compiling ax-errno v0.6.1 (/work/tgoskits/components/axerrno)
   Compiling num-traits v0.2.19
   Compiling ax-percpu v0.4.14 (/work/tgoskits/components/percpu/percpu)
   Compiling radium v0.7.0
   Compiling libc v0.2.186
   Compiling anyhow v1.0.103
   Compiling rust_decimal v1.42.1
   Compiling ax-alloc v0.8.12 (/work/tgoskits/os/arceos/modules/axalloc)
   Compiling someboot v0.3.5 (/work/tgoskits/platforms/someboot)
[K] hart[1] PID24 syscall 95 returned EFAULT
   Compiling somehal v0.8.0 (/work/tgoskits/platforms/somehal)
   Compiling ax-driver v0.12.0 (/work/tgoskits/drivers/ax-driver)
   Compiling axplat-dyn v0.7.12 (/work/tgoskits/platforms/axplat-dyn)
   Compiling ax-hal v0.5.28 (/work/tgoskits/os/arceos/modules/axhal)
   Compiling ax-task v0.6.4 (/work/tgoskits/os/arceos/modules/axtask)
[K] hart[4] PID25 syscall 95 returned EFAULT
   Compiling ax-runtime v0.10.4 (/work/tgoskits/os/arceos/modules/axruntime)
   Compiling scope-local v0.4.2 (/work/tgoskits/components/scope-local)
   Compiling ax-posix-api v0.5.29 (/work/tgoskits/os/arceos/api/arceos_posix_api)
[K] hart[6] PID38 syscall 291 returned EFAULT
[K] hart[6] PID38 syscall 291 returned EFAULT
   Compiling rustc-std-workspace-core v1.99.0 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/rustc-std-workspace-core)
   Compiling alloc v0.0.0 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc)
   Compiling rustc-demangle v0.1.27
   Compiling cfg-if v1.0.4
   Compiling panic_abort v0.0.0 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/panic_abort)
   Compiling rustc-literal-escaper v0.0.7
   Compiling unwind v0.0.0 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/unwind)
   Compiling rustc-std-workspace-alloc v1.99.0 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/rustc-std-workspace-alloc)
   Compiling panic_unwind v0.0.0 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/panic_unwind)
   Compiling hashbrown v0.17.1
   Compiling std_detect v0.1.5 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/std_detect)
   Compiling proc_macro v0.0.0 (/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/src/rust/library/proc_macro)
   Compiling log v0.4.33
   Compiling byteorder v1.5.0
   Compiling bitflags v2.13.0
   Compiling as-any v0.3.2
   Compiling scopeguard v1.2.0
   Compiling stable_deref_trait v1.2.1
   Compiling rdif-def v0.3.0 (/work/tgoskits/drivers/interface/rdif-def)
   Compiling lock_api v0.4.14
   Compiling hash32 v0.3.1
   Compiling bit_field v0.10.3
   Compiling ax-kernel-guard v0.3.11 (/work/tgoskits/components/kernel_guard)
   Compiling rdif-base v0.8.3 (/work/tgoskits/drivers/interface/rdif-base)
   Compiling tock-registers v0.10.1
   Compiling spin v0.12.0
   Compiling ax-kspin v0.3.15 (/work/tgoskits/components/kspin)
   Compiling derive_more v2.1.1
   Compiling pci_types v0.10.1
   Compiling irq-framework v0.3.0 (/work/tgoskits/components/irq-framework)
   Compiling spinning_top v0.3.0
   Compiling ax-memory-addr v0.6.9 (/work/tgoskits/memory/memory_addr)
   Compiling mmio-api v0.2.2 (/work/tgoskits/memory/mmio-api)
   Compiling futures-sink v0.3.32
   Compiling futures-core v0.3.32
   Compiling fdt-raw v0.3.0
   Compiling rdif-msi v0.2.0 (/work/tgoskits/drivers/interface/rdif-msi)
   Compiling rdif-pcie v0.2.5 (/work/tgoskits/drivers/interface/rdif-pcie)
   Compiling acpi v6.1.1
   Compiling strum v0.27.2
   Compiling rdif-reset v0.1.0 (/work/tgoskits/drivers/interface/rdif-reset)
   Compiling rdif-power v0.8.0 (/work/tgoskits/drivers/interface/rdif-power)
   Compiling rdif-clk v0.5.4 (/work/tgoskits/drivers/interface/rdif-clk)
   Compiling futures-task v0.3.32
   Compiling pcie v0.6.7 (/work/tgoskits/drivers/pci/pcie)
   Compiling pin-project-lite v0.2.17
   Compiling slab v0.4.12
   Compiling futures-channel v0.3.32
   Compiling fdt-edit v0.2.3
   Compiling rdif-intc v0.15.0 (/work/tgoskits/drivers/interface/rdif-intc)
   Compiling futures-util v0.3.32
   Compiling tap v1.0.1
   Compiling futures-io v0.3.32
   Compiling rdif-pinctrl v0.1.2 (/work/tgoskits/drivers/interface/rdif-pinctrl)
   Compiling arrayvec v0.7.7
   Compiling wyz v0.5.1
   Compiling embedded-hal v1.0.0
   Compiling rdrive v0.23.6 (/work/tgoskits/drivers/rdrive)
   Compiling funty v2.0.0
   Compiling const-default v1.0.0
   Compiling bytemuck v1.25.0
   Compiling riscv-types v0.1.0
   Compiling critical-section v1.2.0
   Compiling const-str v1.1.0
   Compiling num-align v0.1.0
   Compiling rgb v0.8.53
   Compiling futures v0.3.32
   Compiling riscv v0.16.1
   Compiling bitvec v1.1.1
   Compiling ax-plat v0.12.0 (/work/tgoskits/platforms/ax-plat)
   Compiling rdif-serial v0.9.0 (/work/tgoskits/drivers/interface/rdif-serial)
   Compiling rlsf v0.2.2
   Compiling ranges-ext v0.6.4 (/work/tgoskits/memory/ranges-ext)
   Compiling buddy-slab-allocator v0.4.6 (/work/tgoskits/memory/buddy-slab-allocator)
   Compiling spinning_top v0.2.5
   Compiling sbi-spec v0.0.9
   Compiling axpanic v0.1.1 (/work/tgoskits/components/axpanic)
   Compiling mbarrier v0.1.3
   Compiling utf8-width v0.1.8
   Compiling dma-api v0.9.3 (/work/tgoskits/memory/dma-api)
   Compiling byte-unit v5.2.5
   Compiling sbi-rt v0.0.4
   Compiling kernutil v0.2.2 (/work/tgoskits/components/kernutil)
   Compiling some-serial v0.7.0 (/work/tgoskits/drivers/serial/some-serial)
   Compiling ansi_rgb v0.2.0
   Compiling aml v0.16.4
   Compiling page-table-generic v0.7.4 (/work/tgoskits/memory/page-table-generic)
   Compiling ax-page-table-entry v0.8.10 (/work/tgoskits/memory/page_table_entry)
   Compiling uguid v2.2.1
   Compiling numeric-enum-macro v0.2.0
   Compiling axklib v0.7.5 (/work/tgoskits/components/axklib)
   Compiling axbacktrace v0.4.5 (/work/tgoskits/components/axbacktrace)
   Compiling ax-riscv-plic v0.4.9 (/work/tgoskits/drivers/intc/riscv_plic)
   Compiling riscv_goldfish v0.1.1
   Compiling ax-cpu v0.8.3 (/work/tgoskits/components/axcpu)
   Compiling ax-page-table-multiarch v0.8.13 (/work/tgoskits/memory/page_table_multiarch)
   Compiling fdt-parser v0.4.19
   Compiling ax-memory-set v0.6.12 (/work/tgoskits/memory/memory_set)
   Compiling ax-lazyinit v0.4.8 (/work/tgoskits/components/ax-lazyinit)
   Compiling ax-log v0.5.18 (/work/tgoskits/os/arceos/modules/axlog)
   Compiling chrono v0.4.45
   Compiling memchr v2.8.2
warning: function `set_fdt_addr_phys_if_valid` is never used
  --> platforms/someboot/src/fdt/mod.rs:32:15
   |
32 | pub(crate) fn set_fdt_addr_phys_if_valid(fdt_addr: usize) -> bool {
   |               ^^^^^^^^^^^^^^^^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

   Compiling ax-ctor-bare v0.4.8 (/work/tgoskits/components/ctor_bare/ctor_bare)
   Compiling linux-raw-sys v0.12.1
   Compiling bitmaps v3.2.1
   Compiling axpoll v0.5.1 (/work/tgoskits/components/axpoll)
   Compiling ax-io v0.6.1 (/work/tgoskits/components/axio)
warning: `someboot` (lib) generated 1 warning
   Compiling flatten_objects v0.2.4
   Compiling ax-mm v0.5.28 (/work/tgoskits/os/arceos/modules/axmm)
   Compiling ax-sync v0.5.28 (/work/tgoskits/os/arceos/modules/axsync)
   Compiling ax-api v0.7.4 (/work/tgoskits/os/arceos/api/arceos_api)
   Compiling ax-std v0.5.28 (/work/tgoskits/os/arceos/ulib/axstd)
   Compiling arceos-helloworld v0.1.0 (/work/tgoskits/apps/arceos/helloworld)
    Finished `release` profile [optimized] target(s) in 14m 01s
Converting ELF to BIN format...
  elf: /work/tgoskits/target/riscv64gc-unknown-linux-musl/release/arceos-helloworld
  bin: /work/tgoskits/target/riscv64gc-unknown-linux-musl/release/arceos-helloworld.bin
[K] hart[7] PID92 syscall 291 returned EFAULT
[K] hart[0] PID98 syscall 291 returned EFAULT
/root/.rustup/toolchains/nightly-2026-05-28-riscv64gc-unknown-linux-gnu/lib/rustlib/riscv64gc-unknown-linux-gnu/bin/llvm-objcopy --strip-all -O binary /work/tgoskits/target/riscv64gc-unknown-linux-musl/release/arceos-helloworld /work/tgoskits/target/riscv64gc-unknown-linux-musl/release/arceos-helloworld.bin
[axbuild] cargo build package=arceos-helloworld target=scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json config=/work/tgoskits/tmp/axbuild/config/arceos-helloworld/build-riscv64gc-unknown-none-elf.toml ... done (878.15s)
[axbuild] cargo build elf=/work/tgoskits/target/riscv64gc-unknown-linux-musl/release/arceos-helloworld
[axbuild] cargo build artifact_dir=/work/tgoskits/target/riscv64gc-unknown-linux-musl/release
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=885.91 cores=8 bytes=1681000 arch=riscv64
#### OS COMP TEST GROUP END buildstorm-glibc ####
[init] buildstorm_testcode.sh exited with status 0
[user] _start: main returned 0; exiting
[kernel] Idle process exit with exit_code 0 ...
[kernel] Panicked at src/process/task/exit.rs:42 All applications completed!
syncing disks...
shutting down...
