# Pico IR receiver: signal test

A Raspberry Pi Pico with a bare 2-leg IR photodiode has no amplifier or carrier filter, so its
range has to be measured before the rest of the firmware is built on it. This bench build
prints the signal level over USB serial. It doesn't decode remotes yet. This hardware is model
IR1 (`ir_proto::Model::IR1`); a different receiver circuit needs its own model number and build.

## Wiring

| Part | Connects to |
|---|---|
| Photodiode cathode (usually the shorter leg; check its datasheet) | 3V3(OUT), pin 36 |
| Photodiode anode | GP26, pin 31 |
| 100 kΩ resistor | GP26, pin 31, to AGND, pin 33 |

Use a photodiode sensitive around 940 nm, ideally with a daylight filter (dark body, such as
BPW41N). A 2-leg phototransistor or IR LED looks similar but won't work reliably.

## Build and flash

    rustup target add thumbv6m-none-eabi
    (cd firmware/rp2040 && cargo build --release)
    tools/elf2uf2.py firmware/rp2040/target/thumbv6m-none-eabi/release/signal-test signal-test.uf2

Hold BOOTSEL while plugging in the Pico, then copy `signal-test.uf2` onto the `RPI-RP2` drive.

## Measure

Read the levels with `screen /dev/ttyACM0` (you may need the `dialout` group):

    peak=214 floor=3

Both are in ADC counts, four times a second. `peak` is the strongest 38 kHz carrier in that
quarter second and `floor` is the background. Hold a remote button and note both at 0.5, 1, 2
and 3 m, pointing at the diode and off to one side, with the room's usual lights on.

As a guide, reliable decoding is likely to need `peak` at least 4 times `floor`, and well above
single digits. This is an estimate to be confirmed when the decoder is built. If the room's
distances fall short, try 220 kΩ, then a transistor or op-amp stage before GP26.
