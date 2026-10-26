# Note
- this project copy and modified from embassy examples/stm32f4 folder 

# STM32 VS1053 MP3 Player

A Rust `no_std` embedded MP3 player controlled via a web interface. It runs on an STM32F446RE Nucleo board with a VS1053 audio codec shield and an ESP8266 Wi-Fi shield.

## Hardware Configuration

### 1. STM32F446RE Nucleo Board
- **Power Jumper (JP5)**: Set to **E5V**.
- **External Power**: Connect an external 5V power supply to the Morpho header **Pin 6 (E5V)** and **Pin 8 (GND)**.

### 2. VS1053 Audio Shield
- Stack directly onto the Nucleo board using the standard Arduino Uno headers.

### 3. ESP8266 Wi-Fi Shield
- **Pin Modifications**: The **RST**, **0**, and **1** pins on the shield must be **cut off** (or bent away) so they do not connect to the Nucleo board.
- **UART Wiring**:
  - Connect ESP8266 Debug Port **TX** ➔ Nucleo **PA1** (UART4_RX)
  - Connect ESP8266 Debug Port **RX** ➔ Nucleo **PA0** (UART4_TX)

## Features
- Hardware MP3 decoding via VS1053
- Reads `.MP3` files directly from the SD card
- Embedded HTTP web server via ESP8266 AT commands
- Responsive web interface with music timeline, volume control, and highlighted active tracks
- Playback modes: Sequential, Random
- Repeat modes: Normal, Repeat One, Repeat All

## Usage
1. Place your `.MP3` files into the root directory of a FAT16/FAT32 formatted SD card.
2. Insert the SD card into the VS1053 shield.
3. Flash the firmware using `cargo run --release`.
4. Connect to the Wi-Fi network and access the player's web interface via its IP address.

## Modify SSID and Password for your AP in main.rs
- const WIFI_SSID: &str = "G416C-2G";
- const WIFI_PASS: &str = "Happy12345";