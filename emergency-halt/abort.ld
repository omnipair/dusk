PHDRS
{
  halt_text PT_LOAD FLAGS(5);
}

SECTIONS
{
  . = SIZEOF_HEADERS;
  .text : ALIGN(8) { KEEP(*(.text)) } :halt_text

  /DISCARD/ : {
    *(.data*)
    *(.rodata*)
    *(.bss*)
    *(.eh_frame*)
    *(.gnu.hash*)
    *(.hash*)
    *(.comment)
    *(.symtab)
    *(.strtab)
    *(.dynsym)
    *(.dynstr)
    *(.dynamic)
  }
}

ENTRY(e)
