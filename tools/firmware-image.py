#!/usr/bin/env python3
"""Packs Pico ELF builds into the files that get installed.

    tools/firmware-image.py BOOTLOADER.elf APP.elf OUT_DIR

Writes OUT_DIR/cablebox-ir.uf2 (bootloader and application, for the first flash with BOOTSEL)
and OUT_DIR/cablebox-ir.bin (application only, for updates from CableBox).
"""
import os
import struct
import sys

PAGE = 256
FLASH_START = 0x10000000
FLASH_END = FLASH_START + (2 << 20)
STATE = 0x10006000  # from firmware/rp2040/layout.x
ACTIVE = 0x10007000
ACTIVE_END = ACTIVE + 128 * 1024
FAMILY_RP2040 = 0xE48BFF56


def flash_bytes(path):
    """Returns {address: byte} for everything the ELF loads into flash."""
    with open(path, "rb") as f:
        elf = f.read()
    if elf[:6] != b"\x7fELF\x01\x01":
        sys.exit(f"{path}: not a 32-bit little-endian ELF")
    phoff, = struct.unpack_from("<I", elf, 28)
    phentsize, phnum = struct.unpack_from("<HH", elf, 42)
    out = {}
    for i in range(phnum):
        kind, offset, _vaddr, paddr, filesz = struct.unpack_from("<5I", elf, phoff + i * phentsize)
        if kind == 1 and filesz and FLASH_START <= paddr < FLASH_END:  # PT_LOAD
            for n, byte in enumerate(elf[offset:offset + filesz]):
                out[paddr + n] = byte
    return out


def write_uf2(path, image):
    pages = {}
    for addr, byte in image.items():
        page = addr & ~(PAGE - 1)
        pages.setdefault(page, bytearray(b"\xff" * PAGE))[addr - page] = byte
    with open(path, "wb") as f:
        for n, (addr, data) in enumerate(sorted(pages.items())):
            head = struct.pack("<8I", 0x0A324655, 0x9E5D5157, 0x00002000, addr, PAGE, n, len(pages), FAMILY_RP2040)
            f.write(head + data + b"\0" * (476 - PAGE) + struct.pack("<I", 0x0AB16F30))


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    boot = {a: b for a, b in flash_bytes(sys.argv[1]).items() if a < ACTIVE}
    app = {a: b for a, b in flash_bytes(sys.argv[2]).items() if a >= ACTIVE}
    if FLASH_START not in boot:
        sys.exit("bootloader doesn't start at the beginning of flash; is boot2 linked in?")
    if ACTIVE not in app:
        sys.exit("application doesn't start at the active partition")
    if max(app) >= ACTIVE_END:
        sys.exit("application doesn't fit in the active partition")
    os.makedirs(sys.argv[3], exist_ok=True)
    # Blank update state: whatever a Pico held before could read as an update in progress.
    state = {a: 0xFF for a in range(STATE, ACTIVE)}
    write_uf2(os.path.join(sys.argv[3], "cablebox-ir.uf2"), {**boot, **state, **app})
    end = max(app) + 1
    with open(os.path.join(sys.argv[3], "cablebox-ir.bin"), "wb") as f:
        f.write(bytes(app.get(a, 0xFF) for a in range(ACTIVE, end)))
    print(f"bootloader {len(boot) // 1024} KiB, application {(end - ACTIVE) // 1024} KiB")


if __name__ == "__main__":
    main()
