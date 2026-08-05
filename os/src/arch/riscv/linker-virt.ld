#! 参考 linux 的实现，将内核链接到高半地址空间

OUTPUT_ARCH(riscv)
ENTRY(_start)

# 物理地址 0x80200000 映射到虚拟地址 0xffffffc080200000
BASE_ADDRESS = 0xffffffc080200000;
KERNEL_PHYS_LOAD = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;
    skernel = .;

    stext = .;
    # 内容加载到 0x80200000 起的物理地址
    # 后面的段会自动接续
    .text : AT(KERNEL_PHYS_LOAD) {
        *(.text.entry)
        . = ALIGN(4K);
        strampoline = .;
        *(.text.trampoline);
        . = ALIGN(4K);
        *(.text .text.*)
    }

    . = ALIGN(2M);
    etext = .;
    srodata = .;
    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    . = ALIGN(2M);
    erodata = .;
    sdata = .;
    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    . = ALIGN(2M);
    edata = .;
    sbss_with_stack = .;
    .bss : {
        *(.bss.stack)
        sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
    }

    . = ALIGN(2M);
    ebss = .;
    ekernel = .;

    /DISCARD/ : {
        *(.eh_frame)
    }
}
