/* Flash layout, shared by the bootloader and the application. Swapping takes time for the
   whole partition, so the partitions are only as big as the firmware needs. */
MEMORY {
    BOOT2            : ORIGIN = 0x10000000, LENGTH = 0x100
    BOOTLOADER       : ORIGIN = 0x10000100, LENGTH = 24K - 0x100
    BOOTLOADER_STATE : ORIGIN = 0x10006000, LENGTH = 4K
    ACTIVE           : ORIGIN = 0x10007000, LENGTH = 128K
    DFU              : ORIGIN = 0x10027000, LENGTH = 132K
    RAM              : ORIGIN = 0x20000000, LENGTH = 256K
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(BOOT2);
__bootloader_active_end = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(BOOT2);
__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOT2);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOT2);

SECTIONS {
    .boot2 ORIGIN(BOOT2) : { KEEP(*(.boot2)); } > BOOT2
} INSERT BEFORE .text;
