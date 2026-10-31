/* DIAGNOSTIC: the application alone, with no bootloader. Updates can't apply in this layout. */
MEMORY {
    BOOT2            : ORIGIN = 0x10000000, LENGTH = 0x100
    APP              : ORIGIN = 0x10000100, LENGTH = 512K - 0x100
    BOOTLOADER_STATE : ORIGIN = 0x10080000, LENGTH = 4K
    DFU              : ORIGIN = 0x10081000, LENGTH = 132K
    RAM              : ORIGIN = 0x20000000, LENGTH = 256K
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOT2);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOT2);

SECTIONS {
    .boot2 ORIGIN(BOOT2) : { KEEP(*(.boot2)); } > BOOT2
} INSERT BEFORE .text;
