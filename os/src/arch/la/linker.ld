# 本文件相比riscv版本仅OUTPUT_ARCH和BASE_ADDRESS不同,其他部分完全一样
OUTPUT_ARCH(loongarch)
ENTRY(_start)
# BASE_ADDRESS = 0x9000000000200000; # 偏移2MB留给启动和硬件等

# 用和rv一样的
BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;
    skernel = .;

    stext = .;
    .text : {
        *(.text.entry)
        . = ALIGN(16K);
        strampoline = .;
        *(.text.trampoline);
        . = ALIGN(16K);
        *(.text .text.*)
    }

    . = ALIGN(16K);
    etext = .;
    srodata = .;
    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    . = ALIGN(16K);
    erodata = .;
    sdata = .;
    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    . = ALIGN(16K);
    edata = .;
    sbss_with_stack = .;
    .bss : {
        *(.bss.stack)
        sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
    }

    . = ALIGN(16K);
    ebss = .;
    ekernel = .;

    /DISCARD/ : {
        *(.eh_frame)
    }
}