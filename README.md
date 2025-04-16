# hyprsunset-overdrive

A simple program to enable/disable the blue light filter on Hyprland based on sunrise and sunset in Nairobi, Kenya.

## Requirements

- `Hyprland`
- `hyprsunset`

## Building

To build the program, you will need the Rust toolchain installed.

```bash
cargo build --release
```

Move the executable to a bin folder in your path.

```bash
mv target/release/hyprsunset-overdrive ~/.local/bin
```

## Usage

Add the following to your Hyprland config file:

```
exec-once = ~/.local/bin/hyprsunset-overdrive
```

The program will automatically enable the blue light filter when the sun is above the horizon and disable it when the sun is below the horizon.

## Acknowledgments

This tool borrows some implementations from [sunsetr](https://github.com/psi4j/sunsetr). **sunsetr** is a great tool, as you can manually set the start and end times for the blue light filter.
